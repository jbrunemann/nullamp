#!/usr/bin/env bash
# Cross-build the Windows exe from macOS or Linux and zip it.
# Needs: cargo-xwin (cargo install cargo-xwin) and rustup target add x86_64-pc-windows-msvc.
# Usage: packaging/windows/package.sh [BINARY]   (default: build with cargo xwin)
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET=x86_64-pc-windows-msvc
if [[ -n "${1:-}" ]]; then
  BIN="$1"
else
  cargo xwin build --release -p nullamp --target "$TARGET"
  BIN="target/$TARGET/release/nullamp.exe"
fi
DIR="Nullamp-${VERSION}-windows-x86_64"

rm -rf "$DIR" "$DIR.zip"
mkdir -p "$DIR"
cp "$BIN" "$DIR/Nullamp.exe"
zip -qr "$DIR.zip" "$DIR"
rm -rf "$DIR"
echo "Built $DIR.zip"
