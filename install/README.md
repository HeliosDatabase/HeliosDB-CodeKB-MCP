# Agent install templates

These templates register a HeliosDB portfolio KB rooted at
`/home/app/Helios`.

## Claude Code

Copy `install/claude-code.mcp.json` to the portfolio root as
`/home/app/Helios/.mcp.json`, or merge the `helios-portfolio` server
block into an existing project MCP config.

## Codex

Merge `install/codex.config.toml` into `~/.codex/config.toml`.

Both templates use compact `helios(action, args)` mode and a wrapper
cache of 128 entries to reduce MCP tool-list tokens and repeated
wrapper calls.

For this portfolio-scale checkout, the first completed KB was generated
with `ingest --skip-code-graph --skip-linker`; full code-symbol and
reference graph materialisation is intentionally deferred until the
engine write path is tuned for the corpus size.
