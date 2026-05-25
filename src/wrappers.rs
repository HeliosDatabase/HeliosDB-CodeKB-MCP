//! Layer 2 — plugin-side wrapper tools.
//!
//! These wrap engine library APIs into single distilled responses.
//! The agent calls one wrapper instead of three engine primitives;
//! the wire payload shrinks because (a) we project only the columns
//! the agent needs, (b) we cap result counts at agent-sensible
//! defaults, (c) we fold related queries (definition → references →
//! call hierarchy) into one structured card.
//!
//! Wrappers compose engine library calls and run inside the plugin
//! process; they don't fork out to MCP tools. See CLAUDE.md
//! "Composition wrappers may live here".
//!
//! Surfaced to the agent by `inject_into_tools_list` (called from the
//! stdio loop / HTTP gateway after the engine's native `tools/list`
//! response). Dispatched from the same loops via `dispatch`, which
//! short-circuits past `handle_rpc_with_db` for plugin tool names.

use anyhow::Result;
use heliosdb_nano::EmbeddedDatabase;
use serde_json::{json, Value as JsonValue};

use crate::mcp_trim::{wrapper_tool_names, Profile};

/// Static descriptor for a plugin-side wrapper tool. The `handler`
/// is a fn-pointer so the descriptor table is itself `const`.
pub struct ToolDesc {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: fn() -> JsonValue,
    pub handler: fn(&EmbeddedDatabase, &JsonValue) -> Result<JsonValue, String>,
}

// ---------------------------------------------------------------------------
// Public registration / dispatch entry points
// ---------------------------------------------------------------------------

/// `true` when `name` is owned by the plugin (not the engine).
/// Currently called from tests and external consumers; the main
/// transport loops use `dispatch().is_some()` as the equivalent
/// check, so this helper is `#[allow(dead_code)]` for binary builds.
#[allow(dead_code)]
pub fn is_plugin_tool(name: &str) -> bool {
    wrapper_tool_names().contains(&name)
}

/// Append the plugin's wrapper tools to the engine's native
/// `tools/list` JSON-RPC response. Profile-filtered: tools dropped
/// from `Profile::allows(name)` are skipped here too, so the merge
/// stays consistent with the subsequent `trim_tools_list_wire` pass.
///
/// Idempotent: if a plugin tool name is already present in the engine
/// response (defensive — engine release adds an upstream version),
/// the existing entry wins.
pub fn inject_into_tools_list(json_line: &str, profile: Profile) -> String {
    let mut parsed: JsonValue = match serde_json::from_str(json_line) {
        Ok(v) => v,
        Err(_) => return json_line.to_string(),
    };
    let Some(tools) = parsed
        .get_mut("result")
        .and_then(|r| r.get_mut("tools"))
        .and_then(|t| t.as_array_mut())
    else {
        return json_line.to_string();
    };

    let already: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();

    for tool in PLUGIN_TOOLS {
        if !profile.allows(tool.name) {
            continue;
        }
        if already.contains(tool.name) {
            continue;
        }
        tools.push(json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": (tool.input_schema)(),
        }));
    }
    serde_json::to_string(&parsed).unwrap_or_else(|_| json_line.to_string())
}

/// Dispatch a `tools/call` to a plugin handler if `name` is plugin-
/// owned. Returns:
///
/// * `None` — `name` is an engine tool; the caller falls through to
///   `handle_rpc_with_db`.
/// * `Some(Ok(value))` — handler succeeded; `value` is the inner
///   JSON to wrap in an MCP `tools/call` result envelope.
/// * `Some(Err(msg))` — handler failed; the caller emits a JSON-RPC
///   error response.
pub fn dispatch(
    db: &EmbeddedDatabase,
    name: &str,
    args: &JsonValue,
) -> Option<Result<JsonValue, String>> {
    let tool = PLUGIN_TOOLS.iter().find(|t| t.name == name)?;
    Some((tool.handler)(db, args))
}

// ---------------------------------------------------------------------------
// Mega-tool — one helios(action, args) tool that covers every wrapper
// ---------------------------------------------------------------------------

/// Name of the mega-tool exposed when `--mega-tool` is active.
pub const MEGA_TOOL_NAME: &str = "helios";

