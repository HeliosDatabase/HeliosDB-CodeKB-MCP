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
    // Cache lookup — skip when capacity is 0 (the default). Repeats
    // within a serve session short-circuit; cache evicts LRU when
    // full. The `key` mixes tool name + canonical-JSON arg payload,
    // so different field= selections / detail= levels each get their
    // own slot.
    if let Some(cached) = cache_get(name, args) {
        return Some(Ok(cached));
    }
    let result = (tool.handler)(db, args);
    if let Ok(ref v) = result {
        cache_put(name, args, v);
    }
    Some(result)
}

// ---------------------------------------------------------------------------
// Per-process LRU result cache (Tier A #4)
// ---------------------------------------------------------------------------

/// Tiny LRU keyed on `(tool_name, args_hash)`. Wrapper handlers are
/// pure functions of their args — within a single serve session, the
/// same `(name, args)` always returns the same value (the KB doesn't
/// mutate during a serve), so we can short-circuit repeat lookups.
///
/// Capacity is set via `--wrapper-cache-size N` on the CLI (0 = off).
/// Implementation is a `HashMap` + `VecDeque` over the FNV-1a hash of
/// the canonicalised arg-JSON. Skip-when-size-0 keeps the perf hit to
/// "one hash + one lock check" when the feature is off.
pub struct WrapperCache {
    cap: usize,
    map: std::collections::HashMap<u64, serde_json::Value>,
    order: std::collections::VecDeque<u64>,
    pub hits: u64,
    pub misses: u64,
}

impl WrapperCache {
    fn new() -> Self {
        Self {
            cap: 0,
            map: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            hits: 0,
            misses: 0,
        }
    }

    fn key(name: &str, args: &JsonValue) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in name.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        // canonicalise the args via serde_json's stable serialiser.
        let canon = serde_json::to_string(args).unwrap_or_default();
        for b in canon.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

static WRAPPER_CACHE: std::sync::OnceLock<std::sync::Mutex<WrapperCache>> =
    std::sync::OnceLock::new();

fn cache() -> &'static std::sync::Mutex<WrapperCache> {
    WRAPPER_CACHE.get_or_init(|| std::sync::Mutex::new(WrapperCache::new()))
}

/// Set the cache capacity (0 = disabled). Called once during `serve`
/// startup; later calls reset and resize.
pub fn set_cache_capacity(cap: usize) {
    let mut g = match cache().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    g.cap = cap;
    g.map.clear();
    g.order.clear();
    g.hits = 0;
    g.misses = 0;
}

fn cache_get(name: &str, args: &JsonValue) -> Option<JsonValue> {
    let mut g = cache().lock().ok()?;
    if g.cap == 0 {
        return None;
    }
    let k = WrapperCache::key(name, args);
    if let Some(v) = g.map.get(&k).cloned() {
        // Bump to back (most recently used). Linear scan; cap is
        // expected to be small (≤256), so this is cheap.
        if let Some(pos) = g.order.iter().position(|&x| x == k) {
            g.order.remove(pos);
        }
        g.order.push_back(k);
        g.hits += 1;
        return Some(v);
    }
    g.misses += 1;
    None
}

fn cache_put(name: &str, args: &JsonValue, v: &JsonValue) {
    let mut g = match cache().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if g.cap == 0 {
        return;
    }
    let k = WrapperCache::key(name, args);
    if !g.map.contains_key(&k) {
        while g.order.len() >= g.cap {
            if let Some(old) = g.order.pop_front() {
                g.map.remove(&old);
            } else {
                break;
            }
        }
        g.order.push_back(k);
    }
    g.map.insert(k, v.clone());
}

