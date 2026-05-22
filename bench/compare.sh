#!/usr/bin/env bash
# Aggregate cost / wall / token fields from per-question JSON results
# into a markdown comparison table. Prints to stdout — redirect to
# file. Supports the multi-trial output shape (qNN-tT.json) by
# computing per-question median + min + max across trials; falls back
# to single-value rows when there's only one trial.
#
# Env vars:
#   BENCH_DIR     where the results live (matches bench/run.sh)
#   SUFFIX        "" or "-steered" — picks which results dir pair to
#                 compare. Default "". To compare steered vs bare
#                 directly, run compare.sh twice with different
#                 SUFFIXes and diff the outputs.

set -euo pipefail

BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-$(date +%Y%m%d)}"
SUFFIX="${SUFFIX:-}"
RES_WITH="$BENCH_DIR/results/with${SUFFIX}"
RES_WITHOUT="$BENCH_DIR/results/without${SUFFIX}"

if [[ ! -d "$RES_WITH" || ! -d "$RES_WITHOUT" ]]; then
  echo "No results found under $BENCH_DIR/results (SUFFIX=$SUFFIX). Did bench/run.sh finish?" >&2
  exit 1
fi

# Extract one row of fields from one JSON file.
emit() {
  local file="$1"
  if [[ -f "$file" ]]; then
    jq -r '
      [
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
    ' "$file" 2>/dev/null || echo -e "0\t0\t0\t0\t0\t0\t0\t0\t0"
  else
    echo -e "-\t-\t-\t-\t-\t-\t-\t-\t-"
  fi
}

# Median of a stream of numbers (one per line). Empty input → 0.
median() {
  awk '{a[NR]=$1} END{
    if (NR==0) {print 0; exit}
    n=asort(a)
    if (n%2==1) print a[(n+1)/2]
    else printf "%.6f\n", (a[n/2]+a[n/2+1])/2
  }'
}
minv() { awk 'NR==1||$1<m{m=$1} END{if(NR==0)print 0; else print m}'; }
maxv() { awk '$1>m{m=$1} END{if(NR==0)print 0; else print m}'; }
sumv() { awk '{s+=$1} END{print s+0}'; }

# Detect trials per question by counting matching files.
shopt -s nullglob
detect_trials() {
  local dir="$1" q="$2"
  local n=0
  for f in "$dir"/"$q".json "$dir"/"$q"-t*.json; do
    [[ -f "$f" ]] && n=$((n + 1))
  done
  echo "$n"
}

# Enumerate question stems (qNN) from the WITH side.
questions=$(
  for f in "$RES_WITH"/q*.json; do
    [[ -f "$f" ]] || continue
    basename "$f" .json | sed -E 's/-t[0-9]+$//'
  done | sort -u
)

echo "# Bench summary — heliosdb-codekb MCP vs no MCP"
echo
echo "Source corpus: \`${SRC_CORPUS:-/home/gpc/HDB/Full}\`. Suffix: \`${SUFFIX:-<none>}\`. Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)."
echo

echo "## Per-question results (median across trials)"
echo
echo "| Q | Setup | trials | \$ cost (med / min / max) | wall_s med | resp_chars med | turns med |"
echo "|----|---------|------:|---:|---:|---:|---:|"

n_q=0
overall_cost_with_sum=0; overall_cost_without_sum=0
overall_wall_with_sum=0; overall_wall_without_sum=0
overall_in_with_sum=0;  overall_in_without_sum=0
overall_out_with_sum=0; overall_out_without_sum=0
overall_cin_with_sum=0; overall_cin_without_sum=0
overall_cr_with_sum=0;  overall_cr_without_sum=0

