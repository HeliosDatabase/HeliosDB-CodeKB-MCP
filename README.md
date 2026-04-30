# heliosdb-codekb-mcp

MCP stdio server for code+docs knowledge bases.  Embeds
[HeliosDB-Nano](../Nano) as a Rust library (`code-graph`,
`graph-rag`, `mcp-endpoint`, `code-embed` features) and exposes its
LSP-shaped + GraphRAG tools to Claude Code, Cursor, Codex, Aider,
and any other MCP-aware agent — over plain stdio JSON-RPC, no
ports, no auth dance, all local.

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
      "args": ["serve", "--source", "/home/me/my-repo"]
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
