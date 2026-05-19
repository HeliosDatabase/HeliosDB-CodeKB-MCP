---
description: Re-index the current project's source tree into its KB. Use this after pulling changes, adding new files, or to recover from an interrupted ingest.
---

The plugin only ingests when explicitly told to — it does not auto-re-index on every Claude Code session (that would be expensive). Run this command after material changes.

## Default — resume / incremental

```bash
heliosdb-codekb-mcp ingest --source "${CLAUDE_PROJECT_DIR}"
```

Uses the engine's content-hash gate: only files whose `_hdb_code_files.content_hash` differs from the on-disk hash are re-parsed. Typically finishes in seconds on a previously-indexed repo.

If a prior run was interrupted, this also resumes — the plugin reads `<kb>/.ingest-state.json` and skips already-completed phases (walk → code_index → graph_rag).

## Full re-ingest

If the user reports stale results (e.g. they did a big rename and the KB still shows old symbols) or wants to switch embedding modes, force-reparse:

```bash
heliosdb-codekb-mcp ingest --source "${CLAUDE_PROJECT_DIR}" --force
```

## Switching embedding modes

Re-run with the desired flag. The new mode only takes effect for re-parsed files, so combine with `--force` to recompute embeddings across the whole corpus:

- Add embeddings: `--with-embeddings --force` (blocking ~3 min on a typical repo) or `--background-quality --force` (returns immediately, child finishes ~3 min later).
- Drop embeddings: omit both flags + `--force` to re-parse without them. Existing `body_vec` values remain in the DB but are ignored by ranking that doesn't request them.

## After ingest completes

Tell the user the new symbol/ref/doc counts from the printed summary so they have a sense of growth (e.g. "+1240 symbols, +18 markdown sections since last ingest").
