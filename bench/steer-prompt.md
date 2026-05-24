This project ships with a HeliosDB-Nano-backed code+docs knowledge base mounted as the `helios` MCP server. The plugin exposes **six wrapper tools** plus a curated subset of the engine's primitives. For code or documentation questions about this repository, prefer the wrappers — they replace 3+ round-trips through Read/Grep with one distilled call:

**Wrapper-tool ranking (try in this order):**

1. **`helios_repo_summary(detail="file_index")`** — when the question is "what's the architecture / where does this codebase live / what modules exist". Returns a PageRank-ranked file index with per-file top symbols, pre-computed at ingest.
2. **`helios_outline_first(query="X")`** — when the question is documentation-shaped ("how does X work according to the docs"). Returns DocSection headings + 1-line summaries, NOT chunk bodies. Drill with `helios_doc_drill(section_id)` only if a heading looks relevant.
3. **`helios_symbol_card(qualified_name="X")`** — when the question is "where is X defined / who calls it / what does X do". Returns signature + first-line docstring + ≤5 callers + ≤5 callees in one call. Skips the `Read` step entirely.
4. **`helios_semantic_filter(query="X", where_lang=…, where_path_glob=…)`** — when the question is paraphrase-style ("find anything related to caching"). Filters on language/kind/path BEFORE the vector traversal, so the result doesn't blow up.
5. **`helios_git_summary(commit_a, commit_b)`** — when the question is "what changed between A and B". Returns structured added/removed/moved/signature-changed rows, NOT raw `git diff` text.

**Fall back to the engine primitives** (`helios_graphrag_search`, `helios_lsp_definition`, `helios_lsp_references`, `helios_ast_diff`, `heliosdb_query`) only when:
- A wrapper returned `{"status": "not_found"}` or `{"status": "cards_not_built"}`.
- The question is genuinely outside the wrapper shapes (e.g. write/rename operations, schema introspection).
- You need surrounding context the wrapper didn't include.

**Fall back to `Read` + `Grep`** only when:
- The MCP server isn't loaded for this session.
- You need the exact byte content of a known file path for an edit operation.
- The wrapper + engine-primitive paths both returned insufficient context.

The wrapper tools are smaller-by-design than the equivalent engine sequence (no neighbour-symbol bodies, no full DocChunk text by default, capped result counts). Picking a wrapper when one applies typically saves 60-80% of the tokens vs. the equivalent agent-built `helios_lsp_*` sequence — and dramatically more vs. raw Read+Grep on a large repo.
