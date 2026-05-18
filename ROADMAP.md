# heliosdb-codekb-mcp — Roadmap

Pilot status as of 2026-04-30.  Written after the
`feat/phase3-perf-batch` engine branch and the v1.2.x plugin work
landed; cold ingest on the pilot corpus (`~/Helios/Nano`,
666 files / 18 k symbols / 115 k refs) is now **26 s** end-to-end —
**31× the original v3.19.1 baseline** of 13 m 39 s.

This file groups every remaining FR / known-gap / future-direction
item in one place.  Tier 0 = "next on the bench"; Tier 5 =
"future-direction, post-v1".

## Tier 0 — next experiments

| # | Item | Effort | Impact |
|---|---|---|---|
| 1 | **Re-enable body embeddings** — *closed by `05ed683`*. Inline `--with-embeddings` ships as opt-in (3 m 14 s pilot); `--background-quality` (`<commit>`) keeps the user wait at the fast-pass cost while the embedding pass runs in a detached child. | S | Major **`helios_graphrag_search` quality** lift, no longer at the cost of user wait time on large repos. |

## Tier 1 — small, contained, plugin-side

| # | Item | Effort | Impact |
|---|---|---|---|
| 2 | **Expose `result_cache::stats()` via `helios/info`** — *closed end-to-end*. Engine half: `26956ba` (in v3.22.2). Plugin half: `<commit>` adds `status --source X --mcp-url <addr>` which best-effort-fetches `/info` from a running `serve --http` and renders a one-liner: `mcp cache : 0 / 256 entries, 0.0% hit rate (0 hit / 0 miss), gen 0`. Unreachable URL → graceful one-line note, no exit failure. | XS | Ops visibility. |
| 3 | **Plugin ingest contract tests** | M | Today's `tests/config_smoke.rs` covers config only. End-to-end ingest tests would catch regressions across plugin ↔ engine version transitions. |
| 4 | **`.gitattributes linguist-generated` honour** — *closed by `<commit>`*. `<root>/.gitattributes` and `<root>/.git/info/attributes` parsed for `linguist-generated[=true|=set]` glob patterns, matched against relative paths in the walk loop, alongside the existing `@generated` 4-KiB content-marker check. | S | GitHub-Linguist polish.  Long tail of generated files. |
| 5 | **Resume-on-interrupt for cold ingest** — *closed by `<commit>`*. Phase-level checkpoint at `<kb>/.ingest-state.json` records `walk → code_index → graph_rag` transitions. On startup, plugin reads the file; if a phase ≥ `code_index` was recorded, the walk is skipped and the engine's content-hash gate handles per-file resume inside `code_index`. Checkpoint cleared on successful completion. Status command surfaces "ingest resume" line when present. | M-L | Killing a cold ingest mid-flight no longer loses all work. |
| 6 | **HTTP transport option in the plugin** — *closed by `<commit>`*. `serve --source X --http <addr>` binds the engine's `mcp_router` (POST `/`, GET `/ws`, GET `/sse`, GET `/info`). Stdio remains the default. Graceful shutdown on Ctrl-C. | M | Engine already exposes `/mcp` HTTP / WS / SSE.  Plugin spawns stdio only. Adding HTTP unlocks Cursor and other clients. |

## Tier 2 — engine-side, owned by the HeliosDB-Nano team

