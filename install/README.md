# Agent install templates

These templates register the Helios portfolio KB rooted at
`/home/app/Helios` through the local HTTP MCP daemon at
`http://127.0.0.1:8765/`.

## Binary install

```bash
install/install.sh
```

By default this installs the latest crates.io release. Set
`HELIOS_CODEKB_VERSION=v0.2.3` or another published tag to pin a GitHub
release asset when one exists; unsupported platforms install the matching
crates.io version.

## Claude Code

Copy `install/claude-code.mcp.json` to the portfolio root as
`/home/app/Helios/.mcp.json`, or merge the `helios-portfolio` server
block into an existing project MCP config.

## Codex

Merge `install/codex.config.toml` into `~/.codex/config.toml`.

Start the daemon first:

```bash
heliosdb-codekb-mcp serve --source /home/app/Helios \
  --http 127.0.0.1:8765 --wrapper-cache-size 128
```

The daemon defaults to compact `helios(action, args)` mode and keeps the
wrapper cache warm across Claude Code and Codex sessions.

For this portfolio-scale checkout, the first completed KB was generated
with `ingest --skip-code-graph --skip-linker`; full code-symbol and
reference graph materialisation is intentionally deferred until the
engine write path is tuned for the corpus size.
