#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share/codex-token-metrics-live"
SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$BIN_DIR"
install -m 755 "$SOURCE_DIR/codex-token-metrics-live" "$BIN_DIR/codex-token-metrics-live"
ln -sfn codex-token-metrics-live "$BIN_DIR/cdx-mtr"

if [[ -f "$SOURCE_DIR/assets/codex-token-metrics.html" ]]; then
  mkdir -p "$SHARE_DIR"
  install -m 644 "$SOURCE_DIR/assets/codex-token-metrics.html" "$SHARE_DIR/codex-token-metrics.html"
fi

printf 'Installed codex-token-metrics-live to %s\n' "$BIN_DIR/codex-token-metrics-live"
printf 'Installed cdx-mtr alias command to %s\n' "$BIN_DIR/cdx-mtr"