/// Schema for the mega-tool. Deliberately tiny — the per-action
/// schemas live in `helios(action="list_actions")` so the tools/list
/// payload stays under ~500 bytes regardless of how many sub-actions
/// the plugin and engine expose.
pub fn mega_tool_descriptor() -> JsonValue {
    json!({
        "name": MEGA_TOOL_NAME,
        "description": "One-tool gateway to the heliosdb-codekb MCP. \
            Call with {action: \"<name>\", args: {...}}. Available actions: \
            repo_summary, outline_first, doc_drill, semantic_filter, git_summary, \
            symbol_card (plugin wrappers); plus passthrough to any engine tool by \
            short name (e.g. action=\"lsp_definition\", \"graphrag_search\", \
            \"query\", \"ast_diff\"). Use action=\"list_actions\" to fetch the \
            full per-action arg schema catalogue on demand.",
        "inputSchema": {
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Sub-action name. See list_actions for schemas."
                },
                "args": {
                    "type": "object",
                    "description": "Action arguments (action-specific)."
                }
            }
        }
    })
}

/// Translate a plugin wrapper or engine tool name into the
/// equivalent `tools/call` it represents. Returns:
///
/// * Plugin wrapper: short name → its full plugin-tool name (the
///   plugin handler is then invoked via `dispatch`).
/// * Engine tool: short name → its full engine-tool name (`heliosdb_*`
///   or `helios_*`). Caller routes to `handle_rpc_with_db`.
///
/// Short-name conventions:
/// * Plugin wrappers drop the `helios_` prefix (`repo_summary`,
///   `symbol_card`, …).
/// * Engine tools also drop their prefix where unambiguous
///   (`query` → `heliosdb_query`, `lsp_definition` →
///   `helios_lsp_definition`, `graphrag_search` →
///   `helios_graphrag_search`, `ast_diff` → `helios_ast_diff`, etc.).
pub fn resolve_action_name(action: &str) -> Option<&'static str> {
    match action {
        // Plugin wrappers (drop the `helios_` prefix).
        "repo_summary" => Some("helios_repo_summary"),
        "outline_first" => Some("helios_outline_first"),
        "doc_drill" => Some("helios_doc_drill"),
        "semantic_filter" => Some("helios_semantic_filter"),
        "git_summary" => Some("helios_git_summary"),
        "symbol_card" => Some("helios_symbol_card"),
        // Engine read-only tools we want exposed under the mega-tool.
        "query" => Some("heliosdb_query"),
        "hybrid_search" => Some("heliosdb_hybrid_search"),
        "graphrag_search" => Some("helios_graphrag_search"),
        "lsp_definition" => Some("helios_lsp_definition"),
        "lsp_references" => Some("helios_lsp_references"),
        "lsp_call_hierarchy" => Some("helios_lsp_call_hierarchy"),
        "lsp_hover" => Some("helios_lsp_hover"),
        "lsp_document_symbols" => Some("helios_lsp_document_symbols"),
        "ast_diff" => Some("helios_ast_diff"),
        "list_actions" => Some("__list_actions"),
        _ => None,
    }
}

/// Build the per-action schema catalogue returned by
/// `helios(action="list_actions")`. One JSON object per action, with
/// name + 1-line description + minimal input schema. Total payload
/// ~3 KB — fetched once on demand, NOT re-cached every turn.
pub fn list_actions_payload() -> JsonValue {
    let mut entries = Vec::with_capacity(PLUGIN_TOOLS.len() + 9);
    for tool in PLUGIN_TOOLS {
        let short = tool.name.trim_start_matches("helios_");
        entries.push(json!({
            "action": short,
            "description": tool.description,
            "input_schema": (tool.input_schema)(),
        }));
    }
    // Engine tools we route to. Schemas are intentionally NOT
    // included here — the agent can call action="list_actions" once,
    // then for any engine-action call, the engine will validate the
    // args at dispatch time.
    let engine_actions = &[
        ("query", "Read-only SQL query against the embedded engine. args: {sql: string}"),
        ("hybrid_search", "BM25 + HNSW fusion over a user table. args: {table, query, k?}"),
        ("graphrag_search", "Seed-text BFS expand. args: {seed_text, hops?, limit?, seed_kinds?, edge_kinds?}"),
        ("lsp_definition", "Symbol definition lookup. args: {name, hint_file?, hint_kind?}"),
        ("lsp_references", "All references to a symbol. args: {symbol_id}"),
        ("lsp_call_hierarchy", "Caller/callee traversal. args: {symbol_id, direction, depth}"),
        ("lsp_hover", "Signature + docstring. args: {symbol_id}"),
        ("lsp_document_symbols", "Symbols in one file. args: {path}"),
        ("ast_diff", "AST-level diff between branches/commits. args: {file, at_a, at_b}"),
    ];
    for (a, d) in engine_actions {
        entries.push(json!({"action": a, "description": d, "input_schema": "see engine tool"}));
    }
    json!({"actions": entries})
}