/// Hits + misses for telemetry / status surfaces.
#[allow(dead_code)]
pub fn cache_stats() -> (u64, u64, usize, usize) {
    let g = match cache().lock() {
        Ok(g) => g,
        Err(_) => return (0, 0, 0, 0),
    };
    (g.hits, g.misses, g.map.len(), g.cap)
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
    let plugin_actions = plugin_action_names().join(", ");
    json!({
        "name": MEGA_TOOL_NAME,
        "description": format!("One-tool gateway to the heliosdb-codekb MCP. \
            Call with {{action: \"<name>\", args: {{...}}}}. Available plugin actions: \
            {plugin_actions}; plus passthrough to any engine tool by \
            short name (e.g. action=\"lsp_definition\", \"graphrag_search\", \
            \"query\", \"ast_diff\"). Use action=\"list_actions\" to fetch the \
            full per-action arg schema catalogue on demand."),
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

fn plugin_action_names() -> Vec<&'static str> {
    PLUGIN_TOOLS
        .iter()
        .map(|tool| tool.name.trim_start_matches("helios_"))
        .collect()
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
        "ask" => Some("helios_ask"),
        "repo_summary" => Some("helios_repo_summary"),
        "outline_first" => Some("helios_outline_first"),
        "doc_drill" => Some("helios_doc_drill"),
        #[cfg(feature = "wrappers-semantic")]
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
        (
            "query",
            "Read-only SQL query against the embedded engine. args: {sql: string}",
        ),
        (
            "hybrid_search",
            "BM25 + HNSW fusion over a user table. args: {table, query, k?}",
        ),
        (
            "graphrag_search",
            "Seed-text BFS expand. args: {seed_text, hops?, limit?, seed_kinds?, edge_kinds?}",
        ),
        (
            "lsp_definition",
            "Symbol definition lookup. args: {name, hint_file?, hint_kind?}",
        ),
        (
            "lsp_references",
            "All references to a symbol. args: {symbol_id}",
        ),
        (
            "lsp_call_hierarchy",
            "Caller/callee traversal. args: {symbol_id, direction, depth}",
        ),
        ("lsp_hover", "Signature + docstring. args: {symbol_id}"),
        ("lsp_document_symbols", "Symbols in one file. args: {path}"),
        (
            "ast_diff",
            "AST-level diff between branches/commits. args: {file, at_a, at_b}",
        ),
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
        name: "helios_ask",
        description: "Question router: returns a compact answer-card with evidence.",
        input_schema: ask_schema,
        handler: ask_handler,
    },
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
    #[cfg(feature = "wrappers-semantic")]
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

fn ask_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["question"],
        "properties": {
            "question": {
                "type": "string",
                "description": "Natural-language repository question to route through the smallest applicable CodeKB wrapper."
            },
            "mode": {
                "type": "string",
                "enum": ["answer", "audit", "edit", "navigate"],
                "default": "answer",
                "description": "Intent hint. answer returns compact evidence; edit favors exact paths/lines; audit favors broader coverage."
            },
            "budget_tokens": {
                "type": "integer",
                "default": 1500,
                "minimum": 200,
                "maximum": 8000,
                "description": "Approximate response budget. The server returns snippets/evidence inside this budget instead of arbitrary byte truncation."
            }
        }
    })
}

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
            },
            "fields": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional projection on per-file cards. Choices: path, pagerank, top_symbols. Omit for all."
            },
            "budget_tokens": {
                "type": "integer",
                "default": 1200,
                "minimum": 200,
                "maximum": 8000,
                "description": "Approximate response budget for the answer_card/evidence."
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
            "max_sections": { "type": "integer", "default": 20, "minimum": 1, "maximum": 100 },
            "budget_tokens": { "type": "integer", "default": 1200, "minimum": 200, "maximum": 8000 }
        }
    })
}

fn doc_drill_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["section_id"],
        "properties": {
            "section_id": { "type": "integer", "description": "node_id of the DocSection (from helios_outline_first)." },
            "max_chunks": { "type": "integer", "default": 10, "minimum": 1, "maximum": 50 },
            "budget_tokens": { "type": "integer", "default": 1500, "minimum": 200, "maximum": 12000 }
        }
    })
}

#[cfg(feature = "wrappers-semantic")]
fn semantic_filter_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string" },
            "k": { "type": "integer", "default": 5, "minimum": 1, "maximum": 50 },
            "where_lang": { "type": "string", "description": "Filter by symbol language (rust, python, …)." },
            "where_kind": { "type": "string", "description": "Filter by symbol kind (function, struct, …)." },
            "where_path_glob": { "type": "string", "description": "Filter by file-path glob (e.g. src/storage/%)." },
            "budget_tokens": { "type": "integer", "default": 1200, "minimum": 200, "maximum": 8000 }
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
            },
            "budget_tokens": { "type": "integer", "default": 1500, "minimum": 200, "maximum": 12000 }
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
            "max_callees": { "type": "integer", "default": 5, "minimum": 0, "maximum": 50 },
            "fields": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional projection — when set, only these fields are returned. Choices: qualified, signature, doc1l, llm_summary, file, line, callers, callees. Omit for all fields."
            },
            "budget_tokens": { "type": "integer", "default": 1200, "minimum": 200, "maximum": 8000 }
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

