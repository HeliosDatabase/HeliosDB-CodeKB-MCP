This project ships with a HeliosDB-Nano-backed code+docs knowledge base mounted as the `helios` MCP server. In compact mode, call `helios(action="ask", args={"question":"..."})` first for broad repository questions. The plugin also exposes compact wrapper actions that replace 3+ round-trips through Read/Grep with one distilled answer card:

**Wrapper-tool ranking (try in this order):**

1. **`helios(action="file_lookup", args={"path":"repo/path.rs"})`** — exact source/config path questions. Use `query` when only a filename or directory fragment is known. Add `include_content:true` only after the compact metadata result identifies the right file.
2. **`helios(action="doc_lookup", args={"path":"repo/README.md"})`** — exact README/docs questions. Use `query` for documentation search and keep `include_content:false` unless a snippet is required. For "mentions X, Y, and Z" questions, use `doc_lookup` with a short keyword query such as `"MCP GraphRAG code indexing"`.
3. **`helios(action="repo_summary", args={"detail":"minimal","limit":50})`** — portfolio/module inventory, architecture map, "which repos exist", and named integration repo questions such as Kubernetes/Terraform/Pulumi. Works even when code-graph cards are not built by returning path inventory from the KB.
4. **`helios(action="ask", args={"question":"X", "budget_tokens":1000})`** — default for broad questions that are not clearly exact path, docs, or portfolio inventory.
5. **`helios(action="outline_first", args={"query":"X"})`** — documentation-shaped questions. Returns DocSection headings + 1-line summaries, NOT chunk bodies. Drill with `doc_drill` only if a heading looks relevant.
6. **`helios(action="symbol_card", args={"qualified_name":"X"})`** — when the question is "where is X defined / who calls it / what does X do" and code-graph cards are built. If it reports not_found/cards_not_built, switch to `file_lookup` or `doc_lookup`.
7. **`helios(action="git_summary", args={...})`** — when the question is "what changed between A and B". Returns structured added/removed/moved rows, NOT raw `git diff` text.

Do not call `helios(action="list_actions")` unless you cannot infer the schema from the examples above; it spends tokens on metadata instead of repository facts.

**Fall back to the engine primitives** (`helios_graphrag_search`, `helios_lsp_definition`, `helios_lsp_references`, `helios_ast_diff`, `heliosdb_query`) only when:
- A wrapper returned `{"status": "not_found"}` or `{"status": "cards_not_built"}`.
- The question is genuinely outside the wrapper shapes (e.g. write/rename operations, schema introspection).
- You need surrounding context the wrapper didn't include.

**Fall back to `Read` + `Grep`** only when:
- The MCP server isn't loaded for this session.
- You need the exact byte content of a known file path for an edit operation.
- The wrapper + engine-primitive paths both returned insufficient context.

The wrapper tools are smaller-by-design than the equivalent engine sequence (no neighbour-symbol bodies, no full DocChunk text by default, capped result counts). Picking a wrapper when one applies typically saves 60-80% of the tokens vs. the equivalent agent-built `helios_lsp_*` sequence — and dramatically more vs. raw Read+Grep on a large repo.
