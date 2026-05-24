#!/usr/bin/env bash
# Duplicate the source corpus (~/HDB/Full) into two sibling temp dirs
# and index the WITH copy via heliosdb-codekb-mcp. The WITHOUT copy
# stays untouched so the no-MCP run reads raw files via Read/Grep.
#
# Idempotent: re-running with the same BENCH_DIR is a no-op if both
# copies and the KB are already present.
#
# Gate: refuses to proceed without BENCH_T1_LANDED=1 in the env,
# since the engine FK regression (v3.28.0+) makes ingest of a Full-
# size corpus take many hours. See bench/README.md "Gate" section.

set -euo pipefail

if [[ "${BENCH_T1_LANDED:-0}" != "1" ]]; then
  cat >&2 <<EOF
bench/setup.sh refused: BENCH_T1_LANDED is not set.

The engine FK regression (heliosdb-nano v3.28.0+) makes ingest of a
Full-sized corpus take many hours per copy. Wait for engine T1
(in-txn ART overlay) to land and verify by bumping the lockfile.
Then re-run as:

    BENCH_T1_LANDED=1 bench/setup.sh

See bench/README.md for the full gate explanation.
EOF
  exit 2
fi

CORPUS="${CORPUS:-pilot}"   # pilot | large | huge

case "$CORPUS" in
  pilot) DEFAULT_SRC="/home/gpc/HDB/Full" ;;
  large) DEFAULT_SRC="${HOME}/HDB/linux" ;;   # multi-language ~1-5 GB
  huge)  DEFAULT_SRC="${HOME}/HDB/multi" ;;   # ~10 GB polyglot tree
  *) echo "Unknown CORPUS=$CORPUS — expected pilot|large|huge" >&2; exit 2 ;;
esac

SRC_CORPUS="${SRC_CORPUS:-$DEFAULT_SRC}"
BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-${CORPUS}-$(date +%Y%m%d)}"
WITH_DIR="$BENCH_DIR/full-with"
WITHOUT_DIR="$BENCH_DIR/full-without"
BIN="${HELIOS_BIN:-$(command -v heliosdb-codekb-mcp || echo "$PWD/target/release/heliosdb-codekb-mcp")}"

# Default gateway-config slots in mcp-on.json. Bench/run.sh can override
# at run time without re-running setup.sh.
PROFILE="${PROFILE:-standard}"
STRIP="${STRIP:-200}"
MAX_TOOL_RESULT_BYTES="${MAX_TOOL_RESULT_BYTES:-0}"

if [[ ! -d "$SRC_CORPUS" ]]; then
  echo "Source corpus not found at $SRC_CORPUS — set SRC_CORPUS=<path>." >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "heliosdb-codekb-mcp binary not found (looked at \"$BIN\"). Set HELIOS_BIN=<path> or put it on PATH." >&2
  exit 1
fi

mkdir -p "$BENCH_DIR"
echo "Bench dir: $BENCH_DIR"

# Rsync excludes: mirror the engine's `ingest::SKIP_DIRS` + drop .git
# (we don't ingest VCS metadata) so we don't waste minutes copying
# hundreds of GB of build artifacts that the indexer would skip
# anyway. On a typical engine-Full-sized corpus this shrinks the
# rsync from ~400 GB to ~100 MB.
RSYNC_EXCLUDES=(
  --exclude=target          # Rust / Cargo build output
  --exclude=node_modules    # JS / TS
  --exclude=.git            # VCS metadata
  --exclude=__pycache__     # Python bytecode
  --exclude=.venv --exclude=venv
  --exclude=.cache          # tooling caches
  --exclude=dist --exclude=build --exclude=out
  --exclude=.next --exclude=.nuxt
  --exclude=vendor --exclude=Pods
  --exclude=.gradle --exclude=.mvn
  --exclude=.idea --exclude=.vscode
  --exclude=.pytest_cache --exclude=.mypy_cache --exclude=.ruff_cache --exclude=.tox
)

for D in "$WITH_DIR" "$WITHOUT_DIR"; do
  if [[ ! -d "$D" ]]; then
    echo "Copying $SRC_CORPUS → $D (excluding build/cache/.git)…"
    rsync -a --delete "${RSYNC_EXCLUDES[@]}" "$SRC_CORPUS"/ "$D"/
  else
    echo "Refreshing $D from $SRC_CORPUS (rsync delete, excluding build/cache/.git)…"
    rsync -a --delete "${RSYNC_EXCLUDES[@]}" "$SRC_CORPUS"/ "$D"/
  fi
done

# Sandbox the XDG dirs so we don't pollute the user's real config /
# data with the bench's per-source KB registration.
export XDG_CONFIG_HOME="$BENCH_DIR/xdg-config"
export XDG_DATA_HOME="$BENCH_DIR/xdg-data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

echo "Indexing WITH copy ($WITH_DIR) — global mode KB under \$XDG_DATA_HOME…"
"$BIN" init --source "$WITH_DIR" --mode global --ingest

# Render the WITH mcp-config template into the bench dir with the
# current gateway knobs. Re-render at bench time by re-running this
# script with different PROFILE / STRIP / MAX_TOOL_RESULT_BYTES env
# vars — re-render is cheap (the source corpus copy is the slow step).
sed -e "s|@@BIN@@|$BIN|" \
    -e "s|@@WITH_DIR@@|$WITH_DIR|" \
    -e "s|@@PROFILE@@|$PROFILE|" \
    -e "s|@@STRIP@@|$STRIP|" \
    -e "s|@@MAX_TOOL_RESULT_BYTES@@|$MAX_TOOL_RESULT_BYTES|" \
  "$(dirname "$0")/mcp-on.json.tmpl" > "$BENCH_DIR/mcp-on.json"

cat <<EOF

Setup complete.

  CORPUS                 = $CORPUS
  SRC_CORPUS             = $SRC_CORPUS
  WITH_DIR               = $WITH_DIR
  WITHOUT_DIR            = $WITHOUT_DIR
  mcp-on.json            = $BENCH_DIR/mcp-on.json
  PROFILE                = $PROFILE
  STRIP                  = $STRIP
  MAX_TOOL_RESULT_BYTES  = $MAX_TOOL_RESULT_BYTES

Next:

  BENCH_DIR=$BENCH_DIR bench/run.sh
EOF
