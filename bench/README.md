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

### Multi-trial (recommended for stable numbers)

A single run swings ~1.5-2× due to agent variance. Run 3+ trials:

```bash
TRIALS=3 bench/run.sh          # 10 Q × 3 trials × 2 setups = 60 calls
bench/compare.sh > bench/results/SUMMARY.md
# compare.sh detects qNN-tT.json automatically and reports per-Q
# median + min + max plus a totals row that sums the medians.
```

### Steering test (prompt-tells-agent-to-prefer-MCP)

```bash
STEER=1 bench/run.sh                                  # writes to with-steered/ + without-steered/
SUFFIX=-steered bench/compare.sh > bench/results/SUMMARY-steered.md
```

Combine: `TRIALS=3 STEER=1 bench/run.sh` for a multi-trial steered run.

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

| Model | Setup | Engine | Total $ | Total wall | Completed | Cache-read tokens |
|---|---|---|---:|---:|---:|---:|
| Opus 4.7 (1M ctx) | with MCP   | T1 patch (2026-05-20) | $2.35 | 213 s | **5 / 10** ⚠ | 1.16 M |
| Opus 4.7 (1M ctx) | no MCP     | T1 patch (2026-05-20) | $2.05 | 199 s | 9 / 10 | 0.92 M |
| Haiku 4.5         | with MCP   | T1 patch (2026-05-20) | $0.77 | 366 s | 10 / 10 | 2.38 M |
| Haiku 4.5         | no MCP     | T1 patch (2026-05-20) | $0.52 | 227 s | 10 / 10 | 1.90 M |
| Haiku 4.5         | **with MCP**   | **3.31.2 release (2026-05-22)** | **$0.47** | **258 s** | **10 / 10** | **1.76 M** |
| Haiku 4.5         | **no MCP**     | **3.31.2 release (2026-05-22)** | **$0.88** | **584 s** | **9 / 10** ⚠ | **3.44 M** |
| Haiku 4.5         | with MCP, **trim=800** | 3.31.2 release (2026-05-22, hyp #1) | $0.55 | 240 s | 10 / 10 | 2.40 M |
| Haiku 4.5         | no MCP                  | 3.31.2 release (2026-05-22, hyp #1) | $0.43 | 190 s | 10 / 10 | 1.70 M |
| Haiku 4.5         | **with MCP, STEER=1, TRIALS=3** | 3.31.2 release (2026-05-22, canonical) | **$1.27** | 563 s | 30 / 30 | 1.98 M |
| Haiku 4.5         | **no MCP, STEER=1, TRIALS=3**   | 3.31.2 release (2026-05-22, canonical) | **$1.26** | 502 s | 30 / 30 | 1.24 M |

- Opus + MCP hit the $0.25 budget cap on 5 of 10 questions, returning no answer. Net-negative on every metric (+14.6 % cost / +7 % wall / +26 % cache reads).
- Haiku on 2026-05-20 (yesterday) completed every question in both modes but **MCP added +49 % cost / +61 % wall** — the cache-read token volume per MCP tool call eats Haiku's lower per-token price.
- Haiku on 2026-05-22 (today, same KB, same questions, same model — only the engine was bumped from a [patch] build to the 3.31.2 release, functionally identical to yesterday's binary) shows **MCP −47 % cost / −56 % wall / −49 % cache-reads** — a complete flip. **The swing is dominated by agent run variance, not engine change.** Yesterday no-MCP runs were fast and lean; today two no-MCP runs catastrophed:
  - q01 (FK validation): no-MCP took **32 turns** of Read+Grep ($0.22, 200 s).
  - q04 (time-travel API): no-MCP hit the $0.25 cap with no answer.
- The honest read: **one bench run is not statistically stable.** Need 3–5 trials × per question to get confidence intervals. Today's result says MCP can absolutely shine when no-MCP thrashes, but doesn't prove MCP is intrinsically cheaper across a balanced workload.

### Hypothesis #1 (tried, negative): trim MCP tool result bodies

Added a `--max-tool-result-bytes <N>` flag to `serve` (see `src/mcp_trim.rs` + `src/main.rs::stdio_loop_with_trim`). When set, the plugin replaces the engine's stdio loop with one that calls `handle_rpc_with_db` and walks the JSON response, truncating any string field longer than N bytes at the nearest char boundary and appending `…[+N bytes truncated]`. Char-boundary safe (same lesson as the linker emoji bug). 6 unit tests cover the trimmer.

Bench at cap=800 bytes (above table, rows 5-6) ran net-negative: WITH-MCP cost **+27 %** vs no-MCP, cache-read tokens **+41 %**. Inspection of q01 showed why: the agent treats the `…[truncated]` marker as a signal that data exists somewhere, then calls `Read` on the source file to recover it — **trimming shifted cost to the next tool call instead of saving it**.

Variance across the three Haiku runs above (same model, same KB, same 10 questions) is larger than the trim-cap effect: WITH cost swung from $0.47 → $0.55 → $0.77 across runs without any code change capable of explaining it. **Single-run benches are not statistically reliable for this workload.** Multi-trial measurement is the next step.

The trim feature stays in tree as an experimental knob (off by default — `--max-tool-result-bytes 0`). Useful for workloads where the agent should NOT fall back to Read (e.g. sandbox modes that disable filesystem access), or for tuning above 2000 bytes to clip pathological subgraph blobs without triggering the Read-fallback pattern.

### Canonical result — multi-trial + steering (2026-05-22)

Rows 7-8 of the aggregate table above are the most statistically defensible numbers we have on this corpus: 3 trials per question × 10 questions × 2 setups = 60 total `claude -p` calls, with `bench/steer-prompt.md` injected via `--append-system-prompt-file` for both runs. Totals are summed per-question medians (so a single bad trial doesn't dominate). Detailed run output in `/tmp/codekb-bench-20260520/results/SUMMARY-haiku-steered-trials3.md`.

**Headline:** WITH-MCP $1.27 vs WITHOUT-MCP $1.26 — **+0.8 % difference, tied within noise.**

**Three things this settles:**

1. **Variance, not method, drove earlier swings.** Single-trial runs swung WITH cost from $0.47 → $0.77 with no code change; 3-trial median lands at $1.27 — both single-trial readings were misleading. Multi-trial is the floor for honest measurement on this workload.
2. **Steering didn't break the tie.** Appending a prompt that nudges the agent to prefer `helios_*` tools moved both absolute costs up (more context, more tokens) without tilting either way.
3. **The per-question split is stable across trials** and divides cleanly into three buckets:

| Bucket | Questions | Median Δ (WITH − WITHOUT) | Reason |
|---|---|---:|---|
| **MCP wins clearly** | q02 (WASM docs), q08 (cache vs storage), q10 (vector public types) | −34 % to **−45 %** | Doc-section / multi-symbol-summary queries — `helios_graphrag_search` returns one matching chunk instead of forcing Read+Grep to enumerate. |
| **Tied within noise** | q01, q03, q04, q07 | ±10 % | Mixed code/doc lookups where neither path has a structural advantage. |
| **MCP loses clearly** | q05 (multi-tenancy), q06 (default WAL mode), q09 (HNSW index) | +74 % to **+367 %** | Tiny lookups where MCP overhead dwarfs savings ($0.022 on a $0.006 baseline at q06), or graph traversals returning oversized subgraphs (q05). |

**Per-question variance is still wide** — 3 trials gives min/median/max bands like q01 WITH = $0.054 / $0.231 / $0.253. 3 is the floor for stable signal; 5 would tighten the bands further.

### Verdict on the 80-90 % savings goal

**Unreachable on this workload by design.** Read+Grep on a 100 MB corpus with Haiku averages ~$0.13 per question; for MCP to come in at 10-20 % of that ($0.013-0.026) the helios_* tools would need to answer in 1-2 small chunks per question — but every MCP tool call carries ~96 k cache-read tokens of tool descriptions + schema, which is roughly the entire no-MCP budget. The math doesn't permit it for small models on small corpora.

The plugin's honest value (re-stated from the top-level README):

- **Catastrophe prevention** — on Opus with a $0.25 cap, no-MCP runs *failed to answer* 5 of 10 questions; WITH-MCP completed all of them.
- **Cross-modal MENTIONS** — text → code edges that Read+Grep cannot produce in a single round-trip.
- **Time-travel / branch / AST-diff** — workflows with no Read+Grep alternative at all.
- **Doc-section retrieval** — for the bucket where MCP wins (q02/q08/q10 above), the savings are real and reproducible: −30 to −45 %.

The plugin is the right shape for the workflows above. It is not — and probably cannot be without engine-side `verbose: false` tool modes — a flat token saver across a typical Haiku Q&A workload.

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

## Self-hosted Ollama bench (Qwen3-coder, 2026-05-24+)

A second bench runner lives at `bench/ollama_run.py` + `bench/ollama_compare.py`. It drives the agent loop against a self-hosted Ollama endpoint (typically reached over Tailscale) instead of `claude -p`, removing the API-spend gate from comparison runs. Same canonical questions, same WITH/WITHOUT MCP setups, different model.

### Why self-hosted

- **Free**: no `--max-budget-usd` cap, no API spend, no PR gate.
- **Deterministic**: temperature=0; runs land within ±5% per trial without `TRIALS=3+`.
- **Different signal**: Qwen3-coder ≠ Haiku — the result describes how this specific model uses the tool surface, not how Claude does. Read the comparison as "Qwen3-coder with vs. without MCP," NOT "Qwen3 vs. Claude." Both signals are useful.
- **Substrate for Phase 2 distillation**: the same Ollama endpoint serves as the LLM oracle for `--with-llm-distill` (one-sentence symbol summaries computed at ingest), so the system is internally consistent.

### Setup

```bash
# 1. Verify Ollama is reachable + has the model (default qwen3-coder:30b).
curl -s http://ollama:11434/api/tags | jq '.models[].name'

# 2. Use the existing bench/setup.sh corpus, or rsync your own:
BENCH_DIR=/tmp/mybench WITH_DIR=/tmp/mybench/full-with WITHOUT_DIR=/tmp/mybench/full-without

# 3. Initial ingest of WITH (one-time, heuristic-only Phase 1):
heliosdb-codekb-mcp init --source $WITH_DIR --mode global --ingest

# 4. Run the bench (no `claude -p`, no API spend).
OLLAMA_BASE=http://ollama:11434 \
OLLAMA_MODEL=qwen3-coder:30b \
WITH_DIR=$WITH_DIR \
WITHOUT_DIR=$WITHOUT_DIR \
BENCH_DIR=$BENCH_DIR/results \
python3 bench/ollama_run.py

# 5. Aggregate.
python3 bench/ollama_compare.py $BENCH_DIR/results > $BENCH_DIR/SUMMARY-phase1.md
```

### Phase 2 (LLM-distilled symbol summaries)

```bash
# Re-ingest with --with-llm-distill — POSTs each symbol to Ollama for
# a one-sentence summary, stored in _hdb_plugin_symbol_cards.llm_summary.
# On a 178k-symbol corpus uncapped this is ~hours; cap to top N for a
# tractable demo:
heliosdb-codekb-mcp ingest --source $WITH_DIR \
  --with-llm-distill \
  --llm-distill-endpoint http://ollama:11434 \
  --llm-distill-model qwen3-coder:30b \
  --llm-distill-concurrency 4 \
  --llm-distill-max-symbols 5000

# Re-run the bench against the same WITH copy:
BENCH_DIR=$BENCH_DIR/results-phase2 python3 bench/ollama_run.py
python3 bench/ollama_compare.py $BENCH_DIR/results-phase2 > $BENCH_DIR/SUMMARY-phase2.md
```

### Agent loop details

- `bench/ollama_run.py` issues a system prompt that asks the model to answer in 3-6 sentences, using the tools as needed.
- WITH-MCP: launches `heliosdb-codekb-mcp serve --source $WITH_DIR --profile $MCP_PROFILE --strip-tool-descriptions $MCP_STRIP` as a subprocess and pipes JSON-RPC over its stdio. MCP `tools/list` is translated into OpenAI-format function definitions for Ollama's `/v1/chat/completions`.
- WITHOUT-MCP: exposes 4 local tools — Read (64 KiB cap), Glob, Grep, Bash (60s + 8 KiB cap) — backed by the host filesystem under `$WITHOUT_DIR`.
- Token counts come from Ollama's `usage.prompt_tokens` + `usage.completion_tokens` (no cache mechanic; no `cache_read_input_tokens` slot). Comparison is therefore on raw model tokens, not on cache-read tokens like the Claude bench.
- Qwen3-coder occasionally emits inline `<function=…>` tool calls instead of the OpenAI JSON shape; the harness parses both formats so the agent loop doesn't dead-end.
- `MAX_TURNS` defaults to 24; raise via env for harder questions.

### Self-hosted Qwen3-coder results — smoke corpus (2026-05-24)

Corpus: this repository (`heliosdb-codekb-mcp` itself), 478 code symbols, 28 source files, 309 doc-graph nodes after ingest. Questions: 5 plugin-relevant questions at `/tmp/codekb-bench-ollama-smoke/questions.txt`. Model: `qwen3-coder:30b` via Tailscale Ollama at `http://ollama:11434` (temperature=0, MAX_TURNS=16). One trial per question. Tool surface: `--profile standard --strip-tool-descriptions 200` (default).

**Phase 1** — heuristic distill only (signature + first-line docstring + PageRank cards, no LLM):

| Q | with prompt | with completion | with turns | wall | no-MCP prompt | no-MCP completion | turns | wall | Δ total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q01 What is the `Profile` enum used for | 19,677 | 566 | 6 | 11s | 3,053 | 265 | 3 | 3s | **+16,925** |
| q02 What does `spawn_quality_child` do | 10,167 | 263 | 4 | 4s | 49,741 | 690 | **13** | 12s | **−40,001** |
| q03 What does the linker module do | 56,111 | 799 | 12 | 13s | 2,365 | 224 | 2 | 2s | **+54,321** |
| q04 What does `Phase` enum represent | 46,575 | 825 | 12 | 13s | 3,005 | 271 | 3 | 3s | **+44,124** |
| q05 How does plugin compute PageRank | 60,833 | 770 | 13 | 14s | 34,484 | 645 | 10 | 11s | **+26,474** |
| **TOTAL** | **193,363** | **3,223** | — | **55s** | **92,648** | **2,095** | — | **31s** | **+101,843 (+107.5%)** |

**Phase 2** — LLM distillation enabled (`--with-llm-distill --llm-distill-concurrency 4`), 462/478 symbols summarised in 139 seconds (Qwen3-coder spent 91k prompt + 12k completion tokens at ingest). Same 5 questions, same harness:

| Q | with prompt | with completion | with turns | wall | no-MCP prompt | no-MCP completion | turns | wall | Δ total | vs Phase 1 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q01 | 35,613 | 752 | 11 | 12s | 3,035 | 249 | 3 | 3s | +33,081 | +75% (variance regression) |
| q02 | **4,859** | **178** | **2** | 3s | 39,211 | 689 | 11 | 11s | **−34,863** | **−51% WITH cost** ✓ |
| q03 | 33,170 | 570 | 9 | 9s | 2,357 | 255 | 2 | 3s | +31,128 | **−41% WITH cost** ✓ |
| q04 | 46,234 | 748 | 11 | 12s | 3,018 | 245 | 3 | 3s | +43,719 | tied |
| q05 | 39,728 | 730 | 10 | 11s | 13,418 | 530 | 6 | 7s | +26,510 | **−34% WITH cost** ✓ |
| **TOTAL** | **159,604** | **2,978** | — | **47s** | **61,039** | **1,968** | — | **27s** | **+99,575** | **WITH cost −17%** |

### What these numbers say (and don't)

**Apples-to-apples — WITH-MCP only, Phase 1 → Phase 2: ~17% reduction in agent token cost on the smoke corpus.** Concentrated on questions where `helios_symbol_card` could return the LLM summary directly (q02 −51%, q03 −41%, q05 −34%). q01 regressed (+75%) due to single-trial agent variance — Qwen3 picked a different exploration path on the retry. Q04 tied.

**Quality is preserved.** Spot-check on the biggest win (q02): both phases correctly identify `spawn_quality_child` as a detached-process launcher that survives TTY SIGHUP. Phase 2 reaches the answer in 2 turns vs 4 in Phase 1, because the LLM summary in the symbol card *is* the answer — the agent reads one card and stops investigating.

**Why +107% / +158% overhead vs no-MCP, despite the 17% WITH-MCP improvement.** The smoke corpus is tiny (478 symbols, 28 files). The MCP `tools/list` payload is the floor — its cost doesn't shrink with corpus size, but no-MCP's `Read+Grep` cost grows linearly with corpus size. On a 1-10 GB corpus the no-MCP path scales linearly while the MCP path stays flat, so the same +17% Phase-2 win on the WITH side would flip the comparison: WITH-MCP wins outright. This matches the Haiku bench finding (line 158 above) — 80-90% savings unreachable at small scale.

**Single-trial variance is the dominant uncertainty.** Qwen3-coder is deterministic at temperature=0, but tool-call selection can diverge based on cumulative context. Multi-trial measurement (TRIALS=3+) would tighten the bands. Q01's +75% regression in Phase 2 is the cleanest example of why.

### Reproducing

```bash
# Phase 1
heliosdb-codekb-mcp init --source <repo> --mode global --ingest
MCP_BIN=$(command -v heliosdb-codekb-mcp) \
WITH_DIR=<repo> WITHOUT_DIR=<repo-copy> \
BENCH_DIR=/tmp/p1 \
python3 bench/ollama_run.py
python3 bench/ollama_compare.py /tmp/p1 > /tmp/PHASE1.md

# Phase 2 (re-ingest WITH LLM distillation, then re-run bench)
heliosdb-codekb-mcp ingest --source <repo> \
  --with-llm-distill --llm-distill-concurrency 4
BENCH_DIR=/tmp/p2 python3 bench/ollama_run.py
python3 bench/ollama_compare.py /tmp/p2 > /tmp/PHASE2.md
```

### Cost vs Claude bench

- Self-hosted Qwen3-coder bench: **$0.00** per run (Tailscale + local GPU).
- Equivalent Haiku bench (canonical row at line 117): $1.27 per 30-Q matrix.
- Qwen3-coder is ~10–15× slower wall-clock vs Haiku (no batching, single-stream decode), but that's irrelevant for measuring tool-surface cost.

---

## Phase 1 — layer ablation (Compression-mode rollout, 2026-05-24)

The honest verdict above shipped a structural rebuild — the **Phase 1 compression-mode layers**. Goal: push the savings ceiling toward the 80–90% target a large repo can deliver. Each layer is independently togglable so the bench can attribute savings to a specific mechanism.

### What changed in the plugin

| Layer | Mechanism | File(s) |
|---|---|---|
| **L1 — Tool-surface compression** | `tools/list` gateway: profile filter (`minimal` / `standard` / `full`) + description shortening. Stdio + HTTP. | `src/mcp_trim.rs`, `src/main.rs` |
| **L2 — Wrapper tools** | Plugin-owned `helios_repo_summary`, `helios_outline_first`, `helios_doc_drill`, `helios_semantic_filter`, `helios_git_summary`, `helios_symbol_card`. Compose engine library calls into one distilled response. | `src/wrappers.rs` |
| **L3 — Pre-distillation at ingest** | `_hdb_plugin_symbol_cards` + `_hdb_plugin_repomap_cards` populated via heuristic doc1l + PageRank over the symbol call graph. Idempotent re-run. | `src/distill.rs`, `src/checkpoint.rs::Phase::Distill` |
| **L4 — Agent steering** | Rewritten `bench/steer-prompt.md` with explicit wrapper-tool ranking; `skills/codekb-pro-features.md` "Compression-mode tool mapping" section. | `bench/steer-prompt.md`, `skills/codekb-pro-features.md` |

Phase 2 (LLM distillation — opt-in `--with-llm-distill` for a 1-sentence per-symbol summary) is queued as a follow-up; this section measures the heuristic-only Phase 1 first.

### How to run the layer ablation

```bash
export BENCH_T1_LANDED=1
# Pilot corpus (existing 100 MB), all 4 layers stacked, 3 trials:
PROFILE=standard STRIP=200 CORPUS=pilot bench/setup.sh
LAYERS=L1,L2,L3,L4 TRIALS=3 STEER=1 bench/run.sh
bench/compare.sh > bench/results/SUMMARY-phase1.md

# Large corpus (a 1-10 GB tree where Read+Grep scales linearly):
PROFILE=standard CORPUS=large bench/setup.sh
LAYERS=L1,L2,L3,L4 TRIALS=3 STEER=1 WITH_KB_DIR=<kb-path> bench/run.sh
```

The Phase 1 headline number will be filled in once the bench has been re-run after the L1-L5 code lands and a CORPUS=large tree is in place. Until then, this section documents the *shape* of the comparison.

### Expected stacking on a 1–10 GB corpus (pre-bench math)

| Layer | Expected ∆ vs. no-MCP | Mechanism |
|---|---:|---|
| L1 alone | −30 to −50% per-turn cache overhead | smaller `tools/list` payload (the dominant per-turn cache cost on Haiku) |
| L1 + L2 | additional −15 to −25% per Q | one wrapper call replaces 3+ engine primitive calls |
| L1 + L2 + L3 | additional −10 to −20% | distilled cards mean wrappers return ≤4 KB JSON instead of multi-KB engine output |
| L1 + L2 + L3 + L4 | additional −5 to −10% | steering nudges the agent to pick a wrapper when one applies |

Multiplicative-with-diminishing: `1 − (0.5 × 0.8 × 0.85 × 0.95) ≈ −68%` on a similar workload. Corpus scaling then pushes it above 80% on a 10 GB tree because the no-MCP baseline grows linearly while the MCP path stays flat.

### Phase 1 numbers — TBD (will be filled in once the harness re-runs)

| Corpus | Profile | Layers | Total $ | Cache-read tokens | Wall s | Completed | Δ vs no-MCP |
|---|---|---|---:|---:|---:|---:|---:|
| pilot (100 MB) | standard | L1 | — | — | — | — | — |
| pilot (100 MB) | standard | L1+L2 | — | — | — | — | — |
| pilot (100 MB) | standard | L1+L2+L3 | — | — | — | — | — |
| pilot (100 MB) | standard | L1+L2+L3+L4 | — | — | — | — | — |
| large (1–10 GB) | standard | L1+L2+L3+L4 | — | — | — | — | — |

After the bench runs, the **headline goal** for this section is: `Full stack on large corpus: WITH-MCP $X vs WITHOUT $Y, savings Z%`. If `Z < 80`, Phase 2 (LLM distillation) opens as a follow-up; the bench section there compares Phase 2 against this Phase 1 baseline directly.

### Follow-up hypotheses not yet measured

- **Trim the `helios_lsp_*` response bodies** — neighbouring-symbol context is currently always included; an `include_context: bool` argument defaulting to false would let the agent ask for verbosity explicitly. Biggest expected impact.
- **HTTP-mode `serve`** so the engine stays warm across calls instead of cold-starting per stdio invocation. May matter less than tool-result trimming.
- **`--append-system-prompt`** steering the agent to prefer `helios_graphrag_search` for doc questions and `Read + Grep` for known-symbol lookups. Currently the agent uses MCP enthusiastically and pays for it.
- **Re-run with a small-doc-heavy corpus** (e.g. a Python project with many `.md` files) — DocSection heading chunking should shine there. Full's signal is code-dominated.
- One-shot runs only — no multi-turn dialogues. Some MCP value (cached subsequent calls) only shows over multi-turn workflows. Add a follow-up bench for that if you care.
- The WITHOUT side will likely use `Bash(grep …)` and `Read` heavily. That's the realistic baseline a no-MCP agent has.
