# Token-savings bench: with vs. without the heliosdb-codekb MCP

This harness measures token usage and dollar cost for a set of typical
developer questions about a real repo, run twice:

- **WITH** — Claude Code + the `helios_*` MCP tools, with the source pre-indexed.
- **WITHOUT** — Claude Code with the same built-in tools (Read, Grep, Glob, Bash) but no MCP server attached.

Both runs use Claude Code's default auth + `--strict-mcp-config` (no `--bare`, because `--bare` skips OAuth/keychain reads and would require `ANTHROPIC_API_KEY`). The only intentional difference between setups is whether the MCP server is loaded.

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

- Per-turn breakdown of every tool call's tokens isn't exposed, but the JSON's top-level `usage` object DOES include `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens` per call — `compare.sh` aggregates these.

## Actual results (first run, 2026-05-20, Opus 4.7)

Recorded against `~/HDB/Full` (101 MB after rsync excludes; 6 683 code + 400 text + 662 markdown files; 178 180 symbols; 731 732 refs; 45 189 doc nodes; 12.25 M MENTIONS edges). Default model in the running shell: `claude-opus-4-7[1m]`. Budget cap: $0.25 / question.

| Metric | with MCP | without MCP | Δ |
|---|---:|---:|---:|
| Total $ cost (10 Q) | $2.35 | $2.05 | **+14.6 %** |
| Total wall (s) | 213 | 199 | +7.0 % |
| Cache-read tokens | 1.16 M | 0.92 M | +26.0 % |
| Questions that **completed** | 5 / 10 | 9 / 10 | |
| Questions that **hit budget cap with empty result** | **5 / 10** | 1 / 10 | |

**WITH MCP was net-negative on this profile.** The MCP path made the agent take 4-7 turns instead of 2-3, with each turn cache-reading ~96 k tokens of `helios_*` tool output. Five WITH runs ran out of budget before producing an answer.

Honest follow-ups not yet measured:

- Run with `MODEL=claude-haiku-4-5-20251001` — the per-token economics change drastically for a small model on small lookups; MCP may pay back there.
- Use HTTP-mode `serve` so the engine stays warm across calls instead of cold-starting each stdio invocation.
- Append a system prompt steering the agent to prefer `helios_graphrag_search` over `Read + Grep` for this corpus.
- Trim the `helios_lsp_*` response bodies — neighbouring-symbol context is currently always included.
- One-shot runs only — no multi-turn dialogues. Some MCP value (cached subsequent calls) only shows over multi-turn workflows. Add a follow-up bench for that if you care.
- The WITHOUT side will likely use `Bash(grep …)` and `Read` heavily. That's the realistic baseline a no-MCP agent has.
