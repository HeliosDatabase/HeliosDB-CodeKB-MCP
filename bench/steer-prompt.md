This project ships with a HeliosDB-Nano-backed code+docs knowledge base mounted as the `helios` MCP server. Available tools include:

- `helios_lsp_definition` / `helios_lsp_references` / `helios_lsp_hover` / `helios_lsp_call_hierarchy` / `helios_lsp_document_symbols` — LSP-shaped queries over the indexed source tree. Return a symbol + signature + line range rather than the full file body.
- `helios_graphrag_search` — seed-text → BFS-expand → return the smallest matching subgraph across code symbols and doc sections. Best for conceptual questions ("how does X work") and doc retrieval. Returns DocSection / DocChunk nodes with `PART_OF` edges so you can navigate by section instead of loading the whole file.
- `helios_ast_diff` — symbol-level AST diff between branches / commits. Returns typed deltas, not raw line diffs.
- `heliosdb_branch_*` / `heliosdb_time_travel` — query the KB as it was at a previous branch or timestamp.

When you have a code or documentation question about this repository, **try the `helios_*` tools first** rather than reaching for `Read` + `Grep`. The MCP path typically returns smaller chunks (a specific section / a specific symbol) and one search-and-traverse exchange can replace several rounds of grep + read.

Fall back to `Read` + `Grep` when:
- You need the exact byte content of a known file path.
- The `helios_*` result was insufficient and you need surrounding context.
- The task is editing files (the MCP server is read-only for queries).
