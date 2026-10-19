#!/usr/bin/env bash
# Assemble the Linux release tarball from a release build of nullamp.
# Usage: packaging/linux/package.sh [BINARY]   (default: target/release/nullamp)
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BIN="${1:-target/release/nullamp}"
DIR="Nullamp-${VERSION}-linux-x86_64"

rm -rf "$DIR"
mkdir -p "$DIR/icons"
cp "$BIN" "$DIR/nullamp"
cp packaging/linux/nullamp.desktop packaging/linux/install.sh "$DIR/"
chmod 755 "$DIR/nullamp" "$DIR/install.sh"
# Icon themes want real sizes; the generator is standard-library Python, so no image tools are needed.
for s in 48 64 128 256 512; do
  mkdir -p "$DIR/icons/${s}x${s}"
  python3 packaging/icon.py "$DIR/icons/${s}x${s}/nullamp.png" "$s" --full
done
tar -czf "$DIR.tar.gz" "$DIR"
echo "Built $DIR.tar.gz"