/// Read an optional `fields=["foo","bar"]` array. Returns `None` if
/// absent or empty (caller emits all fields); otherwise a HashSet of
/// the names to keep.
fn arg_field_set(args: &JsonValue) -> Option<std::collections::HashSet<String>> {
    let arr = args.get("fields")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let set: std::collections::HashSet<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// Project a JSON object to the named subset. When `fields` is None
/// the input passes through unchanged. Unknown field names in the
/// set are simply absent from the output.
fn project(v: JsonValue, fields: Option<&std::collections::HashSet<String>>) -> JsonValue {
    let Some(fields) = fields else {
        return v;
    };
    let Some(obj) = v.as_object() else {
        return v;
    };
    let mut out = serde_json::Map::with_capacity(fields.len());
    for (k, val) in obj {
        if fields.contains(k) {
            out.insert(k.clone(), val.clone());
        }
    }
    JsonValue::Object(out)
}

fn arg_budget_tokens(args: &JsonValue, default: usize) -> usize {
    arg_int(args, "budget_tokens", default as i64).clamp(200, 12_000) as usize
}

fn budget_bytes(args: &JsonValue, default_tokens: usize) -> usize {
    arg_budget_tokens(args, default_tokens).saturating_mul(4)
}

fn shorten_chars(s: &str, max_bytes: usize) -> (String, usize) {
    if s.len() <= max_bytes {
        return (s.to_string(), 0);
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (s[..cut].to_string(), s.len().saturating_sub(cut))
}

fn attach_answer_card(
    mut payload: JsonValue,
    kind: &str,
    summary: impl Into<String>,
    evidence: Vec<JsonValue>,
    args: &JsonValue,
    default_budget_tokens: usize,
    omitted: Vec<JsonValue>,
) -> JsonValue {
    let budget = arg_budget_tokens(args, default_budget_tokens);
    let card = json!({
        "kind": kind,
        "summary": summary.into(),
        "budget_tokens": budget,
        "evidence": evidence,
        "omitted": omitted,
    });
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "schema".to_string(),
            JsonValue::String("helios.answer_card.v1".to_string()),
        );
        obj.insert("answer_card".to_string(), card);
    }
    payload
}

fn first_backtick_term(question: &str) -> Option<String> {
    let mut parts = question.split('`');
    let _before = parts.next()?;
    let term = parts.next()?.trim();
    if term.is_empty() {
        None
    } else {
        Some(term.to_string())
    }
}

fn likely_symbol_term(question: &str) -> Option<String> {
    if let Some(term) = first_backtick_term(question) {
        return Some(term);
    }
    question
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|s| !s.is_empty())
        .find(|s| {
            s.contains("::")
                || s.chars().any(|c| c == '_')
                || s.chars().any(|c| c.is_ascii_uppercase())
        })
        .map(str::to_string)
}

fn looks_like_architecture_question(q: &str) -> bool {
    let q = q.to_ascii_lowercase();
    [
        "architecture",
        "overview",
        "layout",
        "modules",
        "structure",
        "where does this codebase",
    ]
    .iter()
    .any(|needle| q.contains(needle))
}