/// Dispatch a mega-tool call. Returns `Some(plugin_result)` when the
/// action is handled in-plugin or `None` when the caller should route
/// to the engine via `handle_rpc_with_db`. The caller is responsible
/// for re-wrapping the engine call to use the resolved tool name (see
/// `resolve_action_name`).
pub fn dispatch_mega(
    db: &EmbeddedDatabase,
    action: &str,
    args: &JsonValue,
) -> Option<Result<JsonValue, String>> {
    if action == "list_actions" {
        return Some(Ok(list_actions_payload()));
    }
    let resolved = resolve_action_name(action)?;
    // Plugin actions → dispatch in-process.
    if resolved.starts_with("helios_") && is_plugin_tool(resolved) {
        return dispatch(db, resolved, args);
    }
    // Engine action — caller routes via handle_rpc_with_db with the
    // resolved name. Signal that by returning None here AFTER the
    // caller has already short-circuited on `MEGA_TOOL_NAME`.
    None
}

/// Wrap a plugin-handler value into the MCP `tools/call` result
/// envelope shape: `{"content":[{"type":"text","text": "<json>"}],
/// "isError": false}`. Used by both transports so the agent sees
/// engine and plugin responses in the same shape.
pub fn wrap_call_result(inner: JsonValue) -> JsonValue {
    let text = serde_json::to_string(&inner).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

/// Same envelope, but marks `isError: true` so MCP clients
/// surface the message to the user verbatim.
pub fn wrap_call_error(message: impl Into<String>) -> JsonValue {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

// ---------------------------------------------------------------------------
// Tool descriptor table
// ---------------------------------------------------------------------------

/// All plugin wrapper tools. The order matches `wrapper_tool_names()`
/// in `mcp_trim.rs` so the two stay in lockstep when tests assert the
/// set of plugin tools.
pub const PLUGIN_TOOLS: &[ToolDesc] = &[
    ToolDesc {
        name: "helios_repo_summary",
        description: "PageRank-ranked file index w/ top symbols.",
        input_schema: repo_summary_schema,
        handler: repo_summary_handler,
    },
    ToolDesc {
        name: "helios_outline_first",
        description: "Doc headings + 1-line summaries (no chunk bodies).",
        input_schema: outline_first_schema,
        handler: outline_first_handler,
    },
    ToolDesc {
        name: "helios_doc_drill",
        description: "Expand one DocSection into its child chunks.",
        input_schema: doc_drill_schema,
        handler: doc_drill_handler,
    },
    ToolDesc {
        name: "helios_semantic_filter",
        description: "Filtered KNN: semantic + lang/kind/path predicates.",
        input_schema: semantic_filter_schema,
        handler: semantic_filter_handler,
    },
    ToolDesc {
        name: "helios_git_summary",
        description: "Structural diff (added/removed/moved/sig-changed) between two commits.",
        input_schema: git_summary_schema,
        handler: git_summary_handler,
    },
    ToolDesc {
        name: "helios_symbol_card",
        description: "Symbol card: sig + doc + ≤5 callers + ≤5 callees in one call.",
        input_schema: symbol_card_schema,
        handler: symbol_card_handler,
    },
];

// ---------------------------------------------------------------------------
// Schema builders (kept as fns so the table can stay `const`)
// ---------------------------------------------------------------------------

fn repo_summary_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "detail": {
                "type": "string",
                "enum": ["minimal", "file_index", "symbol_index"],
                "default": "file_index",
                "description": "Card density. minimal: just file paths + pagerank. file_index: + top symbols per file. symbol_index: + signature snippets."
            },
            "limit": {
                "type": "integer",
                "default": 50,
                "minimum": 1,
                "maximum": 500,
                "description": "Max files to return (by pagerank)."
            }
        }
    })
}

