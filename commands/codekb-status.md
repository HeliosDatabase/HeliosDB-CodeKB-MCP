---
description: Show the KB state for the current project — config, ingest state, background quality phase, and (optionally) live MCP cache stats.
---

Run:

```bash
heliosdb-codekb-mcp status --source "${CLAUDE_PROJECT_DIR}"
```

Interpret the output for the user:

- **`kb-on-disk : exists ({bytes} bytes top-level)`** — KB is present. Bytes is just the top-level directory size, not the full on-disk footprint.
- **`ingest resume : interrupted at phase = …`** — a prior `ingest` was killed (Ctrl-C / OOM / reboot) before finishing. Tell the user to re-run `/codekb-ingest` to resume.
- **`quality phase : running — pid X (Y elapsed)`** — the background-embeddings child is still running. Paraphrase quality is improving; queries already work.
- **`quality phase : complete — took Y, finished Z ago`** — embeddings done. Paraphrase queries are at full quality.
- **`quality phase : stale — pid X not running and no completion recorded`** — the child died unexpectedly. Suggest re-running with `--background-quality` (or tailing the log file the line names).

If the MCP server is running in HTTP mode (the user has `serve --http <addr>` going somewhere), append `--mcp-url http://<addr>` to also print live cache stats:

```
mcp cache : 47 / 256 entries, 73.4% hit rate (188 hit / 68 miss), gen 0
```

A high hit rate means subsequent `helios_*` calls with the same args are served from the engine's per-process LRU — direct token savings on repeat queries.
