//! Plugin-side trimming layer for engine MCP tool responses.
//!
//! Why: bench measurements (`bench/README.md`) showed `helios_lsp_*` and
//! `helios_graphrag_search` responses bloating context with neighbouring-
//! symbol bodies and full doc-section text. Tokens cost real money;
//! agents that don't need the full body shouldn't pay for it.
//!
//! What this module does: walks a JSON value in place and truncates
//! every string field longer than `max_bytes`, replacing the overflow
//! with a marker that tells the agent how much was dropped — so it can
//! ask for the original via a tool call if it actually needs it.
//!
//! Char-boundary safe (won't panic on multi-byte UTF-8 — same lesson as
//! the `src/linker.rs` emoji bug we already shipped a fix for).
//!
//! Applied to MCP `tools/call` responses only; other JSON-RPC methods
//! (`initialize`, `tools/list`, `resources/*`, `ping`, `helios/info`)
//! pass through untouched — they're small and the schema is part of the
//! protocol.

use serde_json::Value as JsonValue;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // 5 emoji × 4 bytes = 20 bytes, all in one char-boundary group.
        // Cap at 9 bytes (mid-emoji at position 8 → must back off to 8/4=2 full emoji = 8 bytes).
        let s = "💰💰💰💰💰".to_string();
        let mut v = json!({ "x": s.clone() });
        trim_value(&mut v, 9);
        let out = v["x"].as_str().unwrap();
        // Must not panic; must produce valid UTF-8.
        assert!(out.starts_with("💰💰"));
        assert!(out.contains("[+"));
    }

    #[test]
    fn wire_form_skips_non_result_methods() {
        // tools/list responses live in `result.tools` — trimming
        // shouldn't touch the request `method` or top-level RPC
        // fields, only `result`.
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
}