fn outline_first_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "description": "Seed text matched against DocSection titles + text." },
            "max_sections": { "type": "integer", "default": 20, "minimum": 1, "maximum": 100 }
        }
    })
}

fn doc_drill_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["section_id"],
        "properties": {
            "section_id": { "type": "integer", "description": "node_id of the DocSection (from helios_outline_first)." },
            "max_chunks": { "type": "integer", "default": 10, "minimum": 1, "maximum": 50 }
        }
    })
}

fn semantic_filter_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string" },
            "k": { "type": "integer", "default": 5, "minimum": 1, "maximum": 50 },
            "where_lang": { "type": "string", "description": "Filter by symbol language (rust, python, …)." },
            "where_kind": { "type": "string", "description": "Filter by symbol kind (function, struct, …)." },
            "where_path_glob": { "type": "string", "description": "Filter by file-path glob (e.g. src/storage/%)." }
        }
    })
}

fn git_summary_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["commit_a", "commit_b"],
        "properties": {
            "commit_a": { "type": "string", "description": "Base commit SHA." },
            "commit_b": { "type": "string", "description": "Head commit SHA." },
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional file-path filter. Empty = diff the whole tree."
            }
        }
    })
}

fn symbol_card_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["qualified_name"],
        "properties": {
            "qualified_name": {
                "type": "string",
                "description": "Symbol name. Bare name (`new`) or qualified (`MyType::new`); the engine resolves."
            },
            "max_callers": { "type": "integer", "default": 5, "minimum": 0, "maximum": 50 },
            "max_callees": { "type": "integer", "default": 5, "minimum": 0, "maximum": 50 }
        }
    })
}

// ---------------------------------------------------------------------------
// Small JSON / Value helpers
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a JsonValue, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing required string argument `{key}`"))
}

fn arg_str_opt<'a>(args: &'a JsonValue, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn arg_int(args: &JsonValue, key: &str, default: i64) -> i64 {
    args.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
}

fn arg_int_required(args: &JsonValue, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("missing required integer argument `{key}`"))
}

