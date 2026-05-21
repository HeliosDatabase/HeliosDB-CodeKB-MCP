#!/usr/bin/env bash
# For each question in bench/questions.txt × each setup (with/without),
# run `claude -p` in headless mode and save the JSON output. Each call
# is capped at $0.25 worst-case.
#
# Outputs:
#   $BENCH_DIR/results/with/qNN.json
#   $BENCH_DIR/results/without/qNN.json
#
# Re-running overwrites existing per-question JSON files; safe to interrupt.

set -euo pipefail

if [[ "${BENCH_T1_LANDED:-0}" != "1" ]]; then
  echo "bench/run.sh refused: set BENCH_T1_LANDED=1 once engine T1 has landed." >&2
  exit 2
fi

BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-$(date +%Y%m%d)}"
QUESTIONS="${QUESTIONS:-$(dirname "$0")/questions.txt}"
MAX_BUDGET="${MAX_BUDGET_USD:-0.25}"
MODEL="${MODEL:-}"

WITH_DIR="$BENCH_DIR/full-with"
WITHOUT_DIR="$BENCH_DIR/full-without"
MCP_ON="$BENCH_DIR/mcp-on.json"
MCP_OFF="$(dirname "$0")/mcp-off.json"

for required in "$WITH_DIR" "$WITHOUT_DIR" "$MCP_ON" "$MCP_OFF"; do
  if [[ ! -e "$required" ]]; then
    echo "Missing: $required. Did you run bench/setup.sh?" >&2
    exit 1
  fi
done

mkdir -p "$BENCH_DIR/results/with" "$BENCH_DIR/results/without"

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
while IFS= read -r line; do
  # Strip leading/trailing whitespace, skip blank / comment lines.
  q="${line#"${line%%[![:space:]]*}"}"
  q="${q%"${q##*[![:space:]]}"}"
  [[ -z "$q" || "$q" == \#* ]] && continue
  i=$((i + 1))
  printf -v num "%02d" "$i"
  echo "Q$num: $q"
  run_one "$q" "$BENCH_DIR/results/with/q$num.json"    "$WITH_DIR"    "$MCP_ON"
  run_one "$q" "$BENCH_DIR/results/without/q$num.json" "$WITHOUT_DIR" "$MCP_OFF"
done < "$QUESTIONS"

echo
echo "Done. $i questions × 2 setups = $((i * 2)) runs."
echo "Next: bench/compare.sh > $BENCH_DIR/results/SUMMARY.md"
