#!/usr/bin/env bash
# For each question × each setup (with/without MCP), run `claude -p`
# in headless mode and save the JSON output. Each call is capped at
# $0.25 worst-case.
#
# Modes (env vars):
#   TRIALS=N    repeat each question/setup N times, writing to
#               qNN-tT.json (default 1; recommended 3-5 for stable
#               numbers since agent runs vary 1.5-2× between trials)
#   STEER=1     prepend an --append-system-prompt-file that nudges
#               the agent to prefer `helios_*` MCP tools for code/doc
#               lookups on this corpus. Results land in a parallel
#               results/{with,without}-steered/ tree so you can
#               compare bare-vs-steered without re-running the
#               un-steered baseline.
#   MODEL=…     pin a specific model (e.g. claude-haiku-4-5-20251001).
#               Default is whatever the user's shell has configured.
#   MAX_BUDGET_USD=… per-call dollar cap (default 0.25).
#
# Outputs:
#   $BENCH_DIR/results/with/qNN-tT.json
#   $BENCH_DIR/results/without/qNN-tT.json
#   (and …-steered variants when STEER=1)
#
# Re-running overwrites existing per-question JSON files; safe to
# interrupt.

set -euo pipefail

if [[ "${BENCH_T1_LANDED:-0}" != "1" ]]; then
  echo "bench/run.sh refused: set BENCH_T1_LANDED=1 once engine T1 has landed." >&2
  exit 2
fi

BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-$(date +%Y%m%d)}"
QUESTIONS="${QUESTIONS:-$(dirname "$0")/questions.txt}"
MAX_BUDGET="${MAX_BUDGET_USD:-0.25}"
MODEL="${MODEL:-}"
TRIALS="${TRIALS:-1}"
STEER="${STEER:-0}"

WITH_DIR="$BENCH_DIR/full-with"
WITHOUT_DIR="$BENCH_DIR/full-without"
MCP_ON="$BENCH_DIR/mcp-on.json"
MCP_OFF="$(dirname "$0")/mcp-off.json"
STEER_PROMPT="$(dirname "$0")/steer-prompt.md"

for required in "$WITH_DIR" "$WITHOUT_DIR" "$MCP_ON" "$MCP_OFF"; do
  if [[ ! -e "$required" ]]; then
    echo "Missing: $required. Did you run bench/setup.sh?" >&2
    exit 1
  fi
done
if [[ "$STEER" == "1" && ! -f "$STEER_PROMPT" ]]; then
  echo "STEER=1 but $STEER_PROMPT is missing." >&2
  exit 1
fi

# Suffix the results dir when steered so we can keep both side-by-
# side without overwriting the bare baseline.
SUFFIX=""
[[ "$STEER" == "1" ]] && SUFFIX="-steered"
WITH_RES="$BENCH_DIR/results/with${SUFFIX}"
WITHOUT_RES="$BENCH_DIR/results/without${SUFFIX}"
mkdir -p "$WITH_RES" "$WITHOUT_RES"

# Common flags. We intentionally do NOT use --bare — that flag skips
# OAuth/keychain reads and would require ANTHROPIC_API_KEY in the
# environment. Both runs use Claude Code's default auth, so the only
# intentional difference between setups is whether the MCP server is
# loaded. --strict-mcp-config ensures only what we name is loaded
# (skips user / project / system MCP configs that might leak in).
COMMON_FLAGS=(
  --print
  --output-format json
  --strict-mcp-config
  --no-session-persistence
  --max-budget-usd "$MAX_BUDGET"
  --permission-mode bypassPermissions
)
[[ -n "$MODEL" ]] && COMMON_FLAGS+=(--model "$MODEL")
if [[ "$STEER" == "1" ]]; then
  # --append-system-prompt-file is the supported way to inject text
  # into the system prompt without overriding the default. Same
  # prompt applied to BOTH setups so the comparison stays apples-
  # to-apples (the agent is told about helios_* tools even when no
  # MCP server is loaded — the without-MCP run then has to ignore
  # the suggestion, which is its own data point).
  COMMON_FLAGS+=(--append-system-prompt-file "$STEER_PROMPT")
fi

run_one() {
  local question="$1"
  local out_file="$2"
  local add_dir="$3"
  local mcp_config="$4"
  echo "  → $(basename "$out_file") ($(basename "$(dirname "$out_file")"))…" >&2
  set +e
  start_ts=$(date +%s)
  claude "${COMMON_FLAGS[@]}" \
    --add-dir "$add_dir" \
    --mcp-config "$mcp_config" \
    -- "$question" \
    > "$out_file" 2> "${out_file%.json}.stderr"
  rc=$?
  end_ts=$(date +%s)
  set -e
  # Decorate the JSON with our own wall-time + exit code (JSON output
  # from claude doesn't include duration today).
  jq --argjson dur "$((end_ts - start_ts))" --argjson rc "$rc" \
    '. + {bench: {wall_secs: $dur, exit_code: $rc}}' \
    "$out_file" > "${out_file}.tmp" 2>/dev/null || cp "$out_file" "${out_file}.tmp"
  mv "${out_file}.tmp" "$out_file"
}

i=0
total_runs=0
while IFS= read -r line; do
  # Strip leading/trailing whitespace, skip blank / comment lines.
  q="${line#"${line%%[![:space:]]*}"}"
  q="${q%"${q##*[![:space:]]}"}"
  [[ -z "$q" || "$q" == \#* ]] && continue
  i=$((i + 1))
  printf -v num "%02d" "$i"
  echo "Q$num: $q"
  for t in $(seq 1 "$TRIALS"); do
    printf -v ts "%d" "$t"
    if [[ "$TRIALS" == "1" ]]; then
      # Keep the old qNN.json naming when there's only one trial so
      # existing tooling / saved comparisons keep working.
      tag=""
    else
      tag="-t$ts"
    fi
    run_one "$q" "$WITH_RES/q${num}${tag}.json"    "$WITH_DIR"    "$MCP_ON"
    run_one "$q" "$WITHOUT_RES/q${num}${tag}.json" "$WITHOUT_DIR" "$MCP_OFF"
    total_runs=$((total_runs + 2))
  done
done < "$QUESTIONS"

echo
echo "Done. $i questions × $TRIALS trial(s) × 2 setups = $total_runs runs."
if [[ "$STEER" == "1" ]]; then
  echo "Results: $WITH_RES, $WITHOUT_RES"
else
  echo "Next: bench/compare.sh > $BENCH_DIR/results/SUMMARY.md"
fi