/// Extract a string column from a Tuple row at `idx`. Returns "" for
/// NULL or non-string types — handler-level errors aren't a great
/// UX here; we'd rather emit a card with a missing field than fail
/// the whole call.
fn tuple_str(row: &heliosdb_nano::Tuple, idx: usize) -> String {
    match row.get(idx) {
        Some(heliosdb_nano::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn tuple_f64(row: &heliosdb_nano::Tuple, idx: usize) -> f64 {
    match row.get(idx) {
        Some(heliosdb_nano::Value::Float8(f)) => *f,
        Some(heliosdb_nano::Value::Float4(f)) => *f as f64,
        _ => 0.0,
    }
}

/// SQL string literal escape — single-quote replacement only.
fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `true` when the table exists in the current KB. Used by handlers
/// that depend on Layer 3 cards to short-circuit with a friendly
/// message until distill has run.
fn table_exists(db: &EmbeddedDatabase, table: &str) -> bool {
    let sql = format!(
        "SELECT 1 FROM information_schema.tables WHERE table_name = {}",
        sql_lit(table)
    );
    matches!(db.query(&sql, &[]), Ok(rows) if !rows.is_empty())
}

// ---------------------------------------------------------------------------
// Handlers — `helios_repo_summary`
// ---------------------------------------------------------------------------

fn repo_summary_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    let detail = arg_str_opt(args, "detail").unwrap_or("file_index");
    let limit = arg_int(args, "limit", 50).clamp(1, 500);

    if !table_exists(db, "_hdb_plugin_repomap_cards") {
        return Ok(json!({
            "status": "cards_not_built",
            "message": "_hdb_plugin_repomap_cards not present. Re-run `heliosdb-codekb-mcp ingest --source <root>` after Layer 3 (distill) lands to populate the table.",
            "files": []
        }));
    }

    let sql = format!(
        "SELECT path, pagerank, top_symbols FROM _hdb_plugin_repomap_cards \
         ORDER BY pagerank DESC LIMIT {limit}"
    );
    let rows = db.query(&sql, &[]).map_err(|e| format!("query failed: {e}"))?;

    let mut files = Vec::with_capacity(rows.len());
    for row in &rows {
        let path = tuple_str(row, 0);
        let pagerank = tuple_f64(row, 1);
        let top_symbols_raw = tuple_str(row, 2);

        match detail {
            "minimal" => files.push(json!({ "path": path, "pagerank": pagerank })),
            "file_index" => {
                let top: JsonValue = serde_json::from_str(&top_symbols_raw)
                    .unwrap_or(JsonValue::Array(vec![]));
                let trimmed = match top {
                    JsonValue::Array(arr) => arr
                        .into_iter()
                        .map(|s| match s {
                            JsonValue::Object(obj) => {
                                let name = obj.get("name").cloned().unwrap_or_default();
                                json!({ "name": name })
                            }
                            other => other,
                        })
                        .collect::<Vec<_>>(),
                    _ => vec![],
                };
                files.push(json!({
                    "path": path,
                    "pagerank": pagerank,
                    "top_symbols": trimmed,
                }));
            }
            _ => {
                // symbol_index: pass the cards through as stored
                // (signature + doc1l per symbol).
                let top: JsonValue = serde_json::from_str(&top_symbols_raw)
                    .unwrap_or(JsonValue::Array(vec![]));
                files.push(json!({
                    "path": path,
                    "pagerank": pagerank,
                    "top_symbols": top,
                }));
            }
        }
    }

    Ok(json!({ "detail": detail, "count": files.len(), "files": files }))
}

// ---------------------------------------------------------------------------
// Handlers — `helios_outline_first` / `helios_doc_drill`
// ---------------------------------------------------------------------------

fn outline_first_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    use heliosdb_nano::graph_rag::{Direction, GraphRagOptions};

    let query = arg_str(args, "query")?;
    let max_sections = arg_int(args, "max_sections", 20).clamp(1, 100) as usize;

    let opts = GraphRagOptions {
        seed_text: query.to_string(),
        seed_kinds: vec!["DocSection".to_string()],
        hops: 0,
        edge_kinds: Vec::new(),
        direction: Direction::Both,
        limit: max_sections,
    };
    let hits = db
        .graph_rag_search(&opts)
        .map_err(|e| format!("graph_rag_search failed: {e}"))?;

    let sections: Vec<JsonValue> = hits
        .into_iter()
        .map(|h| {
            // First-line summary from `text` (truncate at first newline,
            // cap 200 chars). Keeps payload tiny — agents drill with
            // helios_doc_drill when they want the body.
            let summary = h
                .text
                .as_deref()
                .map(|t| {
                    let head = t.split('\n').next().unwrap_or("");
                    head.chars().take(200).collect::<String>()
                })
                .unwrap_or_default();
            json!({
                "section_id": h.node_id,
                "title": h.title,
                "summary": summary,
                "source": h.source_ref,
            })
        })
        .collect();

    Ok(json!({ "query": query, "count": sections.len(), "sections": sections }))
}

fn doc_drill_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    let section_id = arg_int_required(args, "section_id")?;
    let max_chunks = arg_int(args, "max_chunks", 10).clamp(1, 50);

    // Pull the section's title for the seed, then BFS one hop along
    // PART_OF to its child DocChunks. The engine's `graph_rag_search`
    // takes a text seed — use the section title we just looked up.
    let title_sql = format!(
        "SELECT title FROM _hdb_graph_nodes WHERE node_id = {section_id} AND node_kind = 'DocSection'"
    );
    let title_rows = db
        .query(&title_sql, &[])
        .map_err(|e| format!("section lookup failed: {e}"))?;
    let title = title_rows
        .first()
        .map(|r| tuple_str(r, 0))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("no DocSection node with id {section_id}"))?;

    use heliosdb_nano::graph_rag::{Direction, GraphRagOptions};
    let opts = GraphRagOptions {
        seed_text: title.clone(),
        seed_kinds: vec!["DocSection".to_string()],
        hops: 1,
        edge_kinds: vec!["PART_OF".to_string()],
        direction: Direction::Out,
        limit: max_chunks as usize + 1, // +1 for the seed itself
    };
    let hits = db
        .graph_rag_search(&opts)
        .map_err(|e| format!("graph_rag_search failed: {e}"))?;

    let chunks: Vec<JsonValue> = hits
        .into_iter()
        .filter(|h| h.node_kind == "DocChunk")
        .take(max_chunks as usize)
        .map(|h| {
            json!({
                "chunk_id": h.node_id,
                "text": h.text,
                "source": h.source_ref,
            })
        })
        .collect();

    Ok(json!({
        "section_id": section_id,
        "title": title,
        "count": chunks.len(),
        "chunks": chunks,
    }))
}

