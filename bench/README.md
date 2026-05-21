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

## Actual results

Corpus across both runs: `~/HDB/Full` after rsync excludes — 101 MB on disk, 6 683 code + 400 text + 662 markdown files, 178 180 symbols, 731 732 refs, 45 189 doc graph nodes, 12.25 M MENTIONS edges. Same KB used by both model runs. Engine: heliosdb-nano 3.31.1 + the T1 fix (PR #3 on the engine repo). Budget cap: $0.25 / question. Same 10 questions from `questions.txt`.

### Aggregate

| Model | Setup | Total $ | Total wall | Completed | Cache-read tokens |
|---|---|---:|---:|---:|---:|
| Opus 4.7 (1M ctx) | with MCP | $2.35 | 213 s | **5 / 10** ⚠ | 1.16 M |
| Opus 4.7 (1M ctx) | no MCP | $2.05 | 199 s | 9 / 10 | 0.92 M |
| Haiku 4.5 | with MCP | $0.77 | 366 s | 10 / 10 | 2.38 M |
| Haiku 4.5 | no MCP | $0.52 | 227 s | 10 / 10 | 1.90 M |

- Opus + MCP hit the $0.25 budget cap **on 5 of 10 questions, returning no answer**. Net-negative on every metric (+14.6 % cost / +7 % wall / +26 % cache reads).
- Haiku completed every question in both modes but **MCP added +49 % cost / +61 % wall** on average — the cache-read token volume per MCP tool call eats the model's lower per-token price.

### Where MCP actually helped (Haiku per-question)

| Q | WITH cost | no-MCP cost | WITH turns | no-MCP turns | Verdict |
|---|---:|---:|---:|---:|---|
| q02 (WASM plugin docs) | $0.043 | $0.062 | 6 | 10 | **MCP −31 %** ✓ |
| q08 (cache vs storage crates) | $0.030 | $0.058 | 4 | 9 | **MCP −48 %** ✓ |
| q06 (default WAL sync mode) | $0.022 | $0.023 | 2 | 2 | tie |
| q01 (FK validation location) | $0.246 | $0.119 | 3 | **22** | no-MCP cheaper but burned 22 turns of grep + read |

### Where MCP lost badly (Haiku per-question)

| Q | WITH | no-MCP | Failure mode |
|---|---:|---:|---|
| q05 (multi-tenancy module) | $0.085 | $0.039 | `helios_graphrag_search` returned 460 k cache-read tokens of subgraph |
| q07 (HIGH_AVAILABILITY_OVERVIEW.md) | $0.070 | $0.036 | similar — large doc subgraph |
| q10 (heliosdb-vector public types) | $0.074 | $0.030 | `helios_lsp_*` returned every type + signature blob |

### Pattern across both models

- Doc questions where the answer is one section → MCP wins (smaller chunk returned vs reading whole file).
- Code-location questions where one symbol answers it → MCP loses (LSP returns surrounding context the agent doesn't need).
- Conceptual / multi-symbol questions → MCP loses big (graph traversal returns large subgraph the agent has to digest).
- Questions the agent already half-knows from common patterns → no-MCP wins (Read + Grep is cheap when targeted).

### Honest verdict

The infrastructure works end-to-end (T1 unblocked ingest, plugin compiles, tests green, bench is repeatable, both models produce real numbers). **The plugin is not yet a token saver in its current shape.** The MCP tool *result sizes* are tuned for richness, not minimality. Until that's tightened, Read + Grep beats it on a typical Q&A workflow with both Opus and Haiku.

### Follow-up hypotheses not yet measured

- **Trim the `helios_lsp_*` response bodies** — neighbouring-symbol context is currently always included; an `include_context: bool` argument defaulting to false would let the agent ask for verbosity explicitly. Biggest expected impact.
- **HTTP-mode `serve`** so the engine stays warm across calls instead of cold-starting per stdio invocation. May matter less than tool-result trimming.
- **`--append-system-prompt`** steering the agent to prefer `helios_graphrag_search` for doc questions and `Read + Grep` for known-symbol lookups. Currently the agent uses MCP enthusiastically and pays for it.
- **Re-run with a small-doc-heavy corpus** (e.g. a Python project with many `.md` files) — DocSection heading chunking should shine there. Full's signal is code-dominated.
- One-shot runs only — no multi-turn dialogues. Some MCP value (cached subsequent calls) only shows over multi-turn workflows. Add a follow-up bench for that if you care.
- The WITHOUT side will likely use `Bash(grep …)` and `Read` heavily. That's the realistic baseline a no-MCP agent has.
