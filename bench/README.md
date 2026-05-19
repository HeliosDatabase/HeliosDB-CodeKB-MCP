# Token-savings bench: with vs. without the heliosdb-codekb MCP

This harness measures token usage and dollar cost for a set of typical
developer questions about a real repo, run twice:

- **WITH** — Claude Code + the `helios_*` MCP tools, with the source pre-indexed.
- **WITHOUT** — Claude Code with the same built-in tools (Read, Grep, Glob, Bash) but no MCP server attached.

Both runs use `--bare` (skip hooks / LSP / auto-memory) + `--strict-mcp-config` so the only intentional difference is the presence of the MCP server.

## Gate — DO NOT RUN until engine T1 lands

> The engine has a known FK-validation regression introduced in heliosdb-nano
> v3.28.0 that makes the `code_index` ingest ~338× slower on repos with FK-bearing
> code-graph tables (~93 min instead of ~45 s on a 700-file repo). Running the
> bench WITH that regression in effect would (a) take hours of wall time on
> setup alone and (b) skew the comparison — the MCP path's value is in the
> per-query response, not the one-time index cost, but the index cost is currently
> so large that the comparison's intent gets lost.
>
> Wait for engine **T1 (in-txn ART overlay)** to land — tracked at
> `Dimensigon/HDB-HeliosDB-Nano:PROPOSAL_FK_VALIDATION_OPTIMIZATION.md`. Once
> v3.31.2+ is on crates.io, bump the plugin's lockfile (`cargo update -p
> heliosdb-nano`) and the bench is safe to run.
>
> The bench scripts intentionally refuse to run until you set
> `BENCH_T1_LANDED=1` in the environment, so you can't accidentally
> kick it off.

## Cost warning

Each question is a real `claude -p` call against the API. With ~10 questions × 2 setups = ~20 calls. Each call is capped at `--max-budget-usd 0.25`, so worst-case spend is **$5**. Typical case will be well under that.

## Files

| File | Purpose |
|---|---|
| `setup.sh` | Copies `~/HDB/Full` into two sibling temp dirs and indexes the WITH copy via `heliosdb-codekb-mcp init --ingest`. |
| `questions.txt` | One developer question per line. Mix of code-discovery, doc-retrieval, and cross-modal. Edit freely. |
| `mcp-on.json.tmpl` | MCP config template for the WITH run. `setup.sh` materialises it with the chosen `--source` path. |
| `mcp-off.json` | Empty MCP config for the WITHOUT run. |
| `run.sh` | For each question × each setup, runs `claude -p` and saves the JSON output under `results/`. |
| `compare.sh` | Aggregates `total_cost_usd` (and any usage fields present) across results and emits a markdown comparison table to stdout. |

## How to run (once T1 ships and `BENCH_T1_LANDED=1`)

```bash
cd /home/gpc/HDB/heliosdb-codekb-mcp
export BENCH_T1_LANDED=1
bench/setup.sh                                  # ~minutes — duplicates source + indexes
bench/run.sh                                    # ~minutes — fires the question matrix
bench/compare.sh > bench/results/SUMMARY.md     # prints comparison
```

## What "fair comparison" means here

- **Same source corpus.** Both setups have access to identical files at
  `${TMP}/full-with` and `${TMP}/full-without` (full copies, not symlinks —
  avoids any shared inode behaviour from the index dir).
- **Same agent flags** except the MCP config. Both runs get `--bare`,
  `--no-session-persistence`, `--max-budget-usd 0.25`, `--add-dir <its-source>`.
- **Same model.** Whatever the local default is. To pin: edit `run.sh` to add `--model claude-sonnet-4-6` (or whatever you're standardising on).
- **Same questions.** From `questions.txt`. No retries.

## What the comparison shows

`compare.sh` emits a markdown table per question with:

- Cost (`total_cost_usd`) — direct dollar comparison.
- Duration — wall time of each call.
- Response length — character count of the `result` field; rough proxy for output verbosity.

The text responses live under `results/{with,without}/q{NN}.json` for side-by-side qualitative review.

**The bench measures cost, not answer quality.** Cost savings only matter if the answers are at least as good. After running, eyeball a few response pairs to confirm the MCP-equipped agent is still answering correctly. If MCP responses are shorter but wrong, that's a regression, not a win.

## Limitations

- Per-turn input/output/cache token breakdown isn't exposed by `claude -p --output-format json` today (only `total_cost_usd`). The dollar figure is the proxy for total token usage.
- One-shot runs only — no multi-turn dialogues. Some MCP value (cached subsequent calls) only shows over multi-turn workflows. Add a follow-up bench for that if you care.
- The WITHOUT side will likely use `Bash(grep …)` and `Read` heavily. That's the realistic baseline a no-MCP agent has.