| # | Item | Status |
|---|---|---|
| 7 | `FEATURE_REQUEST_cross_process_on_conflict.md` | **fixed on engine branch `feat/cross-process-conflict-and-cache-stats` (`6ec74d3`)**. Two divergences fixed: `insert_tuple_versioned_with_schema` now calls `check_unique_constraints` (matching the SQL fast-path sibling); `execute_plan_with_params_inner`'s INSERT arm now honours `on_conflict` (DoNothing / DoUpdate). Plugin's "skip walk in child" workaround can be removed once the engine pin advances. |
| 8 | `BUGS_MCP_SERVER_CLI_DOCS.md` (option a — docstring fix) | **closed by engine commit `60f0460`**. `docs/code_graph/{pilot,troubleshooting}.md` + module comments now redirect users from the (non-existent) `heliosdb-nano mcp-server` CLI subcommand to `heliosdb-codekb-mcp serve --source X` (with the new `--http <addr>` flag for non-stdio clients). |
| 9 | True multi-threaded multi-writer for `_hdb_code_*` (full `parallel_writes` scope) | gated on `Sync`-ing the catalog / ART / transaction state.  Substantial engine refactor.  **Pilot's 5-min target already met without it** — revisit only if a > 10 k file repo workload lands. |
| 10 | `streaming_pipeline` parse-write overlap | marginal at 12 % post-batched-drain (commit `7bb58c2`). Revisit at 10 k+ file scale. |
| 11 | `adaptive_topk` for `helios_graphrag_search` | blocked on Phase 3.1 vector + BM25 hybrid scoring. Today's `search.rs` ranks by hop_distance only. |
| 12 | Pre-existing `tests/ha_integration.rs` E0063 (`missing tx_id`) | not pilot's; concurrent HA work.  Probably resolves when their HA branch lands.  Today blocks `cargo test --tests` running cleanly across the whole crate. |

## Tier 3 — distribution, required for actual public release

| # | Item | Owner |
|---|---|---|
| 13 | Engine `cargo publish heliosdb-nano` (post-merge of `feat/phase3-perf-batch`) | engine team |
| 14 | Plugin Cargo.toml swap from `path = "../Nano"` → `version = "3.22"` | plugin (one-line change) |
| 15 | `cargo publish heliosdb-codekb-mcp` | plugin |
| 16 | GitHub Actions matrix for prebuilt binaries (linux / macOS × x86_64 / aarch64) | plugin |
| 17 | Marketplace listing (Anthropic-curated and / or community) | distribution |
| 18 | Plugin's launcher auto-fetch path (currently a stub) becomes live | plugin (auto-activates once #16 ships) |

## Retrieval-quality audit (2026-05-18)

Goal: make this plugin **the best token-saving MCP for agentic search**
across code + docs. Beat tree-sitter alone (which only sees code) and
beat PageIndex (which only navigates doc TOCs) by composing engine
features both already cover, into one KB.

Findings, ranked by expected token-savings lift on the pilot corpus:

| # | Gap                                                                            | Engine support today                                | Status      |
|---|--------------------------------------------------------------------------------|-----------------------------------------------------|-------------|
| A | All docs ingested as `ChunkStrategy::Row` — no `DocSection` / `PART_OF` nodes   | `ChunkStrategy::Headings` (engine `graph_rag/ingest.rs`) | **closed (this PR)** — `.md`/`.markdown` now dual-routed: code-graph headings + `DocSection`/`DocChunk` projection for `helios_graphrag_search` to navigate by section instead of returning whole docs |
| B | No text→symbol entity links — agents can't follow a README mention into code    | `link_exact_qualified` (engine `graph_rag/linker.rs`) | **closed (this PR)** — post-ingest phase emits `MENTIONS` edges from `DocChunk` text to `_hdb_code_symbols.qualified`. Uses a plugin-side bulk implementation (`src/linker.rs`) — streams `(from, to)` tuples to a tempfile as multi-row `INSERT … VALUES (…)` batches, then applies via `execute_batch` under `SET bulk_load_mode = true`. The engine's `link_exact_qualified` was ~89 min on the pilot Nano corpus (70k edges via per-row implicit-txn INSERTs); the bulk path targets a 50–100× speedup |
| C | No centrality-based rerank on `helios_graphrag_search`                         | `centrality_rerank` (engine `graph_rag/centrality.rs`) | engine-side wiring needed — search currently ranks by hop-distance only |
| D | Doc outline is one level deep (`level > 0` flattens `#`/`##`/`###`)            | `split_markdown_headings` flat                       | engine FR — nested `DocSection` parents → true tree, PageIndex-grade navigation |
| E | `WITH CONTEXT (…)` SQL clause reachable only via `heliosdb_query`              | `graph_rag_expand_with_context` exists; no dedicated MCP tool wrapper | engine-side — file a request for `helios_graphrag_with_context` |
| F | `ingest_email` / `ingest_issues` / `ingest_qa` adapters not exposed            | engine adapters present                              | deferred — out of v1 scope (would unlock "agentic search across project comms") |
| G | `heliosdb_branch_*` + `heliosdb_time_travel` MCP tools unused in the docs/UX   | already exposed via `mcp-endpoint`                   | docs-only — agent workflow note to be added to README |
| H | `.proto` / `.thrift` / `.graphql` / `.java` / `.cpp` fall to text retrieval     | engine grammars not registered                       | engine FR — extend `code-graph` grammar list |