for q in $questions; do
  n_q=$((n_q + 1))
  for side in with without; do
    if [[ "$side" == "with" ]]; then dir="$RES_WITH"; else dir="$RES_WITHOUT"; fi
    files=("$dir"/"$q".json "$dir"/"$q"-t*.json)
    real=()
    for f in "${files[@]}"; do [[ -f "$f" ]] && real+=("$f"); done
    t=${#real[@]}
    if (( t == 0 )); then
      printf "| %s | %-7s | %d | - | - | - | - |\n" "$q" "$side" 0
      continue
    fi

    # Build vectors of each metric across trials.
    costs=""; walls=""; resps=""; ins=""; outs=""; cins=""; crs=""; turns=""
    for f in "${real[@]}"; do
      IFS=$'\t' read -r c w _ r in_ out cin cr tu <<<"$(emit "$f")"
      costs+="$c"$'\n'; walls+="$w"$'\n'; resps+="$r"$'\n'
      ins+="$in_"$'\n'; outs+="$out"$'\n'; cins+="$cin"$'\n'; crs+="$cr"$'\n'
      turns+="$tu"$'\n'
    done

    cost_med=$(echo -n "$costs" | median)
    cost_min=$(echo -n "$costs" | minv)
    cost_max=$(echo -n "$costs" | maxv)
    wall_med=$(echo -n "$walls" | median)
    resp_med=$(echo -n "$resps" | median)
    turns_med=$(echo -n "$turns" | median)

    printf "| %s | %-7s | %d | %.5f / %.5f / %.5f | %s | %s | %s |\n" \
      "$q" "$side" "$t" "$cost_med" "$cost_min" "$cost_max" "$wall_med" "$resp_med" "$turns_med"

    # For totals we sum the medians (representative per-question cost).
    if [[ "$side" == "with" ]]; then
      overall_cost_with_sum=$(awk -v a="$overall_cost_with_sum" -v b="$cost_med" 'BEGIN{print a+b}')
      overall_wall_with_sum=$(awk -v a="$overall_wall_with_sum" -v b="$wall_med" 'BEGIN{print a+b}')
      overall_in_with_sum=$(awk -v a="$overall_in_with_sum" -v b="$(echo -n "$ins" | median)" 'BEGIN{print a+b}')
      overall_out_with_sum=$(awk -v a="$overall_out_with_sum" -v b="$(echo -n "$outs" | median)" 'BEGIN{print a+b}')
      overall_cin_with_sum=$(awk -v a="$overall_cin_with_sum" -v b="$(echo -n "$cins" | median)" 'BEGIN{print a+b}')
      overall_cr_with_sum=$(awk -v a="$overall_cr_with_sum" -v b="$(echo -n "$crs" | median)" 'BEGIN{print a+b}')
    else
      overall_cost_without_sum=$(awk -v a="$overall_cost_without_sum" -v b="$cost_med" 'BEGIN{print a+b}')
      overall_wall_without_sum=$(awk -v a="$overall_wall_without_sum" -v b="$wall_med" 'BEGIN{print a+b}')
      overall_in_without_sum=$(awk -v a="$overall_in_without_sum" -v b="$(echo -n "$ins" | median)" 'BEGIN{print a+b}')
      overall_out_without_sum=$(awk -v a="$overall_out_without_sum" -v b="$(echo -n "$outs" | median)" 'BEGIN{print a+b}')
      overall_cin_without_sum=$(awk -v a="$overall_cin_without_sum" -v b="$(echo -n "$cins" | median)" 'BEGIN{print a+b}')
      overall_cr_without_sum=$(awk -v a="$overall_cr_without_sum" -v b="$(echo -n "$crs" | median)" 'BEGIN{print a+b}')
    fi
  done
done

echo
echo "## Totals (sum of per-question medians, $n_q questions)"
echo
echo "| Metric | with MCP | without MCP | Δ (with − without) | Δ % |"
echo "|---|---:|---:|---:|---:|"
pct() { awk -v a="$1" -v b="$2" 'BEGIN{ if (b==0) print "n/a"; else printf "%+.1f%%\n", 100*(a-b)/b }'; }
delta() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%+.6f\n", a-b}'; }
delta_int() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%+d\n", a-b}'; }
printf "| total_cost_usd | %.6f | %.6f | %s | %s |\n" "$overall_cost_with_sum" "$overall_cost_without_sum" "$(delta "$overall_cost_with_sum" "$overall_cost_without_sum")" "$(pct "$overall_cost_with_sum" "$overall_cost_without_sum")"
printf "| total_wall_secs | %s | %s | %s | %s |\n" "$overall_wall_with_sum" "$overall_wall_without_sum" "$(delta_int "$overall_wall_with_sum" "$overall_wall_without_sum")" "$(pct "$overall_wall_with_sum" "$overall_wall_without_sum")"
printf "| total_input_tokens | %s | %s | %s | %s |\n" "$overall_in_with_sum" "$overall_in_without_sum" "$(delta_int "$overall_in_with_sum" "$overall_in_without_sum")" "$(pct "$overall_in_with_sum" "$overall_in_without_sum")"
printf "| total_output_tokens | %s | %s | %s | %s |\n" "$overall_out_with_sum" "$overall_out_without_sum" "$(delta_int "$overall_out_with_sum" "$overall_out_without_sum")" "$(pct "$overall_out_with_sum" "$overall_out_without_sum")"
printf "| total_cache_creation_tokens | %s | %s | %s | %s |\n" "$overall_cin_with_sum" "$overall_cin_without_sum" "$(delta_int "$overall_cin_with_sum" "$overall_cin_without_sum")" "$(pct "$overall_cin_with_sum" "$overall_cin_without_sum")"
printf "| total_cache_read_tokens | %s | %s | %s | %s |\n" "$overall_cr_with_sum" "$overall_cr_without_sum" "$(delta_int "$overall_cr_with_sum" "$overall_cr_without_sum")" "$(pct "$overall_cr_with_sum" "$overall_cr_without_sum")"
echo
echo "## How to interpret"
echo
echo "- Per-question table uses the **median** across trials so a single bad run doesn't dominate. Min and max alongside it give a sense of variance."
echo "- Totals sum the per-question medians — a representative per-question cost rolled up."
echo "- A **lower** \`total_cost_usd\` with MCP means the MCP path saved tokens."
echo "- For multi-trial robustness: at least 3 trials per question (TRIALS=3 on bench/run.sh)."
echo "- For prompt-steering comparison: run twice with STEER=0 and STEER=1, then run \`SUFFIX=-steered bench/compare.sh\` to see the steered numbers."
