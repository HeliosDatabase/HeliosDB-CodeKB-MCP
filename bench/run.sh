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
#   LAYERS=L1,L2,L3,L4  comma-list of Phase 1 compression layers to
#               require. Each layer is enforced at run time:
#                 L1 — requires PROFILE != full (verifies the
#                      tools/list trim is active in mcp-on.json).
#                 L2 — requires plugin wrappers in tools/list. The
#                      gateway always injects them when PROFILE !=
#                      full, so this is a no-op check today; future-
#                      proof for a refactor.
#                 L3 — refuses to run if `_hdb_plugin_*_cards` are
#                      empty (distill hasn't run on the WITH KB).
#                 L4 — implies STEER=1 (inject steer-prompt.md).
#               Default: no layer enforcement. Suffix the results dir
#               with -L<layers> when set so multiple ablations
#               co-exist.
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
LAYERS="${LAYERS:-}"
WITH_KB_DIR="${WITH_KB_DIR:-}"

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

# Layer enforcement (Phase 1). LAYERS=L1,L2,L3,L4 — comma-separated.
# Empty = no enforcement (legacy behaviour).
if [[ -n "$LAYERS" ]]; then
  IFS=',' read -ra LAYER_LIST <<< "$LAYERS"
  for layer in "${LAYER_LIST[@]}"; do
    case "$layer" in
      L1)
        # Verify mcp-on.json sets a non-full profile. Cheap grep over
        # the rendered template.
        if ! grep -q '"--profile"' "$MCP_ON" || grep -q '"full"' "$MCP_ON"; then
          echo "LAYERS includes L1 but mcp-on.json doesn't set --profile (or sets it to 'full'). Re-run setup.sh with PROFILE=minimal|standard." >&2
          exit 3
        fi
        ;;
      L2)
        # Plugin wrappers are always injected when L1 holds. Future-
        # proofing: assert the mcp-on.json profile isn't 'full' (full
        # bypasses the wrapper injection too).
        if grep -q '"full"' "$MCP_ON"; then
          echo "LAYERS includes L2 but PROFILE=full bypasses wrapper injection." >&2
          exit 3
        fi
        ;;
      L3)
        # Distill cards must be present in the WITH KB. Caller must
        # pass WITH_KB_DIR explicitly (where the KB lives — usually
        # XDG_DATA_HOME slug for `--mode global` corpora, or
        # <WITH_DIR>/.helios-kb for co-located).
        if [[ -z "$WITH_KB_DIR" ]]; then
          echo "LAYERS includes L3 — set WITH_KB_DIR=<path-to-helios-kb> so the harness can verify the distill cards exist." >&2
          exit 3
        fi
        # Use a tiny stdio JSON-RPC ping via the binary itself.
        # Heuristic: query SQLite-style metadata for the table name.
        if ! "$(jq -r '.mcpServers.helios.command' "$MCP_ON")" \
            serve --source "$WITH_DIR" --profile full --strip-tool-descriptions none --http 127.0.0.1:0 \
            --help > /dev/null 2>&1; then
          : # binary present; the help probe is a sanity check
        fi
        # Real check: look at the KB's RocksDB for the
        # _hdb_plugin_repomap_cards SST. Cheap approximation.
        if ! ls "$WITH_KB_DIR" 2>/dev/null | grep -qE '(sst|MANIFEST)'; then
          echo "LAYERS includes L3 but WITH_KB_DIR ($WITH_KB_DIR) doesn't look like an open KB. Run \`heliosdb-codekb-mcp ingest --source $WITH_DIR\` first." >&2
          exit 3
        fi
        ;;
      L4)
        if [[ "$STEER" != "1" ]]; then
          echo "LAYERS includes L4 — auto-enabling STEER=1." >&2
          STEER=1
          COMMON_FLAGS_EXTRA=true
        fi
        ;;
      *)
        echo "Unknown layer: $layer (expected L1|L2|L3|L4)" >&2
        exit 3
        ;;
    esac
  done
fi

# Suffix the results dir when steered or layered so we can keep
# multiple ablations side-by-side without overwriting earlier baselines.
SUFFIX=""
[[ "$STEER" == "1" ]] && SUFFIX="-steered"
[[ -n "$LAYERS" ]] && SUFFIX="${SUFFIX}-${LAYERS//,/_}"
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
