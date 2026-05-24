---
description: Show the active heliosdb-codekb MCP profile and the wrapper-tool ranking the agent should use for common question shapes.
---

You are surfacing the compression-mode hint for the heliosdb-codekb MCP plugin running against this project. The plugin runs in one of three profiles (`minimal`, `standard`, `full`) that determine which tools the agent sees. Show the user what's active and remind them (and yourself) what each wrapper is best at.

## 1. Detect active profile

```bash
# Try config first (user-level override), then fall back to the
# binary's built-in default (standard).
CONFIG=$(heliosdb-codekb-mcp config path 2>/dev/null)
if [[ -f "$CONFIG" ]]; then
  PROFILE=$(grep -E '^profile\s*=' "$CONFIG" | head -1 | sed -E 's/.*"(.*)"/\1/' || true)
fi
echo "Active profile: ${PROFILE:-standard (built-in default)}"
```

## 2. Print the wrapper ranking

Tell the user:

> The `helios_*` MCP wrapper tools are designed to replace common Read+Grep loops with one distilled call. Pick the wrapper that matches the question shape:
>
> | If the user asks… | Reach for |
> |---|---|
> | "Show me the architecture / overview" | `helios_repo_summary(detail="file_index")` |
> | A doc question ("how does X work in the docs") | `helios_outline_first(query="X")`, then `helios_doc_drill(section_id)` if needed |
> | "Where is X defined / who calls it" | `helios_symbol_card(qualified_name="X")` |
> | Paraphrase semantic search | `helios_semantic_filter(query="X", where_lang=…)` |
> | "What changed between A and B" | `helios_git_summary(commit_a=…, commit_b=…)` |
>
> Fall back to the engine primitives (`helios_lsp_*`, `helios_graphrag_search`, `heliosdb_query`) only when the wrapper returns `not_found` or `cards_not_built`.

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
#   profile = "minimal"  # or "full"
# Then restart the MCP server (a fresh agent session does this automatically).
```
