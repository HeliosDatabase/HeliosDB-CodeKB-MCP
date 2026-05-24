//! Plugin-side trimming + tool-surface gateway for MCP responses.
//!
//! Two concerns live here:
//!
//! 1. **Tool-result trimming** (`trim_rpc_response_wire`) — caps every
//!    string in a `tools/call` `result` payload at N bytes. The engine
//!    can't know the agent's context budget; we know we're shipping
//!    to Claude Code, which charges per token. Truncation marker tells
//!    the agent how much was dropped so it can ask for the rest.
//!
//! 2. **Tool-surface compression** (`trim_tools_list_wire`) — rewrites
//!    `tools/list` responses to drop tools outside the active
//!    `Profile`'s allow list and shorten or strip `description`
//!    fields per `StripDescMode`. This is the Layer 1 lever in
//!    `bench/README.md`'s "Phase 1 layer ablation" section — the
//!    `tools/list` payload was the dominant per-turn cache cost.
//!
//! Both functions are best-effort: if JSON parsing fails the input
//! passes through unchanged. The MCP protocol must never break on a
//! gateway hiccup.
//!
//! Char-boundary safe (won't panic on multi-byte UTF-8 — same lesson
//! as the linker emoji bug we shipped a fix for in `src/linker.rs`).

use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// Profiles: which tools are advertised to the agent
// ---------------------------------------------------------------------------

/// Profile chooses the allow list applied to `tools/list` responses.
/// The dispatch path is unchanged: every `tools/call` still reaches
/// `heliosdb_nano::mcp::rpc::handle_rpc_with_db`. Profile only filters
/// what the agent *sees* and therefore caches per turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Agent sees only the plugin's distilled wrappers + `heliosdb_query`.
    /// Smallest tool-surface; targets the bench's "broad-scan" workload
    /// where the wrappers cover the question shape.
    Minimal,
    /// Wrappers + a curated subset of engine LSP / search / SQL tools.
    /// Default; tested to keep all bench questions answerable.
    Standard,
    /// Pass-through — no filtering. Use when integrating tools we
    /// haven't yet vetted, or when debugging.
    Full,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "minimal" => Ok(Profile::Minimal),
            "standard" => Ok(Profile::Standard),
            "full" => Ok(Profile::Full),
            other => Err(format!(
                "invalid profile `{other}` (expected minimal|standard|full)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Minimal => "minimal",
            Profile::Standard => "standard",
            Profile::Full => "full",
        }
    }

    /// `true` ⇒ keep this tool in the advertised list. `Profile::Full`
    /// always returns true (no filtering).
    pub fn allows(self, tool_name: &str) -> bool {
        match self {
            Profile::Full => true,
            Profile::Minimal => MINIMAL_ALLOW.contains(&tool_name),
            Profile::Standard => STANDARD_ALLOW.contains(&tool_name),
        }
    }
}

/// Plugin-owned wrapper tool names (Layer 2). Source of truth for the
/// consistency check in `wrappers::tests::plugin_tools_match_mcp_trim_wrapper_names`.
/// Kept here (rather than re-exported from `wrappers.rs`) so the
/// `Profile` allow lists at the top of this file can reference the
/// names without pulling in `wrappers` at module-init time.
#[allow(dead_code)]
const WRAPPER_TOOLS: &[&str] = &[
    "helios_repo_summary",
    "helios_outline_first",
    "helios_doc_drill",
    "helios_semantic_filter",
    "helios_git_summary",
    "helios_symbol_card",
];

/// Minimal profile: wrappers + `heliosdb_query` (escape hatch for
/// custom SQL the wrappers don't cover).
const MINIMAL_ALLOW: &[&str] = &[
    "helios_repo_summary",
    "helios_outline_first",
    "helios_doc_drill",
    "helios_semantic_filter",
    "helios_symbol_card",
    "heliosdb_query",
];

/// Standard profile: wrappers + curated engine tools that cover every
/// canonical bench question. Notable omissions: `heliosdb_create_table`,
/// `heliosdb_insert`, the `heliosdb_branch_*` mutation tools, and the
/// `helios_lsp_rename_*` writers — agents don't write to the KB.
const STANDARD_ALLOW: &[&str] = &[
    // wrappers
    "helios_repo_summary",
    "helios_outline_first",
    "helios_doc_drill",
    "helios_semantic_filter",
    "helios_git_summary",
    "helios_symbol_card",
    // read-only engine surface
    "heliosdb_query",
    "heliosdb_hybrid_search",
    "helios_graphrag_search",
    "helios_lsp_definition",
    "helios_lsp_references",
    "helios_ast_diff",
];

