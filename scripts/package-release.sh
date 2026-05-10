#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
ASSET_NAME="codex-token-metrics-live-${TARGET}"
ARCHIVE="dist/${ASSET_NAME}.tar.gz"
WORK_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

cargo build --release

mkdir -p dist
mkdir -p "$WORK_DIR/codex-token-metrics-live"
install -m 755 target/release/codex-token-metrics-live "$WORK_DIR/codex-token-metrics-live/codex-token-metrics-live"
ln -s codex-token-metrics-live "$WORK_DIR/codex-token-metrics-live/cdx-mtr"
install -m 755 install.sh "$WORK_DIR/codex-token-metrics-live/install.sh"
cp README.md LICENSE CHANGELOG.md "$WORK_DIR/codex-token-metrics-live/"
printf '%s\n' "$VERSION" > "$WORK_DIR/codex-token-metrics-live/VERSION"

tar -C "$WORK_DIR" -czf "$ARCHIVE" codex-token-metrics-live
sha256sum "$ARCHIVE" > "${ARCHIVE}.sha256"

printf 'Wrote %s\n' "$ARCHIVE"
printf 'Wrote %s\n' "${ARCHIVE}.sha256"