// ---------------------------------------------------------------------------
// Handlers — `helios_semantic_filter` (gated on vector-persist)
// ---------------------------------------------------------------------------

#[cfg(feature = "wrappers-semantic")]
fn semantic_filter_handler(_db: &EmbeddedDatabase, _args: &JsonValue) -> Result<JsonValue, String> {
    // Placeholder until the engine's `vector-persist` feature publishes
    // the stable `PersistentVectorIndex::open(db, "code_symbols")`
    // path. Adoption tracked in NANO_PERSISTENT_PQ_HNSW_ADOPTION.md.
    Err("helios_semantic_filter: not yet wired — pending engine release of \
         `vector-persist` on crates.io. Track via NANO_PERSISTENT_PQ_HNSW_ADOPTION.md."
        .to_string())
}

#[cfg(not(feature = "wrappers-semantic"))]
fn semantic_filter_handler(_db: &EmbeddedDatabase, _args: &JsonValue) -> Result<JsonValue, String> {
    Err("helios_semantic_filter: compile with `--features wrappers-semantic` to enable. \
         Requires engine feature `vector-persist`."
        .to_string())
}

// ---------------------------------------------------------------------------
// Handlers — `helios_git_summary`
// ---------------------------------------------------------------------------

fn git_summary_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    use heliosdb_nano::code_graph::AsOfRef;

    let commit_a = arg_str(args, "commit_a")?;
    let commit_b = arg_str(args, "commit_b")?;
    let at_a = AsOfRef::commit(commit_a);
    let at_b = AsOfRef::commit(commit_b);

    // Path filter: explicit list, or fall back to the whole tree
    // (one diff per changed file). For the whole-tree case we
    // intersect with files that have any symbol activity between A
    // and B — pulling the union of file_path from _hdb_code_files
    // would over-shoot for a typical PR.
    let paths: Vec<String> = match args.get("paths") {
        Some(JsonValue::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };

    let targets: Vec<String> = if paths.is_empty() {
        // Whole-tree mode: enumerate file paths in the current KB.
        // For a "git diff"-style summary the agent usually knows
        // which paths to ask about; whole-tree is an escape hatch.
        let rows = db
            .query("SELECT path FROM _hdb_code_files ORDER BY path", &[])
            .map_err(|e| format!("file list failed: {e}"))?;
        rows.iter().map(|r| tuple_str(r, 0)).collect()
    } else {
        paths
    };

    let mut added: Vec<JsonValue> = Vec::new();
    let mut removed: Vec<JsonValue> = Vec::new();
    let mut moved: Vec<JsonValue> = Vec::new();
    let signature_changed: Vec<JsonValue> = Vec::new();
    let mut errors_seen = 0u32;

    for path in &targets {
        let diff = match db.ast_diff(path, &at_a, &at_b) {
            Ok(d) => d,
            Err(_) => {
                errors_seen += 1;
                continue;
            }
        };
        for row in diff {
            use heliosdb_nano::code_graph::DiffChange;
            let bucket = match row.change {
                DiffChange::Added => &mut added,
                DiffChange::Removed => &mut removed,
                DiffChange::Moved => &mut moved,
            };
            bucket.push(json!({
                "path": path,
                "kind": row.kind,
                "qualified": row.qualified,
                "line_a": row.line_a,
                "line_b": row.line_b,
            }));
        }

        // Note: `signature_changed` is reserved for the Phase 2
        // body-diff aggregation pass — Layer 2 ships with ast_diff
        // alone (Added / Removed / Moved). Body-diff per moved symbol
        // would add ~one round-trip per row; defer until the bench
        // shows it useful.
    }

    Ok(json!({
        "commit_a": commit_a,
        "commit_b": commit_b,
        "scanned_paths": targets.len(),
        "skipped_paths": errors_seen,
        "added": added,
        "removed": removed,
        "moved": moved,
        "signature_changed": signature_changed,
    }))
}