/// Names of plugin wrapper tools — consumed by the consistency test
/// in `wrappers` and by external callers that want the canonical list
/// without depending on the `wrappers` module. The main dispatch path
/// detects plugin tools via `wrappers::dispatch().is_some()` instead.
#[allow(dead_code)]
pub fn wrapper_tool_names() -> &'static [&'static str] {
    WRAPPER_TOOLS
}

// ---------------------------------------------------------------------------
// Description stripping
// ---------------------------------------------------------------------------

/// How much of each tool's `description` to keep in the advertised
/// `tools/list` payload. Schemas (`inputSchema`) are always kept —
/// they're how the agent picks an arg shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripDescMode {
    /// Pass `description` through unchanged.
    None,
    /// Truncate `description` at the nearest char boundary ≤ N bytes,
    /// no marker (saves the 24-byte `[+N bytes truncated]` overhead —
    /// the agent doesn't need to ask for the rest of a tool name).
    ShortenTo(usize),
    /// Drop `description` entirely (empty string). Violates strict
    /// MCP spec — some clients require non-empty descriptions. Opt-in
    /// only.
    All,
}

impl StripDescMode {
    /// Parse a CLI value: integer → `ShortenTo`, `"none"` → `None`,
    /// `"all"` → `All`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(StripDescMode::None),
            "all" => Ok(StripDescMode::All),
            other => other
                .parse::<usize>()
                .map(StripDescMode::ShortenTo)
                .map_err(|_| {
                    format!("invalid --strip-tool-descriptions `{other}` (expected int|none|all)")
                }),
        }
    }
}

// ---------------------------------------------------------------------------
// Trimming primitives
// ---------------------------------------------------------------------------

/// Walk `v` in place. For every `String` longer than `max_bytes`,
/// truncate at the nearest char boundary ≤ max_bytes and append a
/// `…[+N bytes truncated]` marker. `Array` / `Object` children are
/// recursed.
pub fn trim_value(v: &mut JsonValue, max_bytes: usize) {
    match v {
        JsonValue::String(s) => {
            if s.len() <= max_bytes {
                return;
            }
            let mut cut = max_bytes;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            let dropped = s.len() - cut;
            let head: String = s[..cut].to_string();
            *s = format!("{head}…[+{dropped} bytes truncated]");
        }
        JsonValue::Array(arr) => {
            for el in arr.iter_mut() {
                trim_value(el, max_bytes);
            }
        }
        JsonValue::Object(obj) => {
            for (_k, child) in obj.iter_mut() {
                trim_value(child, max_bytes);
            }
        }
        _ => {}
    }
}

