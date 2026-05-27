---
description: Show the active heliosdb-codekb MCP profile and the wrapper-tool ranking the agent should use for common question shapes.
---

You are surfacing the compression-mode hint for the heliosdb-codekb MCP plugin running against this project. The plugin now defaults to compact `helios(action,args)` mode, with profile mode (`minimal`, `standard`, `full`) available for clients that need per-tool schemas. Show the user what's active and remind them (and yourself) what each wrapper/action is best at.

## 1. Detect active profile

```bash
# Try config first (user-level override), then fall back to the
# binary's built-in default (compact mega-tool).
CONFIG=$(heliosdb-codekb-mcp config path 2>/dev/null)
if [[ -f "$CONFIG" ]]; then
  PROFILE=$(grep -E '^profile\s*=' "$CONFIG" | head -1 | sed -E 's/.*"(.*)"/\1/' || true)
  MEGA=$(grep -E '^mega_tool\s*=' "$CONFIG" | head -1 | awk '{print $3}' || true)
fi
echo "Active mode: ${MEGA:-true} mega-tool, profile: ${PROFILE:-standard}"
```

## 2. Print the wrapper ranking

Tell the user:

> The compact `helios` MCP actions are designed to replace common Read+Grep loops with one distilled call. Pick the action that matches the question shape:
>
> | If the user asks… | Reach for |
> |---|---|
> | General repo question | `helios(action="ask", args={"question":"..."})` |
> | "Show me the architecture / overview" | `helios_repo_summary(detail="file_index")` |
> | A doc question ("how does X work in the docs") | `helios_outline_first(query="X")`, then `helios_doc_drill(section_id)` if needed |
> | "Where is X defined / who calls it" | `helios_symbol_card(qualified_name="X")` |
> | "What changed between A and B" | `helios_git_summary(commit_a=…, commit_b=…)` |
>
> Fall back to the engine primitives (`helios_lsp_*`, `helios_graphrag_search`, `heliosdb_query`) only when the wrapper returns `not_found` or `cards_not_built`.
>
> `helios_semantic_filter` is advertised only in builds compiled with the future `wrappers-semantic` feature; otherwise use `ask`, `outline_first`, or engine GraphRAG as the fallback.

## 3. If `cards_not_built`, suggest a re-ingest

If the user has run `/codekb-setup` before this plugin shipped Layer 3 (pre-distillation), the two `_hdb_plugin_*_cards` tables don't exist yet, so `helios_repo_summary` and the docstring field on `helios_symbol_card` return `cards_not_built` /  empty. Re-ingest fixes it:

```bash
heliosdb-codekb-mcp ingest --source "${CLAUDE_PROJECT_DIR}"
```

The fresh ingest passes through every phase including the new distill step (build_symbol_cards + build_repomap_cards). Idempotent — unchanged symbols are skipped on subsequent runs.

## 4. Profile change tip (optional)

If the user wants to switch profiles for this project:

```bash
heliosdb-codekb-mcp config path   # shows the TOML location
# Edit it, set:
#   [serve]
#   mega_tool = false
#   profile = "minimal"  # or "standard" / "full"
# Then restart the MCP server (a fresh agent session does this automatically).
```
