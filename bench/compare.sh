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

# Per-question extractor — pulls the same shape regardless of setup.
# Order matters; downstream tooling reads positional fields.
emit() {
  local label="$1" file="$2"
  # Note: jq variable named `$lbl`, not `$label` — `label` is a jq
  # reserved keyword (used for early-termination) and bare `$label`
  # makes the parser error with "unexpected label".
  if [[ -f "$file" ]]; then
    jq -r --arg lbl "$label" '
      [
        $lbl,
        (.total_cost_usd // 0),
        (.bench.wall_secs // 0),
        (.bench.exit_code // 0),
        (.result // "" | length),
        (.usage.input_tokens // 0),
        (.usage.output_tokens // 0),
        (.usage.cache_creation_input_tokens // 0),
        (.usage.cache_read_input_tokens // 0),
        (.num_turns // 0)
      ] | @tsv
    ' "$file" 2>/dev/null || echo -e "${label}\t0\t0\tparse_err\t0\t0\t0\t0\t0\t0"
  else
    echo -e "${label}\t-\t-\tmissing\t-\t-\t-\t-\t-\t-"
  fi
}

echo "# Bench summary — heliosdb-codekb MCP vs no MCP"
echo
echo "Source corpus: \`${SRC_CORPUS:-/home/gpc/HDB/Full}\`. Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)."
echo
echo "## Per-question results"
echo
echo "| Q | Setup | \$ cost | wall_s | exit | resp_chars | in_tok | out_tok | cache_in | cache_read | turns |"
echo "|----|---------|--------:|-------:|-----:|-----------:|-------:|--------:|---------:|-----------:|------:|"

total_cost_with=0;  total_cost_without=0
total_wall_with=0;  total_wall_without=0
total_in_with=0;    total_in_without=0
total_out_with=0;   total_out_without=0
total_cin_with=0;   total_cin_without=0
total_cread_with=0; total_cread_without=0
n=0

for w in "$RES_WITH"/q*.json; do
  [[ -e "$w" ]] || continue
  q="$(basename "$w" .json)"
  wo="$RES_WITHOUT/$q.json"
  n=$((n + 1))

  IFS=$'\t' read -r _ cw  sw  ew  rw  iw  ow  ciw  crw  tw  <<<"$(emit with    "$w")"
  IFS=$'\t' read -r _ cwo swo ewo rwo iwo owo ciwo crwo two <<<"$(emit without "$wo")"

  printf "| %s | with    | %.5f | %4s | %2s | %8s | %6s | %6s | %7s | %8s | %3s |\n" \
    "$q" "${cw:-0}"  "$sw"  "$ew"  "$rw"  "$iw"  "$ow"  "$ciw"  "$crw"  "$tw"
  printf "| %s | without | %.5f | %4s | %2s | %8s | %6s | %6s | %7s | %8s | %3s |\n" \
    "$q" "${cwo:-0}" "$swo" "$ewo" "$rwo" "$iwo" "$owo" "$ciwo" "$crwo" "$two"

  total_cost_with=$(awk -v a="$total_cost_with"   -v b="${cw:-0}"   'BEGIN{print a + b}')
  total_cost_without=$(awk -v a="$total_cost_without" -v b="${cwo:-0}" 'BEGIN{print a + b}')
  total_wall_with=$((total_wall_with + ${sw:-0}))
  total_wall_without=$((total_wall_without + ${swo:-0}))
  total_in_with=$((total_in_with     + ${iw:-0}))
  total_in_without=$((total_in_without  + ${iwo:-0}))
  total_out_with=$((total_out_with    + ${ow:-0}))
  total_out_without=$((total_out_without + ${owo:-0}))
  total_cin_with=$((total_cin_with    + ${ciw:-0}))
  total_cin_without=$((total_cin_without + ${ciwo:-0}))
  total_cread_with=$((total_cread_with  + ${crw:-0}))
  total_cread_without=$((total_cread_without + ${crwo:-0}))
done

echo
echo "## Totals ($n questions)"
echo
echo "| Metric                       | with MCP | without MCP | Δ (with − without) | Δ %       |"
echo "|------------------------------|---------:|------------:|-------------------:|----------:|"
delta_cost=$(awk -v a="$total_cost_with" -v b="$total_cost_without" 'BEGIN{print a - b}')
delta_wall=$((total_wall_with - total_wall_without))
delta_in=$((total_in_with - total_in_without))
delta_out=$((total_out_with - total_out_without))
delta_cin=$((total_cin_with - total_cin_without))
delta_cread=$((total_cread_with - total_cread_without))
pct() { awk -v a="$1" -v b="$2" 'BEGIN{ if (b==0) print "n/a"; else printf "%+.1f%%\n", 100*(a-b)/b }'; }
printf "| total_cost_usd               | %.6f | %.6f | %+.6f | %s |\n" "$total_cost_with" "$total_cost_without" "$delta_cost"  "$(pct $total_cost_with $total_cost_without)"
printf "| total_wall_secs              | %8s | %11s | %+18s | %s |\n" "$total_wall_with"  "$total_wall_without" "$delta_wall"  "$(pct $total_wall_with $total_wall_without)"
printf "| total_input_tokens           | %8s | %11s | %+18s | %s |\n" "$total_in_with"    "$total_in_without"   "$delta_in"    "$(pct $total_in_with    $total_in_without)"
printf "| total_output_tokens          | %8s | %11s | %+18s | %s |\n" "$total_out_with"   "$total_out_without"  "$delta_out"   "$(pct $total_out_with   $total_out_without)"
printf "| total_cache_creation_tokens  | %8s | %11s | %+18s | %s |\n" "$total_cin_with"   "$total_cin_without"  "$delta_cin"   "$(pct $total_cin_with   $total_cin_without)"
printf "| total_cache_read_tokens      | %8s | %11s | %+18s | %s |\n" "$total_cread_with" "$total_cread_without" "$delta_cread" "$(pct $total_cread_with $total_cread_without)"

echo
echo "## How to interpret"
echo
echo "- A **lower** \`total_cost_usd\` with MCP means the MCP path saved tokens."
echo "- A **shorter** \`response_chars\` for MCP runs might mean either tighter answers (good) or the model gave up (bad). Spot-check the text under \`$BENCH_DIR/results/{with,without}/qNN.json -> .result\`."
echo "- An \`exit\` of non-zero in either run usually means \`--max-budget-usd\` was hit before the agent finished, which is itself a data point."