/// Char-boundary-safe shorten without the truncation marker.
/// Used by the description stripper — tool descriptions don't need
/// the "ask for the rest" affordance (the agent picks tools by name,
/// not by exact prose).
fn shorten_for_desc(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

// ---------------------------------------------------------------------------
// Wire-level rewrites
// ---------------------------------------------------------------------------

/// Convenience: parse a JSON-RPC response, trim its `result` payload,
/// re-serialise. Used by the stdio loop so the trim happens between
/// the engine's `handle_rpc_with_db` and the byte write to stdout.
///
/// Returns the trimmed wire form. On parse failure returns the input
/// unchanged (trimming is best-effort; never block the protocol).
pub fn trim_rpc_response_wire(json_line: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || max_bytes == usize::MAX {
        return json_line.to_string();
    }
    let mut parsed: JsonValue = match serde_json::from_str(json_line) {
        Ok(v) => v,
        Err(_) => return json_line.to_string(),
    };
    if let Some(result) = parsed.get_mut("result") {
        trim_value(result, max_bytes);
    }
    serde_json::to_string(&parsed).unwrap_or_else(|_| json_line.to_string())
}

/// Rewrite a `tools/list` JSON-RPC response: drop tools outside the
/// profile's allow list, shorten or drop `description` fields per
/// `StripDescMode`. `inputSchema` is always preserved.
///
/// Best-effort: parse failure ⇒ pass-through.
pub fn trim_tools_list_wire(
    json_line: &str,
    profile: Profile,
    strip: StripDescMode,
) -> String {
    // Full + None = no-op; bail before paying the parse cost.
    if profile == Profile::Full && matches!(strip, StripDescMode::None) {
        return json_line.to_string();
    }
    let mut parsed: JsonValue = match serde_json::from_str(json_line) {
        Ok(v) => v,
        Err(_) => return json_line.to_string(),
    };

    let tools_array = parsed
        .get_mut("result")
        .and_then(|r| r.get_mut("tools"))
        .and_then(|t| t.as_array_mut());
    let Some(tools) = tools_array else {
        // Not a tools/list response shape (or an error response) —
        // pass through.
        return json_line.to_string();
    };

    // Filter by profile allow list. Retain tools whose name is
    // allowed; drop the rest entirely.
    tools.retain(|t| {
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
        profile.allows(name)
    });

    // Apply description stripping in place.
    for tool in tools.iter_mut() {
        if let Some(obj) = tool.as_object_mut() {
            match strip {
                StripDescMode::None => {}
                StripDescMode::ShortenTo(n) => {
                    if let Some(desc) = obj.get_mut("description") {
                        if let Some(s) = desc.as_str() {
                            *desc = JsonValue::String(shorten_for_desc(s, n));
                        }
                    }
                }
                StripDescMode::All => {
                    obj.insert("description".to_string(), JsonValue::String(String::new()));
                }
            }
        }
    }

    serde_json::to_string(&parsed).unwrap_or_else(|_| json_line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- result-body trim (existing behavior) ----

    #[test]
    fn short_strings_pass_through() {
        let mut v = json!({ "a": "hi", "b": ["ok", "fine"] });
        trim_value(&mut v, 100);
        assert_eq!(v, json!({ "a": "hi", "b": ["ok", "fine"] }));
    }

    #[test]
    fn long_string_truncated_with_marker() {
        let body = "x".repeat(5000);
        let mut v = json!({ "body": body });
        trim_value(&mut v, 100);
        let trimmed = v["body"].as_str().unwrap();
        assert!(trimmed.starts_with(&"x".repeat(100)));
        assert!(trimmed.contains("[+4900 bytes truncated]"));
    }

    #[test]
    fn nested_array_of_objects_recursed() {
        let body = "y".repeat(3000);
        let mut v = json!({ "hits": [ { "text": body.clone() }, { "text": body.clone() } ] });
        trim_value(&mut v, 200);
        for hit in v["hits"].as_array().unwrap() {
            let t = hit["text"].as_str().unwrap();
            assert!(t.len() < body.len());
            assert!(t.contains("[+2800 bytes truncated]"));
        }
    }

    #[test]
    fn multibyte_safe() {
        let s = "💰💰💰💰💰".to_string();
        let mut v = json!({ "x": s.clone() });
        trim_value(&mut v, 9);
        let out = v["x"].as_str().unwrap();
        assert!(out.starts_with("💰💰"));
        assert!(out.contains("[+"));
    }

    #[test]
    fn wire_form_skips_non_result_methods() {
        let req = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"helios_x","description":"AAAAA"}]}}"#;
        let out = trim_rpc_response_wire(req, 3);
        let parsed: JsonValue = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        let desc = parsed["result"]["tools"][0]["description"].as_str().unwrap();
        assert!(desc.starts_with("AAA"));
        assert!(desc.contains("[+"));
    }

    #[test]
    fn no_trim_when_max_bytes_is_zero_or_max() {
        let req = r#"{"jsonrpc":"2.0","id":1,"result":{"x":"hellohello"}}"#;
        assert_eq!(trim_rpc_response_wire(req, 0), req);
        assert_eq!(trim_rpc_response_wire(req, usize::MAX), req);
    }

    // ---- profile parsing ----

    #[test]
    fn profile_parse_round_trip() {
        for s in ["minimal", "standard", "full"] {
            assert_eq!(Profile::parse(s).unwrap().as_str(), s);
        }
        assert!(Profile::parse("bogus").is_err());
    }

    #[test]
    fn profile_allow_lists_are_self_consistent() {
        // Standard + Full advertise every plugin wrapper. Minimal
        // deliberately drops `helios_git_summary` (heavy: AST-diffs
        // the whole tree) — confirm only the intended subset.
        let minimal_keeps: &[&str] = &[
            "helios_repo_summary",
            "helios_outline_first",
            "helios_doc_drill",
            "helios_semantic_filter",
            "helios_symbol_card",
        ];
        for w in WRAPPER_TOOLS {
            assert!(Profile::Standard.allows(w), "Standard should allow {w}");
            assert!(Profile::Full.allows(w), "Full should allow {w}");
            if minimal_keeps.contains(w) {
                assert!(Profile::Minimal.allows(w), "Minimal should allow {w}");
            } else {
                assert!(
                    !Profile::Minimal.allows(w),
                    "Minimal should NOT allow {w} (heavy / out-of-scope for minimal profile)"
                );
            }
        }
        // Full allows anything, even unknown names.
        assert!(Profile::Full.allows("totally_made_up"));
        // Minimal does not allow the engine's LSP tools.
        assert!(!Profile::Minimal.allows("helios_lsp_definition"));
        // Standard does.
        assert!(Profile::Standard.allows("helios_lsp_definition"));
        // Standard does NOT allow write-shape tools.
        assert!(!Profile::Standard.allows("helios_lsp_rename_apply"));
        assert!(!Profile::Standard.allows("heliosdb_insert"));
    }

    // ---- strip mode parsing ----

    #[test]
    fn strip_mode_parses_int_none_all() {
        assert_eq!(StripDescMode::parse("none").unwrap(), StripDescMode::None);
        assert_eq!(StripDescMode::parse("all").unwrap(), StripDescMode::All);
        assert_eq!(
            StripDescMode::parse("200").unwrap(),
            StripDescMode::ShortenTo(200)
        );
        assert!(StripDescMode::parse("nope").is_err());
    }

    // ---- tools/list filtering + stripping ----

    fn sample_list_resp() -> String {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {
                        "name": "helios_lsp_definition",
                        "description": "LSP-style definition lookup with hint-based ranking — returns Vec<DefinitionRow>",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "helios_lsp_rename_apply",
                        "description": "Apply a rename across the symbol's reference set",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "heliosdb_query",
                        "description": "Run a read-only SQL query against the embedded engine",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "helios_repo_summary",
                        "description": "Plugin: compressed repo overview (pre-computed at ingest)",
                        "inputSchema": { "type": "object" }
                    },
                ]
            }
        });
        body.to_string()
    }

    #[test]
    fn list_strips_descriptions_per_profile() {
        let resp = sample_list_resp();
        let out = trim_tools_list_wire(&resp, Profile::Standard, StripDescMode::ShortenTo(20));
        let parsed: JsonValue = serde_json::from_str(&out).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        // Standard allows helios_lsp_definition + heliosdb_query +
        // helios_repo_summary, drops helios_lsp_rename_apply.
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"helios_lsp_definition"));
        assert!(names.contains(&"heliosdb_query"));
        assert!(names.contains(&"helios_repo_summary"));
        assert!(!names.contains(&"helios_lsp_rename_apply"));
        // Every description ≤ 20 bytes, no truncation marker.
        for t in tools {
            let d = t["description"].as_str().unwrap();
            assert!(d.len() <= 20, "description too long: {d:?}");
            assert!(!d.contains("[+"));
        }
        // inputSchema preserved.
        for t in tools {
            assert!(t.get("inputSchema").is_some());
        }
    }

    #[test]
    fn list_filter_minimal_keeps_only_allowlist() {
        let resp = sample_list_resp();
        let out = trim_tools_list_wire(&resp, Profile::Minimal, StripDescMode::None);
        let parsed: JsonValue = serde_json::from_str(&out).unwrap();
        let names: Vec<&str> = parsed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        // Minimal keeps heliosdb_query + helios_repo_summary; drops
        // both helios_lsp_* entries (lsp_definition is not in the
        // Minimal allow list).
        assert!(names.contains(&"heliosdb_query"));
        assert!(names.contains(&"helios_repo_summary"));
        assert!(!names.contains(&"helios_lsp_definition"));
        assert!(!names.contains(&"helios_lsp_rename_apply"));
    }

    #[test]
    fn list_full_is_passthrough() {
        let resp = sample_list_resp();
        let out = trim_tools_list_wire(&resp, Profile::Full, StripDescMode::None);
        // Full + None should be byte-identical (the early-exit path).
        assert_eq!(out, resp);
    }

    #[test]
    fn list_strip_all_empties_descriptions() {
        let resp = sample_list_resp();
        let out = trim_tools_list_wire(&resp, Profile::Full, StripDescMode::All);
        let parsed: JsonValue = serde_json::from_str(&out).unwrap();
        for t in parsed["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["description"].as_str().unwrap(), "");
        }
    }

    #[test]
    fn list_invalid_json_passthrough() {
        let garbage = "{ not json";
        assert_eq!(
            trim_tools_list_wire(garbage, Profile::Minimal, StripDescMode::ShortenTo(10)),
            garbage
        );
    }

    #[test]
    fn list_shortens_at_char_boundary() {
        // Description containing emoji at a boundary that would split.
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [{
                    "name": "heliosdb_query",
                    "description": "💰💰💰💰💰 cap"
                }]
            }
        })
        .to_string();
        let out = trim_tools_list_wire(&body, Profile::Full, StripDescMode::ShortenTo(9));
        let parsed: JsonValue = serde_json::from_str(&out).unwrap();
        let d = parsed["result"]["tools"][0]["description"].as_str().unwrap();
        // Should back off to 2 full emoji = 8 bytes (≤ 9), never panic.
        assert!(d.starts_with("💰💰"));
        assert!(d.len() <= 9);
    }
}
