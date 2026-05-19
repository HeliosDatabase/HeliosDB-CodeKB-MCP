#!/usr/bin/env bash
# Aggregate cost + wall time from per-question JSON results into a
# markdown comparison table. Prints to stdout — redirect to file.
#
# Reads $BENCH_DIR/results/{with,without}/qNN.json. The JSON shape
# (per `claude -p --output-format json`) includes at least:
#   .total_cost_usd   number
#   .result           string (the assistant's text response)
#   .bench.wall_secs  number (decorated by run.sh)
#   .bench.exit_code  number (decorated by run.sh)
# Extra usage fields (input_tokens, output_tokens, …), if present, are
# detected by `jq -r 'paths | join(".")'` and surfaced.

set -euo pipefail

BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-$(date +%Y%m%d)}"
RES_WITH="$BENCH_DIR/results/with"
RES_WITHOUT="$BENCH_DIR/results/without"

if [[ ! -d "$RES_WITH" || ! -d "$RES_WITHOUT" ]]; then
  echo "No results found under $BENCH_DIR/results. Did bench/run.sh finish?" >&2
  exit 1
fi

# Detect any token-usage fields that landed in the JSON (varies by
# Claude Code version). Probe the first WITH result; reuse for all.
probe="$(ls "$RES_WITH"/q*.json 2>/dev/null | head -1)"
extra_fields=()
if [[ -n "$probe" ]]; then
  while IFS= read -r p; do
    extra_fields+=("$p")
  done < <(jq -r '
    paths(scalars) as $p
    | $p | join(".")
    | select(test("token|usage|cache"; "i"))
  ' "$probe" 2>/dev/null | sort -u)
fi

emit() {
  local label="$1" file="$2"
  if [[ -f "$file" ]]; then
    jq -r --arg label "$label" \
      '[$label, (.total_cost_usd // 0), (.bench.wall_secs // 0), (.bench.exit_code // 0), (.result // "" | length)] | @tsv' \
      "$file" 2>/dev/null || echo -e "${label}\t0\t0\tparse_err\t0"
  else
    echo -e "${label}\t-\t-\tmissing\t-"
  fi
}

echo "# Bench summary — heliosdb-codekb MCP vs no MCP"
echo
echo "Source corpus: \`${SRC_CORPUS:-/home/gpc/HDB/Full}\`. Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)."
echo
echo "## Per-question results"
echo
echo "| Q  | Setup    | total_cost_usd | wall_secs | exit | response_chars |"
echo "|----|----------|---------------:|----------:|-----:|---------------:|"

total_cost_with=0
total_cost_without=0
total_wall_with=0
total_wall_without=0
n=0

for w in "$RES_WITH"/q*.json; do
  [[ -e "$w" ]] || continue
  q="$(basename "$w" .json)"
  wo="$RES_WITHOUT/$q.json"
  n=$((n + 1))

  read -r _ cw sw ew rw < <(emit "with" "$w" | awk '{print}')
  read -r _ cwo swo ewo rwo < <(emit "without" "$wo" | awk '{print}')

  printf "| %s | %-8s | %14s | %9s | %4s | %14s |\n" "$q" "with"    "$cw"  "$sw"  "$ew"  "$rw"
  printf "| %s | %-8s | %14s | %9s | %4s | %14s |\n" "$q" "without" "$cwo" "$swo" "$ewo" "$rwo"

  total_cost_with=$(awk -v a="$total_cost_with" -v b="${cw:-0}" 'BEGIN{print a + b}')
  total_cost_without=$(awk -v a="$total_cost_without" -v b="${cwo:-0}" 'BEGIN{print a + b}')
  total_wall_with=$((total_wall_with + ${sw:-0}))
  total_wall_without=$((total_wall_without + ${swo:-0}))
done

echo
echo "## Totals ($n questions)"
echo
echo "| Metric             | with MCP | without MCP | Δ (with − without) |"
echo "|--------------------|---------:|------------:|-------------------:|"
delta_cost=$(awk -v a="$total_cost_with" -v b="$total_cost_without" 'BEGIN{print a - b}')
delta_wall=$((total_wall_with - total_wall_without))
printf "| total_cost_usd     | %.6f | %.6f | %+.6f |\n" "$total_cost_with" "$total_cost_without" "$delta_cost"
printf "| total_wall_secs    | %8s | %11s | %+18s |\n" "$total_wall_with" "$total_wall_without" "$delta_wall"

if (( ${#extra_fields[@]} > 0 )); then
  echo
  echo "## Token-usage fields detected in JSON"
  echo
  for f in "${extra_fields[@]}"; do
    echo "- \`$f\`"
  done
  echo
  echo "_(per-question values not aggregated above — extend \`compare.sh\` to sum these when you decide which matter.)_"
fi

echo
echo "## How to interpret"
echo
echo "- A **lower** \`total_cost_usd\` with MCP means the MCP path saved tokens."
echo "- A **shorter** \`response_chars\` for MCP runs might mean either tighter answers (good) or the model gave up (bad). Spot-check the text under \`$BENCH_DIR/results/{with,without}/qNN.json -> .result\`."
echo "- An \`exit\` of non-zero in either run usually means \`--max-budget-usd\` was hit before the agent finished, which is itself a data point."
