This project ships with a HeliosDB-Nano-backed code+docs knowledge base mounted as the `helios` MCP server. In compact mode, call `helios(action="ask", args={"question":"..."})` first for broad repository questions. The plugin also exposes compact wrapper actions that replace 3+ round-trips through Read/Grep with one distilled answer card:

**Wrapper-tool ranking (try in this order):**

1. **`helios(action="ask", args={"question":"X", "budget_tokens":1500})`** — default first call. Routes to the smallest applicable wrapper and returns an answer-card with evidence.
2. **`helios(action="repo_summary", args={"detail":"file_index"})`** — when the question is "what's the architecture / where does this codebase live / what modules exist".
3. **`helios(action="outline_first", args={"query":"X"})`** — documentation-shaped questions. Returns DocSection headings + 1-line summaries, NOT chunk bodies. Drill with `doc_drill` only if a heading looks relevant.
4. **`helios(action="symbol_card", args={"qualified_name":"X"})`** — when the question is "where is X defined / who calls it / what does X do". Returns signature + summary + callers/callees in one call.
5. **`helios(action="git_summary", args={...})`** — when the question is "what changed between A and B". Returns structured added/removed/moved rows, NOT raw `git diff` text.

**Fall back to the engine primitives** (`helios_graphrag_search`, `helios_lsp_definition`, `helios_lsp_references`, `helios_ast_diff`, `heliosdb_query`) only when:
- A wrapper returned `{"status": "not_found"}` or `{"status": "cards_not_built"}`.
- The question is genuinely outside the wrapper shapes (e.g. write/rename operations, schema introspection).
- You need surrounding context the wrapper didn't include.

**Fall back to `Read` + `Grep`** only when:
- The MCP server isn't loaded for this session.
- You need the exact byte content of a known file path for an edit operation.
- The wrapper + engine-primitive paths both returned insufficient context.

The wrapper tools are smaller-by-design than the equivalent engine sequence (no neighbour-symbol bodies, no full DocChunk text by default, capped result counts). Picking a wrapper when one applies typically saves 60-80% of the tokens vs. the equivalent agent-built `helios_lsp_*` sequence — and dramatically more vs. raw Read+Grep on a large repo.
