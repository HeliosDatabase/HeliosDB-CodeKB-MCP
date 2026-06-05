# CodeKB MCP 0.2.5 Adoption Gate

Goal: make `heliosdb-codekb-mcp` attractive to Claude Code and Codex users on
large repos and monorepos by improving setup reliability, answer quality, and
measured token efficiency.

## User-Facing Changes

- HTTP-first local setup: one `serve --http 127.0.0.1:<port>` daemon per KB.
- Stdio fallback remains for clients that only support subprocess MCP.
- `doctor` diagnoses config, KB lock holders, live HTTP `/info`, and cache stats.
- `config set-serve` persists compact mode and wrapper cache defaults.
- Exact path wrappers:
  - `helios(action="file_lookup", args={...})`
  - `helios(action="doc_lookup", args={...})`
- `helios_ask` routes filename/doc questions to exact lookup before broad search.

## Benchmark Gate

For a release-quality adoption claim, run the warm HTTP Ollama benchmark:

```bash
heliosdb-codekb-mcp serve --source /home/app/Helios \
  --http 127.0.0.1:8765 --wrapper-cache-size 128

OLLAMA_BASE=http://ollama:11434 \
OLLAMA_MODEL=qwen3-coder:30b \
WITH_DIR=/home/app/Helios \
WITHOUT_DIR=/home/app/Helios \
MCP_URL=http://127.0.0.1:8765/ \
QUESTIONS=bench/portfolio_questions.txt \
BENCH_DIR=/tmp/codekb-bench-portfolio-http \
TRIALS=3 \
STEER=1 \
python3 bench/ollama_run.py
```

Then aggregate:

```bash
python3 bench/ollama_compare.py /tmp/codekb-bench-portfolio-http \
  portfolio-http > /tmp/codekb-bench-portfolio-http/SUMMARY.md
```

Pass condition for the marketing headline:

- WITH-MCP model tokens at least 35% lower than WITHOUT-MCP.
- No increase in max-turn failures.
- Answers cite concrete files, symbols, or doc sections for broad questions.
- Direct path questions use `file_lookup` / `doc_lookup`, not broad GraphRAG.

If the result is below 35%, publish the per-question matrix and position CodeKB
around quality/catastrophe-prevention/cross-modal retrieval rather than a flat
token-savings claim.
