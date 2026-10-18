#!/usr/bin/env bash
# Build Nullamp.app from a release build.
# Usage: packaging/macos/bundle.sh [--universal]
#   --universal  build for arm64 and x86_64 and merge with lipo
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=target/bundle/Nullamp.app

if [[ "${1:-}" == "--universal" ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cargo build --release -p nullamp --target aarch64-apple-darwin
  cargo build --release -p nullamp --target x86_64-apple-darwin
  BIN=target/bundle/nullamp-universal
  mkdir -p target/bundle
  lipo -create -output "$BIN" target/aarch64-apple-darwin/release/nullamp target/x86_64-apple-darwin/release/nullamp
else
  cargo build --release -p nullamp
  BIN=target/release/nullamp
fi

rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
cp "$BIN" "$OUT/Contents/MacOS/nullamp"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$OUT/Contents/Info.plist"

# Icon: render the PNG from the SVG-free generator in packaging/icon.py if needed.
ICON_PNG=packaging/icon-1024.png
if [[ ! -f "$ICON_PNG" ]]; then
  python3 packaging/icon.py "$ICON_PNG"
fi
ICONSET=$(mktemp -d)/nullamp.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$OUT/Contents/Resources/nullamp.icns"

# Ad-hoc signature so the bundle runs locally; use a Developer ID for distribution.
codesign --force --deep --sign - "$OUT" >/dev/null 2>&1 || true
echo "Built $OUT ($VERSION)"