### Why this matters for token saving

Without (A): a 2000-line architecture doc returns as one `DocChunk` —
the agent pays for the entire blob just to find the section it
needed. With (A): the agent gets the smallest matching `DocChunk`
and can walk `PART_OF` to neighbours only when needed.

Without (B): the agent must run two queries — one against
`_hdb_graph_nodes` (docs) and one against `_hdb_code_symbols` (code) —
and reconcile them client-side, burning context and tool budget.
With (B): one `helios_graphrag_search` traverses both halves through
the `MENTIONS` edge.

## Tier 4 — future direction, post-v1.0

| # | Item | Notes |
|---|---|---|
| 19 | Bundled Docling compose stack | explicitly deferred (Docker pre-req too heavy for v1.0). Revisit if pilot hits scanned-PDF / OCR demand. |
| 20 | IDE plugin (VS Code, JetBrains) | separate repo. Terminal / MCP works for v1; IDE expands the audience. |
| 21 | Better counterfactual model in token-dashboard | per-query learnt model vs today's tool-class constants. |
| 22 | Inline savings preview in MCP tool output | needs MCP middleware in engine to decorate tool responses. |
| 23 | Body-embeddings + adaptive top-k on `graphrag_search` once Phase 3.1 scoring lands | quality lift for cross-modal queries. |

## Cumulative cold-ingest history (for context)

| Engine | Mode | Wall (Nano cold, 666 files) | vs v3.19.1 baseline |
|---|---|---|---|
| v3.19.1 baseline | sequential, Sync | 13 m 39 s | 1.0× |
| v3.21.0 | parallel parse, Sync | 5 m 43 s | 2.4× |
| v3.22.0 | + Tier 2.4 v2 direct-write | 3 m 42 s | 3.7× |
| v3.22.1 | + FK txn-write-set merge | 3 m 46 s | 3.6× |
| v3.22.1 + plugin Async wal_sync | regenerable-index Async fsync | 1 m 42 s | 8.0× |
| **v3.22.1 + Async + cross-file bulk batching (`7bb58c2`)** | **current state — fast tier** | **26.4 s** | **31.0×** |
| v3.22.1 + Async + bulk-batch + `--with-embeddings` (inline) | quality tier, blocking | 3 m 14.9 s | 4.2× |
| **v3.22.1 + Async + bulk-batch + `--background-quality`** | **user wait time** | **~26 s** | **31×** (unchanged) |
| same | total wall (parent + detached child) | ~3 m 15 s | 4.2× |

## How to read this file

- **Tier 0** items are bench-and-ship within a single session.
- **Tier 1** items are clean follow-up commits on this repo.
- **Tier 2** items live in `~/Helios/Nano` (the engine repo); the
  plugin tracks them so version pins stay accurate.
- **Tier 3** items unblock a public crates.io / marketplace release.
- **Tier 4** items are the "everything we'd love to do post-v1.0"
  list — captured here so they don't drift.

Status snapshot at: 2026-04-30.
