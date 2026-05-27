# HeliosDB-Nano CodeKB MCP Report

Date: 2026-05-27

Context: prepared for AGNTCon + MCPCon Europe 2026 and for the current
HeliosDB-Nano CodeKB MCP implementation work.

## Executive Summary

The main product problem this MCP addresses is context tax. Agentic coding
clients pay for every advertised tool, schema, and verbose result. On large
repositories, that overhead competes directly with useful reasoning budget.

The current implementation changes the default experience from a many-tool MCP
surface to a compact `helios(action, args)` gateway, with server-routed answer
cards and evidence. The result is a smaller tool catalogue, fewer raw snippets,
and a clearer server-side contract for Claude Code, Codex, Gemini, and local
Ollama-style agents.

Measured on the large `/home/gpc/HDB/Full` corpus with `qwen3-coder:30b`,
compact MCP reduced model tokens by 37.2% versus a no-MCP Read/Grep/Bash
baseline. The quality result is mixed: broad repository questions benefit most,
while direct file/symbol lookups can still be better served by raw filesystem
tools when symbol cards are unavailable.

## Implemented Product Changes

1. Fresh `serve` defaults to compact one-tool mode: `helios(action, args)`.
2. Added `--no-mega-tool` for clients that require explicit per-tool schemas.
3. Persisted `[serve] mega_tool` and `wrapper_cache_size` in config.
4. Added `helios_ask`, a question router that chooses the smallest useful
   wrapper and returns a compact answer card.
5. Standardized wrapper responses around `helios.answer_card.v1`, with
   summaries, evidence, token budgets, and omitted metadata.
6. Hid `helios_semantic_filter` unless built with `wrappers-semantic`.
7. Fixed `doc_drill` to follow `DocSection -> DocChunk` `PART_OF` edges
   directly instead of re-searching by title.
8. Fixed RepoMap top-symbol `doc1l` lookup to use qualified names.
9. Updated `.mcp.json`, setup commands, bench defaults, README, and steering
   prompts so compact mode and `ask` are the default adoption path.

## Tool Surface Impact

Measured against the same temp fixture before and after the implementation:

| Scenario | Before | After | Change |
|---|---:|---:|---:|
| Fresh default serve | 12 tools, 6083 bytes | 1 tool, 721 bytes | -88.1% |
| Explicit standard profile | 12 tools, 6083 bytes | 12 tools, 6751 bytes | +11.0% |
| Explicit minimal profile | 6 tools, 2901 bytes | 6 tools, 3489 bytes | +20.3% |
| Explicit mega mode | 1 tool, 744 bytes | 1 tool, 721 bytes | -3.1% |

The default path is the important ecosystem change: a new user no longer
advertises the full engine catalogue on every turn. Explicit profiles remain
available for debugging and clients that need separate schemas.

## Large Corpus Benchmark

Corpus source: `/home/gpc/HDB/Full`

Benchmark copy: `/tmp/codekb-bench-20260520/full-with` and
`/tmp/codekb-bench-20260520/full-without`

Model: `qwen3-coder:30b`

Endpoint: `http://ollama:11434`

Mode: compact `helios` mega-tool with steering prompt enabled.

Trials: 1

Questions: 15

Limits: `MAX_TURNS=16`, `MAX_TOOL_BYTES=4096`

Final results: `/tmp/codekb-bench-ollama-full-reingested-20260526/SUMMARY.md`

### Reingest

The existing May 20 KB opened, but it was stale enough that wrapper quality was
misleading. I reingested the filtered Full bench copy with the current release
binary before taking final numbers.

Reingest result:

| Metric | Value |
|---|---:|
| Elapsed | 93m57s |
| Files seen | 7349 |
| Code files upserted | 6683 |
| Text files upserted | 400 |
| Markdown files upserted | 662 |
| Read errors | 8 binary XA log files |
| MENTIONS edges | 17,888 |
| File cards | 6,480 |
| Symbol cards | 0 |

The 0 symbol-card result is a real quality caveat for symbol-signature
questions.

### Final Token Results

| Setup | Model Tokens | Turns | Tool Bytes | Wall |
|---|---:|---:|---:|---:|
| WITH MCP | 311,634 | 108 | 139,315 | 988s |
| WITHOUT MCP | 496,341 | 122 | 255,565 | 152s |

Headline: WITH-MCP saved 184,707 model tokens, or 37.2%.

MCP won on 8 of 15 questions. The strongest wins were broad or hard
cross-repository searches:

| Question | WITH MCP | WITHOUT MCP | Delta |
|---|---:|---:|---:|
| q01 foreign-key validation on INSERT | 11,115 | 58,304 | -47,189 |
| q03 WAL flushing function | 10,736 | 86,850 | -76,114 |
| q09 HNSW implementation and inserts | 28,641 | 98,166 | -69,525 |
| q11 overall architecture | 3,798 | 19,804 | -16,006 |

MCP regressed on 7 of 15 questions. The largest regressions were direct
file/symbol questions where raw filesystem tools were cheaper:

| Question | WITH MCP | WITHOUT MCP | Delta |
|---|---:|---:|---:|
| q10 public types in `heliosdb-vector` | 31,428 | 10,596 | +20,832 |
| q07 `HIGH_AVAILABILITY_OVERVIEW.md` | 17,080 | 10,351 | +6,729 |
| q05 multi-tenancy | 24,783 | 18,502 | +6,281 |

WITHOUT MCP hit the 16-turn cap on q03 and q09. WITH MCP had no harness errors
and no max-turn caps.

## Quality Findings

Token savings are real, but answer quality still needs work. A simple scan for
"cannot find" style language flagged 8 of 15 WITH-MCP answers and 0 of 15
WITHOUT-MCP answers in the final run. This does not mean all flagged answers
were wrong, but it does show that the model still lacks enough exact evidence
for some questions.

Likely causes:

- Symbol-card generation did not populate for this corpus, so exact signature
  questions could not use the intended symbol summary path.
- `helios_ask` sometimes routes broad enough that it spends more turns than a
  direct file read would need.
- Cold-start cost is high because the benchmark launches a fresh MCP server for
  every WITH-MCP question; `tools/list` on the reingested KB took about 54s.
- Direct filename questions are often cheaper through raw Read/Grep unless the
  MCP has an exact document lookup path.

## Ecosystem Benefits

The ecosystem benefit is not just "another MCP server." The useful pattern is
server-side compression of agent work:

- Advertise fewer tools by default.
- Move schema detail behind an on-demand action catalogue.
- Return answer cards instead of raw result dumps.
- Include explicit evidence and omission metadata.
- Let clients set response budgets in tokens.
- Keep raw engine access as an escape hatch.

These patterns help every MCP client, not only this project. A smaller tool
surface reduces per-turn cache pressure. Evidence-first responses make agent
answers easier to ground. Budgeted answer cards let the server preserve the
most useful facts instead of relying on blind byte truncation.

For Claude Code, Codex, Gemini, and similar tools, the practical value is
simple: less context spent on plumbing, more context left for reasoning.

## Recommended Next Work

1. Fix symbol-card population on the Full corpus and rerun q03, q06, q10.
2. Add a direct document/file lookup wrapper for exact filename questions.
3. Improve `helios_ask` routing so direct symbol/file questions avoid broad
   repo-summary exploration.
4. Add a persistent-server benchmark mode to remove per-question cold start.
5. Run `TRIALS=3` to reduce model path variance.
6. Add quality grading, not only token accounting.
