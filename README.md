# heliosdb-codekb-mcp

MCP stdio server for code+docs knowledge bases. Embeds
[HeliosDB-Nano](https://crates.io/crates/heliosdb-nano) as a Rust
library (`code-graph`, `graph-rag`, `mcp-endpoint`, `code-embed`
features) and exposes its LSP-shaped + GraphRAG tools to Claude Code,
Cursor, Codex, Aider, and any other MCP-aware agent — over plain stdio
JSON-RPC, no ports, no auth dance, all local.

**v0.2.0 headline (qwen3-coder:30b on `/home/gpc/HDB/Full`):
−37.2 % model tokens vs no-MCP across 15 dev questions** (full report
in [`MCP_ECOSYSTEM_BENCHMARK_REPORT_2026-05-27.md`](./MCP_ECOSYSTEM_BENCHMARK_REPORT_2026-05-27.md)).
Biggest single-question wins: **−76 k, −69 k, −47 k tokens** on
broad-architecture queries. See [Honest caveats](#honest-caveats) for
the workloads where Read+Grep is still cheaper.

## Why it improves answer quality, not just token count

The bench measures cost. But the reason MCP wins on broad questions
isn't just compression — it's that the wrapper layer **gives the model
better-grounded raw material to reason from**. Every quality lever
also reduces tokens; both directions point the same way.

| Quality lever | What changes for the agent |
|---|---|
| **Pre-distilled answer cards** (`helios.answer_card.v1`) | Every wrapper response carries a `summary` + `evidence` array (file:line citations, qualified symbol names, doc-section IDs) + `omitted` metadata. The model doesn't have to remember where it found something across N turns — the citation rides with the answer. Less hallucinated provenance, fewer "I think this was in some file…" lapses. |
| **`helios_ask` question router** | One entry point that inspects the question and picks the right sub-wrapper (repo summary / outline-first / symbol card / doc drill). Stops the model from going down the wrong path (e.g. grep when it should outline-first). On the Full bench this picked correctly on most questions; the report calls out where it didn't and what would fix it. |
| **LLM-distilled symbol summaries** (Phase 2 ingest) | Each public symbol gets a one-sentence purpose summary generated ONCE at ingest by a code-aware model (e.g. `qwen3-coder:30b`), stored in `_hdb_plugin_symbol_cards.llm_summary`, reused across every agent query. A Haiku 4.5 query against a qwen-distilled card often produces a more accurate answer than Haiku reading the raw body and re-deriving the purpose itself. |
| **Cross-modal `MENTIONS` edges** | When the agent searches "FastEmbedder", the response includes the symbol AND the doc passages that name it. The agent verifies the doc claim against the actual code in one round-trip instead of having to grep both surfaces independently and stitch results. |
| **PageRank-ranked file index** | `helios_repo_summary` orders files by the symbol-graph PageRank, so the agent investigates the load-bearing implementation files before tests or examples. Without this, "describe the architecture" agents reliably waste 5–10 turns walking peripheral code. |
| **Typed AST diff for "what changed"** | `helios_ast_diff` / `helios_git_summary` return structured `{added,removed,moved,signature_changed}` rows, not raw line diffs. The model parses 200 bytes of structured rows instead of pages of `+/-` lines, with the same information density. |
| **Bounded-by-design responses** | Wrappers cap result counts (`max_callers=5`, `max_chunks=10`, `fields=[…]` projection). The model never gets a half-truncated response with `…[truncated]` and has to guess what came next — every response is complete relative to its declared bounds. |
| **Compact one-tool surface** | `helios(action, args)` default replaces the 12-tool catalogue. The model sees ~720 bytes of tool descriptions per turn instead of ~6 KB. Less "what tool should I call?" deliberation, more answer reasoning. |

## When the plugin earns its keep

| Scenario | What you get |
|---|---|
| **Broad architecture / cross-cutting questions** | "Describe the WAL flushing path", "where does foreign-key validation happen", "what modules depend on storage" — these are the questions where MCP saved 47-76 k tokens per question on the Full bench. Read+Grep here sends the agent on a 20+ turn grep tour; MCP lands in 3-6 turns with citations. |
| **Cross-modal queries** ("which doc section mentions the `FastEmbedder` symbol?") | `Read`+`Grep` literally can't answer in one shot. The plugin's text → code `MENTIONS` edges traverse both sides in one tool call. |
| **"What did this look like before"** (time-travel / branch / commit diff) | `helios_ast_diff`, `heliosdb_branch_*`, `heliosdb_time_travel` answer "what did this symbol look like at commit X / on branch Y" with typed AST deltas. Read+Grep would need a checkout + reindex + grep cycle. |
| **Catastrophe prevention on multi-turn tours** | On the canonical Haiku bench, Opus hit its budget cap on 5/10 questions WITHOUT MCP (no answer); Haiku burned 22-32 grep+read turns on the same questions. WITH MCP, the same questions land in 3-6 turns. This stabilises the *tail*, not always the median. |
| **Doc-heavy workflows** | Heading-chunked `.md` ingest means `helios_graphrag_search` returns the matching `DocSection` instead of the full file. Doc retrieval shrinks to one section per question. |

## Honest caveats

- **Direct file/symbol lookups where the agent knows the path can still be cheaper through raw Read+Grep.** The Full bench: MCP won 8/15 questions, lost 7/15. Wins were broad; losses were targeted lookups (e.g. "show me the public types in crate X" — one Read of the lib.rs beats a wrapper round-trip).
- **Symbol-card population is content-hash-gated.** If `helios_symbol_card` returns `{"status": "not_found"}`, re-run `ingest` to backfill the cards. The Full benchmark report has a "Recommended Next Work" section calling out the cases where card coverage matters most.
- **Cold-start cost is high on benchmarks** because each WITH-MCP run launches a fresh `serve` subprocess. Real agent sessions keep the server warm; the per-question cold start ratio improves with longer sessions.
- **Engine-side FRs are queued.** Four engine improvements would unlock another step-change in both quality and cost — tracked in [`ENGINE_FRS_FROM_CODEKB_2026-05-26.md`](./ENGINE_FRS_FROM_CODEKB_2026-05-26.md). FR #1 (FK validation throughput) is the prerequisite for benching on `/home/gpc/HDB`-scale (10 k+ files) corpora; FR #4 (`tools/list verbose=false`) would compose with the existing `--mega-tool` for even smaller catalogue payloads.

Use the plugin when the cross-modal / time-travel / catastrophe-prevention / answer-grounding value matters. The aggregate token win on broad workloads (−37 % on Full) is a bonus; the per-answer quality lift is the durable value.

## Install

### From crates.io (recommended)

Latest release: **[v0.2.0](https://crates.io/crates/heliosdb-codekb-mcp)** (2026-05-27).

```bash
cargo install heliosdb-codekb-mcp
# binary: ~/.cargo/bin/heliosdb-codekb-mcp
```

### Pre-built binary

Linux x86_64 binaries are published per release on GitHub:
**[v0.2.0 release page](https://github.com/dimensigon/heliosdb-codekb-mcp/releases/tag/v0.2.0)**.

| Platform | Status |
|----------|--------|
| Linux x86_64 | ✅ pre-built binary + crates.io |
| macOS x86_64 (Intel) | crates.io (`cargo install`) |
| macOS / Linux aarch64 | crates.io (`cargo install`) |

```bash
curl -L \
  https://github.com/dimensigon/heliosdb-codekb-mcp/releases/download/v0.2.0/heliosdb-codekb-mcp-linux-x86_64 \
  -o /usr/local/bin/heliosdb-codekb-mcp
chmod +x /usr/local/bin/heliosdb-codekb-mcp
```

Verify with the matching `.sha256` from the release page.

### From source (any platform)

```bash
cargo build --release --features native-binary-docs
# binary: ./target/release/heliosdb-codekb-mcp
```

## Use as a Claude Code plugin

This repo ships a `.claude-plugin/plugin.json` manifest plus three slash
commands (`/codekb-setup`, `/codekb-ingest`, `/codekb-status`) and a
`codekb-pro-features` skill. Install for a single Claude Code session:

```bash
claude --plugin-dir /abs/path/to/heliosdb-codekb-mcp
```

Or fetch directly from a release/branch URL once distributed via a
plugin marketplace. On first use, run `/codekb-setup` — it walks you
through the binary install (if needed), asks whether to enable the
optional one-time ~30 MB embeddings download, and indexes the current
project.

The plugin's `.mcp.json` declares the MCP server with
`--source ${CLAUDE_PROJECT_DIR}`, so the helios MCP tools follow
whichever project Claude Code is opened in.

## What it is

Three things at once:

1. **A user-level config tool.** Run `init --source <PATH> --mode
   <co-located|global|hybrid>` once per source-directory you want
   indexed.  The choice persists in
   `${XDG_CONFIG_HOME:-~/.config}/heliosdb-codekb-mcp/config.toml`.
2. **A KB resolver.** Given a source path, finds the right KB
   directory using the persisted config (with longest-prefix match
   for hybrid setups that span multiple sub-trees).
3. **The MCP stdio server.** `serve --source <PATH>` opens an
   `EmbeddedDatabase` rooted at that source's KB and runs
   `heliosdb_nano::mcp::McpServer::new(db).run().await`.  All
   query tools (`helios_lsp_*`, `helios_graphrag_search`,
   `helios_ast_diff`, …) come from the engine library — this
   binary owns transport and config, not tool surface.

## KB-location modes

| Mode         | Where the KB lives                                       | Use when                                    |
|--------------|----------------------------------------------------------|---------------------------------------------|
| `co-located` | `<source>/.helios-kb` (auto-added to `.gitignore`)       | Single repo, KB travels with the code.       |
| `global`     | `${XDG_DATA_HOME}/helios-kb/<slug>` (slug = source path) | Many independent projects on one machine.   |
| `hybrid`     | An explicit `--kb <PATH>` you can register many sources to | Multi-repo aggregate (e.g. `~/Helios/*`).   |

## Document ingestion (default tier — no Docling)

Out of the box, ingestion uses pure-Rust crates.  Single static
binary, no Python, no Docker.  Covers ~80% of real-world content:

| Format                                | Backend                  |
|---------------------------------------|--------------------------|
| `.rs` / `.py` / `.ts` / `.tsx` / `.js` / `.go` / `.sql` | tree-sitter via the engine's `code-graph` |
| `.md` / `.txt`                         | tree-sitter Markdown / generic text |
| `.pdf` (born-digital)                  | [`pdf-extract`](https://crates.io/crates/pdf-extract) |
| `.docx`                                | [`docx-rs`](https://crates.io/crates/docx-rs) |
| `.xlsx`                                | [`calamine`](https://crates.io/crates/calamine) |

## Document ingestion (`--features docling`)

Build with `--features docling` and the binary additionally routes:

| Format                              | Backend (via engine `graph_rag::ingest_*`) |
|-------------------------------------|--------------------------------------------|
| Scanned / OCR-required PDFs         | Docling layout + OCR                       |
| Multi-column scientific PDFs        | Docling table & formula extraction         |
| `.pptx` / complex Office            | Docling Office pipeline                    |
| Audio (`.mp3` / `.wav` / `.m4a`)    | Docling Whisper-pipeline                   |
| Images (`.png` / `.jpg` / `.tiff`)  | Docling OCR                                |

This tier requires a `docling-serve` HTTP endpoint to be running
(host-managed, or via the `helios-code-graph` plugin's bundled
compose stack).

## Quickstart

```bash
# 0. Build
cargo build --release
BIN=$(realpath target/release/heliosdb-codekb-mcp)

# 1. Configure a KB for a source path AND do the first ingest in one shot
$BIN init \
    --source /home/me/my-repo --mode co-located \
    --ingest

# 2. Wire the MCP server into your agent.  For Claude Code (.mcp.json):
{
  "mcpServers": {
    "helios": {
      "command": "/abs/path/to/heliosdb-codekb-mcp",
      "args": [
        "serve", "--source", "/home/me/my-repo",
        "--mega-tool", "--wrapper-cache-size", "128"
      ]
    }
  }
}

# Or HTTP transport (Cursor / Continue / any non-stdio client):
$BIN serve --source /home/me/my-repo --http 127.0.0.1:8765

# 3. Inspect / sanity-check
$BIN status                                           # global summary
$BIN status --source /home/me/my-repo                 # per-KB
$BIN status --source /home/me/my-repo \
    --mcp-url http://127.0.0.1:8765                   # + live cache stats
$BIN config show                                      # raw TOML
```

## Ingest tiers

Three knobs, picked at `init --ingest` or `ingest` time, scaling
quality vs. wall time on the pilot corpus
(`~/Helios/Nano`, 666 files / 18 k symbols / 115 k refs):

| Tier                       | Flag                    | Pilot wall   | What lights up                                  |
|----------------------------|-------------------------|--------------|-------------------------------------------------|
| **fast** (default)         | *(none)*                | **26 s**     | BM25 + hop-distance ranking on `helios_graphrag_search` |
| **quality** (blocking)     | `--with-embeddings`     | 3 m 15 s     | Adds in-process FastEmbedder body vectors → paraphrase queries |
| **background-quality**     | `--background-quality`  | **26 s parent** + ~2 m 50 s detached child | User-wait stays at 26 s; paraphrase quality lifts when the child finishes |

Recommended for repos `>~1 000 files`: `--background-quality`.
Track via `status --source X` ("quality phase : running pid X" →
"complete — took Y").

## Resume on interrupt

`ingest` writes `<kb_dir>/.ingest-state.json` at each phase
transition (`walk → code_index → graph_rag`).  If a process is
killed mid-flight (Ctrl-C, OOM, reboot), the next `ingest` reads
the file at startup and skips already-completed phases — the
walk is skipped if `phase >= code_index`.  Per-file resume *inside*
`code_index` is the engine's content-hash gate.  Cleared on
successful completion; surfaced by `status --source X` as
`ingest resume : interrupted at phase = ...` until then.

## Generated-file skip

Two layers, both honoured during the walk:

- **`@generated` content-marker scan** of the first 4 KiB
  (Facebook / Google / Bazel / Go convention).
- **`<root>/.gitattributes` linguist-generated globs** —
  patterns flagged with `linguist-generated`,
  `linguist-generated=true`, or `linguist-generated=set` are
  matched against relative paths and skipped.  Long-tail
  coverage of generated files (`*.pb.rs`, `codegen/**`,
  vendored bundles) that don't carry an in-file marker.

## CLI surface

```text
heliosdb-codekb-mcp <SUBCOMMAND>

  init     [--source PATH] [--mode co-located|global|hybrid] [--kb PATH]
           [--ingest] [--include-binary-docs BOOL]
           [--force] [--durable-writes]
           [--with-embeddings] [--background-quality]

  ingest   [--source PATH] [--include-binary-docs BOOL]
           [--force] [--durable-writes]
           [--with-embeddings] [--background-quality]

  serve    [--source PATH] [--http <ADDR>]

  status   [--source PATH] [--mcp-url <URL>]

  config   show | set-default-mode <MODE> | path
```

## Why a separate binary

HeliosDB-Nano is a generic database; baking one specific MCP
transport into its binary would adapt the engine to one consumer.
This crate keeps that boundary clean: the engine stays generic and
publishable to crates.io for any downstream consumer; this binary
holds the MCP packaging, transport, and per-source KB-location
ergonomics.

## License

Apache-2.0.
