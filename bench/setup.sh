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

SRC_CORPUS="${SRC_CORPUS:-/home/gpc/HDB/Full}"
BENCH_DIR="${BENCH_DIR:-${TMPDIR:-/tmp}/codekb-bench-$(date +%Y%m%d)}"
WITH_DIR="$BENCH_DIR/full-with"
WITHOUT_DIR="$BENCH_DIR/full-without"
BIN="${HELIOS_BIN:-$(command -v heliosdb-codekb-mcp || echo "$PWD/target/release/heliosdb-codekb-mcp")}"

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

for D in "$WITH_DIR" "$WITHOUT_DIR"; do
  if [[ ! -d "$D" ]]; then
    echo "Copying $SRC_CORPUS → $D (this can take a while; rsync with --delete on re-run)…"
    rsync -a --delete "$SRC_CORPUS"/ "$D"/
  else
    echo "Refreshing $D from $SRC_CORPUS (rsync delete)…"
    rsync -a --delete "$SRC_CORPUS"/ "$D"/
  fi
done

# Sandbox the XDG dirs so we don't pollute the user's real config /
# data with the bench's per-source KB registration.
export XDG_CONFIG_HOME="$BENCH_DIR/xdg-config"
export XDG_DATA_HOME="$BENCH_DIR/xdg-data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

echo "Indexing WITH copy ($WITH_DIR) — global mode KB under \$XDG_DATA_HOME…"
"$BIN" init --source "$WITH_DIR" --mode global --ingest

# Render the WITH mcp-config template into the bench dir.
sed -e "s|@@BIN@@|$BIN|" \
    -e "s|@@WITH_DIR@@|$WITH_DIR|" \
  "$(dirname "$0")/mcp-on.json.tmpl" > "$BENCH_DIR/mcp-on.json"

cat <<EOF

Setup complete.

  WITH_DIR    = $WITH_DIR
  WITHOUT_DIR = $WITHOUT_DIR
  mcp-on.json = $BENCH_DIR/mcp-on.json

Next:

  BENCH_DIR=$BENCH_DIR bench/run.sh
EOF