// ---------------------------------------------------------------------------
// Handlers — `helios_symbol_card`
// ---------------------------------------------------------------------------

fn symbol_card_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    use heliosdb_nano::code_graph::{lsp::CallDirection, DefinitionHint};

    let qualified = arg_str(args, "qualified_name")?;
    let max_callers = arg_int(args, "max_callers", 5).clamp(0, 50) as usize;
    let max_callees = arg_int(args, "max_callees", 5).clamp(0, 50) as usize;

    // `lsp_definition` matches on bare `name`; accept either bare or
    // `Type::method` shapes by taking the last `::` segment.
    let bare = qualified
        .rsplit("::")
        .next()
        .unwrap_or(qualified)
        .to_string();
    let hint = DefinitionHint::default();
    // An empty KB (pre-ingest) doesn't yet have the `_hdb_code_*`
    // tables, so lsp_definition errors with "Table does not exist".
    // That's a "not built" condition, not a tool failure — surface it
    // as the same not_found shape the agent already handles.
    let defs = match db.lsp_definition(&bare, &hint) {
        Ok(d) => d,
        Err(_) => {
            return Ok(json!({
                "status": "not_found",
                "query": qualified,
                "message": "code-graph tables not yet built — run `heliosdb-codekb-mcp ingest --source <root>` first."
            }));
        }
    };
    let Some(def) = defs.into_iter().next() else {
        return Ok(json!({
            "status": "not_found",
            "query": qualified,
            "message": "no definition found — try a qualified name or check the symbol exists in the indexed KB."
        }));
    };

    // Distilled doc1l + Phase-2 LLM summary from Layer 3 if present;
    // engine `lsp_hover` is the fallback for symbols whose cards
    // haven't been built yet.
    let mut doc1l = String::new();
    let mut llm_summary = String::new();
    if table_exists(db, "_hdb_plugin_symbol_cards") {
        // Parameterized — the engine's plain `query` path panics on
        // multibyte UTF-8 inside the SQL literal (em-dash in a doc
        // heading triggered `with_context.rs:125:43`). Tolerate the
        // llm_summary column being absent on legacy KBs by retrying
        // without it.
        let qual = heliosdb_nano::Value::String(def.qualified.clone());
        let rows = db
            .query_params(
                "SELECT doc1l, llm_summary FROM _hdb_plugin_symbol_cards WHERE qualified = $1",
                &[qual.clone()],
            )
            .or_else(|_| {
                db.query_params(
                    "SELECT doc1l FROM _hdb_plugin_symbol_cards WHERE qualified = $1",
                    &[qual],
                )
            });
        if let Ok(rs) = rows {
            if let Some(r) = rs.first() {
                doc1l = tuple_str(r, 0);
                llm_summary = tuple_str(r, 1);
            }
        }
    }
    if doc1l.is_empty() && llm_summary.is_empty() {
        if let Ok(Some(h)) = db.lsp_hover(def.symbol_id) {
            doc1l = h
                .doc
                .as_deref()
                .map(|d| d.split('\n').next().unwrap_or(d).chars().take(160).collect())
                .unwrap_or_default();
        }
    }

    let mut callers: Vec<JsonValue> = Vec::new();
    if max_callers > 0 {
        if let Ok(refs) = db.lsp_references(def.symbol_id) {
            for r in refs.into_iter().take(max_callers) {
                callers.push(json!({
                    "path": r.path,
                    "line": r.line,
                    "caller_symbol_id": r.caller_symbol_id,
                }));
            }
        }
    }
    let mut callees: Vec<JsonValue> = Vec::new();
    if max_callees > 0 {
        if let Ok(rows) = db.lsp_call_hierarchy(def.symbol_id, CallDirection::Outgoing, 1) {
            for r in rows.into_iter().take(max_callees) {
                callees.push(json!({
                    "qualified": r.qualified,
                    "path": r.path,
                    "line": r.line,
                }));
            }
        }
    }

    Ok(json!({
        "qualified": def.qualified,
        "signature": def.signature,
        "doc1l": doc1l,
        "llm_summary": llm_summary,
        "file": def.path,
        "line": def.line,
        "callers": callers,
        "callees": callees,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_tools_match_mcp_trim_wrapper_names() {
        let here: std::collections::HashSet<&str> =
            PLUGIN_TOOLS.iter().map(|t| t.name).collect();
        let other: std::collections::HashSet<&str> =
            wrapper_tool_names().iter().copied().collect();
        assert_eq!(
            here, other,
            "PLUGIN_TOOLS and mcp_trim::wrapper_tool_names() must list the same names"
        );
    }

    #[test]
    fn each_tool_has_valid_schema() {
        for t in PLUGIN_TOOLS {
            let s = (t.input_schema)();
            assert_eq!(s["type"], "object", "{}: schema type must be object", t.name);
            assert!(
                s.get("properties").is_some(),
                "{}: schema needs `properties`",
                t.name
            );
        }
    }

    #[test]
    fn inject_appends_plugin_tools() {
        let engine_resp = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"heliosdb_query","description":"sql","inputSchema":{"type":"object"}}]}}"#;
        let merged = inject_into_tools_list(engine_resp, Profile::Standard);
        let v: JsonValue = serde_json::from_str(&merged).unwrap();
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"heliosdb_query"));
        for w in wrapper_tool_names() {
            assert!(names.contains(w), "missing wrapper {w}");
        }
    }

    #[test]
    fn inject_is_idempotent() {
        let engine_resp = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let once = inject_into_tools_list(engine_resp, Profile::Standard);
        let twice = inject_into_tools_list(&once, Profile::Standard);
        let v: JsonValue = serde_json::from_str(&twice).unwrap();
        let count = v["result"]["tools"].as_array().unwrap().len();
        assert_eq!(
            count,
            wrapper_tool_names().len(),
            "double-injection should not duplicate plugin tools"
        );
    }

    #[test]
    fn inject_skips_when_not_tools_list_shape() {
        let other = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[]}}"#;
        let out = inject_into_tools_list(other, Profile::Standard);
        assert_eq!(out, other, "non-tools/list shapes must pass through");
    }

    #[test]
    fn dispatch_returns_none_for_engine_tool() {
        // Open a tempdir KB just to give dispatch() a live db handle.
        let td = tempfile::tempdir().unwrap();
        let db = EmbeddedDatabase::new(td.path()).unwrap();
        let r = dispatch(&db, "heliosdb_query", &json!({"sql": "SELECT 1"}));
        assert!(r.is_none(), "engine tool name must not match plugin dispatch");
    }

    #[test]
    fn dispatch_routes_symbol_card_call() {
        let td = tempfile::tempdir().unwrap();
        let db = EmbeddedDatabase::new(td.path()).unwrap();
        let r = dispatch(&db, "helios_symbol_card", &json!({"qualified_name": "nonexistent"}));
        assert!(r.is_some(), "plugin name must be dispatched");
        let payload = r.unwrap().unwrap();
        // Empty KB → not_found status, but no Rust panic.
        assert_eq!(payload["status"], "not_found");
    }

    #[test]
    fn wrap_envelopes() {
        let ok = wrap_call_result(json!({"a": 1}));
        assert_eq!(ok["isError"], false);
        assert!(ok["content"][0]["text"].as_str().unwrap().contains("\"a\":1"));
        let err = wrap_call_error("bang");
        assert_eq!(err["isError"], true);
        assert_eq!(err["content"][0]["text"], "bang");
    }
}
