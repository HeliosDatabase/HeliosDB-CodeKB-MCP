---
name: codekb-pro-features
description: Power-user features exposed by the heliosdb-codekb MCP server — time-travel, branch queries, AST diff, HTTP transport, hybrid search, cross-modal MENTIONS. Use when the user asks about historical code state, comparing branches, diffing implementations, or needs a non-stdio MCP transport.
---

The heliosdb-codekb MCP server inherits its tool surface from **HeliosDB-Nano** (a PostgreSQL-compatible embedded DB with vector + graph + time-travel). Most users never realise the engine gives them more than tree-sitter / file-grep would. Surface these when relevant.

## Time-travel: "what did this look like before?"

The engine indexes content under a logical time/branch axis. Tools that accept time-travel parameters can answer "show me the implementation of `foo` as it was at commit X / on branch Y / before date Z".

Available via:

- `heliosdb_query` with `WITH CONTEXT (… AS OF NOW … )` — explicit SQL.
- `heliosdb_time_travel` — direct MCP tool. Parameters include `as_of_timestamp` and `as_of_branch`.
- `heliosdb_branch_list` / `heliosdb_branch_create` / `heliosdb_branch_merge` — first-class branches over the KB itself, not just the source.

**Typical pro-user pattern:** before a refactor, `heliosdb_branch_create name=pre-refactor`. After: `helios_ast_diff` between current and `pre-refactor` to surface every changed symbol. Far smaller token footprint than `git diff` on a multi-thousand-line refactor because it returns *typed AST deltas*, not raw lines.

## AST diff (cross-branch or cross-commit)

`helios_ast_diff` returns symbol-level adds / removes / signature-changes between two versions, not raw text diff. For agents auditing API drift this is dramatically cheaper than reading both file versions.

## Hybrid search (BM25 + vector + graph)

- `helios_graphrag_search` — seed (BM25 or vector) → BFS expand over the typed graph (`PART_OF`, `MENTIONS`, `CALLS`, `REFERENCES`) → return the smallest matching subgraph.
- `heliosdb_hybrid_search` — direct BM25 + HNSW fusion on a user table; useful when querying ingested doc text directly.
- `heliosdb_bm25_index` — create a BM25 index on any text column (e.g. a project's `CHANGELOG` ingested into the `docs` table).

## Cross-modal MENTIONS

The plugin's post-ingest linker emits `MENTIONS` edges from doc text nodes to `_hdb_code_symbols`. A `helios_graphrag_search "FastEmbedder"` traverses both halves in one round trip — agent gets the README section AND the symbol back, not two queries' worth of context.

## Transport modes

Default plugin install uses stdio (one server per Claude Code session). For multi-client workflows (e.g. Cursor in another window querying the same KB), the user can run:

```bash
heliosdb-codekb-mcp serve --source <path> --http 127.0.0.1:8765
```

and any MCP-aware client can hit `POST /`, `GET /ws`, `GET /sse`, or `GET /info` (cache stats). The plugin's `/codekb-status --mcp-url …` reads `/info`.

## Tuning knobs worth knowing

- `--durable-writes` on `ingest`: switch to `WalSyncModeConfig::Sync` (fsync per write). Default is `Async` since the KB is regenerable from source. Mention only if user explicitly asks for crash-safe ingest.
- Result cache (engine-side per-process LRU): `read-only` tools like `helios_lsp_*` and `helios_graphrag_search` are auto-cached. Repeated identical calls cost zero engine work. Hit rate is visible via `/codekb-status --mcp-url …`.

## Compression-mode tool mapping (Phase 1)

The plugin ships six **wrapper tools** that compose engine library calls into one distilled response. They are smaller-by-design than the equivalent engine-primitive sequence. The agent should prefer them when one applies:

| Question shape | Prefer this wrapper | Instead of |
|---|---|---|
| General repository question | `helios(action="ask", args={"question":"..."})` or `helios_ask(question="...")` | model-selected Read/Grep loops |
| "Describe the architecture" / "what's the layout of this codebase" | `helios_repo_summary(detail="file_index")` | walking `Read` over many files |
| Doc question ("where do the docs cover X") | `helios_outline_first(query="X")` → if needed, `helios_doc_drill(section_id)` | `helios_graphrag_search` returning whole DocChunks |
| "Where is `X` defined / who calls it" | `helios_symbol_card(qualified_name="X")` | `Read` + `Grep` loop or `helios_lsp_definition` + `helios_lsp_references` separately |
| Diff / refactor audit ("what changed between A and B") | `helios_git_summary(commit_a, commit_b)` | `Bash(git diff)` |

`helios_semantic_filter` is intentionally hidden unless the binary is built with `--features wrappers-semantic`; the engine filtered-KNN dependency is not in the default release yet.

Each wrapper is gated by the active `--profile`. The default `standard` profile advertises all six plus a curated read-only engine subset; `minimal` drops the engine LSP/branch tools entirely; `full` is pass-through.

**Falling back:** if a wrapper returns `{"status": "not_found"}` or `{"status": "cards_not_built"}`, drop down to the engine-primitive tools (`helios_lsp_*`, `helios_graphrag_search`). The wrappers degrade gracefully — they never block a question.

## What this plugin is NOT

- **Not a code formatter / linter.** No `helios_format` / `helios_lint`. Use language-native tools.
- **Not auto-re-indexing.** The plugin only re-ingests when the user runs `/codekb-ingest`. Suggest this after meaningful changes — but warn before doing it on a repo with the engine FK regression unfixed.
- **Not real-time collaborative.** One writer per KB (engine constraint). Multiple readers are fine via HTTP transport.