fn looks_like_doc_question(q: &str) -> bool {
    let q = q.to_ascii_lowercase();
    [
        "doc",
        "docs",
        "readme",
        "according",
        "guide",
        "explain",
        "how does",
    ]
    .iter()
    .any(|needle| q.contains(needle))
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

fn tuple_int(row: &heliosdb_nano::Tuple, idx: usize) -> Option<i64> {
    match row.get(idx) {
        Some(heliosdb_nano::Value::Int8(i)) => Some(*i),
        Some(heliosdb_nano::Value::Int4(i)) => Some(*i as i64),
        Some(heliosdb_nano::Value::Int2(i)) => Some(*i as i64),
        _ => None,
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
// Handlers — `helios_ask`
// ---------------------------------------------------------------------------

fn ask_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    let question = arg_str(args, "question")?;
    let mode = arg_str_opt(args, "mode").unwrap_or("answer");
    let budget = arg_budget_tokens(args, 1500);

    let (route, routed_args, result) = if looks_like_architecture_question(question) {
        let routed_args = json!({
            "detail": "file_index",
            "limit": 20,
            "budget_tokens": budget,
        });
        let result = repo_summary_handler(db, &routed_args)?;
        ("repo_summary", routed_args, result)
    } else if let Some(symbol) = likely_symbol_term(question) {
        let routed_args = json!({
            "qualified_name": symbol,
            "max_callers": if mode == "audit" { 8 } else { 3 },
            "max_callees": if mode == "audit" { 8 } else { 3 },
            "budget_tokens": budget,
        });
        let result = symbol_card_handler(db, &routed_args)?;
        if result.get("status").and_then(|v| v.as_str()) == Some("not_found")
            && looks_like_doc_question(question)
        {
            let fallback_args = json!({
                "query": question,
                "max_sections": 8,
                "budget_tokens": budget,
            });
            let fallback = outline_first_handler(db, &fallback_args)?;
            ("outline_first", fallback_args, fallback)
        } else {
            ("symbol_card", routed_args, result)
        }
    } else {
        let routed_args = json!({
            "query": question,
            "max_sections": if mode == "audit" { 12 } else { 8 },
            "budget_tokens": budget,
        });
        let result = outline_first_handler(db, &routed_args)?;
        ("outline_first", routed_args, result)
    };

    let evidence = result
        .get("answer_card")
        .and_then(|c| c.get("evidence"))
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let summary = result
        .get("answer_card")
        .and_then(|c| c.get("summary"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Routed question through {route}."));

    let payload = json!({
        "question": question,
        "mode": mode,
        "route": route,
        "routed_args": routed_args,
        "result": result,
    });
    Ok(attach_answer_card(
        payload,
        "ask",
        summary,
        evidence,
        args,
        1500,
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------
// Handlers — `helios_repo_summary`
// ---------------------------------------------------------------------------

fn repo_summary_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    let detail = arg_str_opt(args, "detail").unwrap_or("file_index");
    let limit = arg_int(args, "limit", 50).clamp(1, 500);
    let fields = arg_field_set(args);

    if !table_exists(db, "_hdb_plugin_repomap_cards") {
        let payload = json!({
            "status": "cards_not_built",
            "message": "_hdb_plugin_repomap_cards not present. Re-run `heliosdb-codekb-mcp ingest --source <root>` after Layer 3 (distill) lands to populate the table.",
            "files": []
        });
        return Ok(attach_answer_card(
            payload,
            "repo_summary",
            "RepoMap cards are not built yet; run ingest to populate compact file summaries.",
            Vec::new(),
            args,
            1200,
            Vec::new(),
        ));
    }

    let sql = format!(
        "SELECT path, pagerank, top_symbols FROM _hdb_plugin_repomap_cards \
         ORDER BY pagerank DESC LIMIT {limit}"
    );
    let rows = db
        .query(&sql, &[])
        .map_err(|e| format!("query failed: {e}"))?;

    let mut files = Vec::with_capacity(rows.len());
    for row in &rows {
        let path = tuple_str(row, 0);
        let pagerank = tuple_f64(row, 1);
        let top_symbols_raw = tuple_str(row, 2);

        let full = match detail {
            "minimal" => json!({ "path": path, "pagerank": pagerank }),
            "file_index" => {
                let top: JsonValue =
                    serde_json::from_str(&top_symbols_raw).unwrap_or(JsonValue::Array(vec![]));
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
                json!({
                    "path": path,
                    "pagerank": pagerank,
                    "top_symbols": trimmed,
                })
            }
            _ => {
                // symbol_index: pass the cards through as stored
                // (signature + doc1l per symbol).
                let top: JsonValue =
                    serde_json::from_str(&top_symbols_raw).unwrap_or(JsonValue::Array(vec![]));
                json!({
                    "path": path,
                    "pagerank": pagerank,
                    "top_symbols": top,
                })
            }
        };
        files.push(project(full, fields.as_ref()));
    }

    let evidence = files
        .iter()
        .take(8)
        .filter_map(|f| {
            Some(json!({
                "path": f.get("path")?.clone(),
                "pagerank": f.get("pagerank").cloned().unwrap_or(JsonValue::Null),
            }))
        })
        .collect::<Vec<_>>();
    let payload = json!({ "detail": detail, "count": files.len(), "files": files });
    Ok(attach_answer_card(
        payload,
        "repo_summary",
        format!(
            "Returned {} PageRank-ranked file cards at detail={detail}.",
            rows.len()
        ),
        evidence,
        args,
        1200,
        Vec::new(),
    ))
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

    let evidence = sections
        .iter()
        .take(8)
        .filter_map(|s| {
            Some(json!({
                "section_id": s.get("section_id")?.clone(),
                "title": s.get("title").cloned().unwrap_or(JsonValue::Null),
                "source": s.get("source").cloned().unwrap_or(JsonValue::Null),
            }))
        })
        .collect::<Vec<_>>();
    let payload = json!({ "query": query, "count": sections.len(), "sections": sections });
    Ok(attach_answer_card(
        payload,
        "outline_first",
        format!(
            "Returned {} matching documentation sections without full chunk bodies.",
            evidence.len()
        ),
        evidence,
        args,
        1200,
        Vec::new(),
    ))
}

fn doc_drill_handler(db: &EmbeddedDatabase, args: &JsonValue) -> Result<JsonValue, String> {
    let section_id = arg_int_required(args, "section_id")?;
    let max_chunks = arg_int(args, "max_chunks", 10).clamp(1, 50);

    // Directly expand the requested DocSection id. The older
    // implementation re-searched by section title, which could return
    // the wrong chunks when headings repeated and paid an unnecessary
    // retrieval round trip.
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

    let chunk_sql = format!(
        "SELECT n.node_id, n.text, n.source_ref \
         FROM _hdb_graph_edges e \
         JOIN _hdb_graph_nodes n ON n.node_id = e.from_node \
         WHERE e.to_node = {section_id} \
           AND e.edge_kind = 'PART_OF' \
           AND n.node_kind = 'DocChunk' \
         ORDER BY n.node_id \
         LIMIT {max_chunks}"
    );
    let rows = db
        .query(&chunk_sql, &[])
        .map_err(|e| format!("chunk lookup failed: {e}"))?;

    let mut omitted = Vec::new();
    let mut remaining = budget_bytes(args, 1500);
    let mut chunks: Vec<JsonValue> = Vec::with_capacity(rows.len());
    for row in &rows {
        let chunk_id = tuple_int(row, 0).unwrap_or_default();
        let text = tuple_str(row, 1);
        let source = tuple_str(row, 2);
        let (text, dropped) = if remaining > 0 {
            let cap = remaining.min(text.len());
            let (out, dropped) = shorten_chars(&text, cap);
            remaining = remaining.saturating_sub(out.len());
            (out, dropped)
        } else {
            (String::new(), text.len())
        };
        if dropped > 0 {
            omitted.push(json!({
                "reason": "budget",
                "chunk_id": chunk_id,
                "bytes": dropped,
                "open_with": { "action": "doc_drill", "args": { "section_id": section_id, "budget_tokens": 12000 } }
            }));
        }
        chunks.push(json!({
            "chunk_id": chunk_id,
            "text": text,
            "source": source,
        }));
    }
    let evidence = chunks
        .iter()
        .take(8)
        .filter_map(|c| {
            Some(json!({
                "chunk_id": c.get("chunk_id")?.clone(),
                "source": c.get("source").cloned().unwrap_or(JsonValue::Null),
            }))
        })
        .collect::<Vec<_>>();

    let payload = json!({
        "section_id": section_id,
        "title": title,
        "count": chunks.len(),
        "chunks": chunks,
    });
    Ok(attach_answer_card(
        payload,
        "doc_drill",
        format!(
            "Expanded DocSection {section_id} into {} child chunks.",
            rows.len()
        ),
        evidence,
        args,
        1500,
        omitted,
    ))
}

// ---------------------------------------------------------------------------
// Handlers — `helios_semantic_filter` (gated on vector-persist)
// ---------------------------------------------------------------------------

#[cfg(feature = "wrappers-semantic")]
fn semantic_filter_handler(_db: &EmbeddedDatabase, _args: &JsonValue) -> Result<JsonValue, String> {
    // Placeholder until the engine's `vector-persist` feature publishes
    // the stable `PersistentVectorIndex::open(db, "code_symbols")`
    // path. Adoption tracked in NANO_PERSISTENT_PQ_HNSW_ADOPTION.md.
    Err(
        "helios_semantic_filter: not yet wired — pending engine release of \
         `vector-persist` on crates.io. Track via NANO_PERSISTENT_PQ_HNSW_ADOPTION.md."
            .to_string(),
    )
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

    let evidence = added
        .iter()
        .chain(removed.iter())
        .chain(moved.iter())
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let payload = json!({
        "commit_a": commit_a,
        "commit_b": commit_b,
        "scanned_paths": targets.len(),
        "skipped_paths": errors_seen,
        "added": added,
        "removed": removed,
        "moved": moved,
        "signature_changed": signature_changed,
    });
    Ok(attach_answer_card(
        payload,
        "git_summary",
        format!(
            "Scanned {} paths and returned structural AST changes.",
            targets.len()
        ),
        evidence,
        args,
        1500,
        Vec::new(),
    ))
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
            let payload = json!({
                "status": "not_found",
                "query": qualified,
                "message": "code-graph tables not yet built — run `heliosdb-codekb-mcp ingest --source <root>` first."
            });
            return Ok(attach_answer_card(
                payload,
                "symbol_card",
                "Code graph tables are not built yet; run ingest before symbol lookup.",
                Vec::new(),
                args,
                1200,
                Vec::new(),
            ));
        }
    };
    let Some(def) = defs.into_iter().next() else {
        let payload = json!({
            "status": "not_found",
            "query": qualified,
            "message": "no definition found — try a qualified name or check the symbol exists in the indexed KB."
        });
        return Ok(attach_answer_card(
            payload,
            "symbol_card",
            format!("No definition found for `{qualified}`."),
            Vec::new(),
            args,
            1200,
            Vec::new(),
        ));
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
                .map(|d| {
                    d.split('\n')
                        .next()
                        .unwrap_or(d)
                        .chars()
                        .take(160)
                        .collect()
                })
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

    let full = json!({
        "qualified": def.qualified,
        "signature": def.signature,
        "doc1l": doc1l,
        "llm_summary": llm_summary,
        "file": def.path,
        "line": def.line,
        "callers": callers,
        "callees": callees,
    });
    let projected = project(full, arg_field_set(args).as_ref());
    let evidence = vec![json!({
        "qualified": projected.get("qualified").cloned().unwrap_or(JsonValue::Null),
        "path": projected.get("file").cloned().unwrap_or(JsonValue::Null),
        "line": projected.get("line").cloned().unwrap_or(JsonValue::Null),
    })];
    let summary = projected
        .get("llm_summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            projected
                .get("doc1l")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| {
            let q = projected
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(qualified);
            format!("Returned compact symbol card for `{q}`.")
        });
    Ok(attach_answer_card(
        projected,
        "symbol_card",
        summary,
        evidence,
        args,
        1200,
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_tools_match_mcp_trim_wrapper_names() {
        let here: std::collections::HashSet<&str> = PLUGIN_TOOLS.iter().map(|t| t.name).collect();
        let other: std::collections::HashSet<&str> = wrapper_tool_names().iter().copied().collect();
        assert_eq!(
            here, other,
            "PLUGIN_TOOLS and mcp_trim::wrapper_tool_names() must list the same names"
        );
    }

    #[test]
    fn each_tool_has_valid_schema() {
        for t in PLUGIN_TOOLS {
            let s = (t.input_schema)();
            assert_eq!(
                s["type"], "object",
                "{}: schema type must be object",
                t.name
            );
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
        assert!(
            r.is_none(),
            "engine tool name must not match plugin dispatch"
        );
    }

    #[test]
    fn dispatch_routes_symbol_card_call() {
        let td = tempfile::tempdir().unwrap();
        let db = EmbeddedDatabase::new(td.path()).unwrap();
        let r = dispatch(
            &db,
            "helios_symbol_card",
            &json!({"qualified_name": "nonexistent"}),
        );
        assert!(r.is_some(), "plugin name must be dispatched");
        let payload = r.unwrap().unwrap();
        // Empty KB → not_found status, but no Rust panic.
        assert_eq!(payload["status"], "not_found");
    }

    #[test]
    fn wrap_envelopes() {
        let ok = wrap_call_result(json!({"a": 1}));
        assert_eq!(ok["isError"], false);
        assert!(ok["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"a\":1"));
        let err = wrap_call_error("bang");
        assert_eq!(err["isError"], true);
        assert_eq!(err["content"][0]["text"], "bang");
    }
}
