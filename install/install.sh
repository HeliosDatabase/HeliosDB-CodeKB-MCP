#!/usr/bin/env bash
set -euo pipefail

# Install heliosdb-codekb-mcp into ~/.local/bin by default.
#
# Published release assets currently cover Linux x86_64 and macOS x86_64.
# By default this installs the latest crates.io release. Set
# HELIOS_CODEKB_VERSION=vX.Y.Z to pin a GitHub release asset when available,
# or the matching crates.io version on unsupported platforms.

VERSION="${HELIOS_CODEKB_VERSION:-}"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
REPO="HeliosDatabase/HeliosDB-CodeKB-MCP"
BIN="$BIN_DIR/heliosdb-codekb-mcp"

mkdir -p "$BIN_DIR"

os="$(uname -s)"
arch="$(uname -m)"
asset=""
case "$os:$arch" in
  Linux:x86_64)
    asset="heliosdb-codekb-mcp-linux-x86_64"
    ;;
  Darwin:x86_64)
    asset="heliosdb-codekb-mcp-macos-x86_64"
    ;;
esac

if [ -n "$VERSION" ] && [ -n "$asset" ] && command -v curl >/dev/null 2>&1; then
  base="https://github.com/$REPO/releases/download/$VERSION"
  tmp="$(mktemp)"
  trap 'rm -f "$tmp" "$tmp.sha256"' EXIT
  echo "downloading $base/$asset"
  curl -fsSL "$base/$asset" -o "$tmp"
  curl -fsSL "$base/$asset.sha256" -o "$tmp.sha256"
  expected="$(awk '{print $1}' "$tmp.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$tmp" | awk '{print $1}')"
  fi
  [ "$expected" = "$actual" ] || {
    echo "sha256 verification failed" >&2
    exit 1
  }
  install -m 0755 "$tmp" "$BIN"
else
  if [ -n "$VERSION" ]; then
    crate_version="${VERSION#v}"
    echo "installing heliosdb-codekb-mcp $crate_version from crates.io"
    cargo install heliosdb-codekb-mcp --version "$crate_version" --features native-binary-docs --root "$PREFIX"
  else
    echo "installing latest heliosdb-codekb-mcp from crates.io"
    cargo install heliosdb-codekb-mcp --features native-binary-docs --root "$PREFIX"
  fi
fi

echo "installed $BIN"
"$BIN" --help >/dev/null
