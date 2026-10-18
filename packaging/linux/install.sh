#!/usr/bin/env bash
# Install Nullamp from the release tarball so it shows up in the app menu and dock with its icon.
# Usage: ./install.sh [--prefix DIR] [--uninstall]
#   --prefix DIR  install below DIR (default: ~/.local, or /usr/local when run as root)
#   --uninstall   remove what a previous install put there
set -euo pipefail
cd "$(dirname "$0")"

if [[ $EUID -eq 0 ]]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
UNINSTALL=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done

BIN_DIR="$PREFIX/bin"
APPS_DIR="$PREFIX/share/applications"
ICON_ROOT="$PREFIX/share/icons/hicolor"

refresh_caches() {
  command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -f -t "$ICON_ROOT" 2>/dev/null || true
}

if [[ $UNINSTALL -eq 1 ]]; then
  rm -f "$BIN_DIR/nullamp" "$APPS_DIR/nullamp.desktop" "$ICON_ROOT"/*/apps/nullamp.png
  refresh_caches
  echo "Removed Nullamp from $PREFIX. Your library and settings are untouched."
  exit 0
fi

[[ -x nullamp && -f nullamp.desktop && -d icons ]] || { echo "Run this script from the unpacked Nullamp folder." >&2; exit 1; }

mkdir -p "$BIN_DIR" "$APPS_DIR"
install -m 755 nullamp "$BIN_DIR/nullamp"

# The menu entry must point at the binary by absolute path: ~/.local/bin is often not on the
# PATH of a graphical session, and a desktop file that cannot find its binary is silently hidden.
while IFS= read -r line; do
  case "$line" in
    Exec=*) printf 'Exec="%s" %%F\n' "$BIN_DIR/nullamp" ;;
    *) printf '%s\n' "$line" ;;
  esac
done < nullamp.desktop > "$APPS_DIR/nullamp.desktop"
chmod 644 "$APPS_DIR/nullamp.desktop"

for dir in icons/*/; do
  size=$(basename "$dir")
  mkdir -p "$ICON_ROOT/$size/apps"
  install -m 644 "$dir/nullamp.png" "$ICON_ROOT/$size/apps/nullamp.png"
done
refresh_caches

echo "Installed Nullamp to $PREFIX."
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) echo "Note: $BIN_DIR is not on your PATH; start Nullamp from the app menu or run $BIN_DIR/nullamp." ;; esac
echo "If the dock still shows a generic icon, log out and back in, or restart the shell (GNOME: Alt+F2, r, Enter on X11)."
