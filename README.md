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

# 1. Configure a KB for a source path
target/release/heliosdb-codekb-mcp init --source /home/me/my-repo --mode co-located
target/release/heliosdb-codekb-mcp init --source /home/me/Helios  --mode hybrid --kb /home/me/Helios/.helios-kb

# 2. Wire the MCP server into your agent.  For Claude Code:
#    .mcp.json
{
  "mcpServers": {
    "helios": {
      "command": "/abs/path/to/heliosdb-codekb-mcp",
      "args": ["serve", "--source", "${workspaceFolder}"]
    }
  }
}

# 3. Inspect / sanity-check
target/release/heliosdb-codekb-mcp status
target/release/heliosdb-codekb-mcp status --source /home/me/my-repo
target/release/heliosdb-codekb-mcp config show
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
