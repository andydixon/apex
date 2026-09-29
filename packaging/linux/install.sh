#!/usr/bin/env sh
# Install the unpacked tarball for the current user (no root needed):
#   ./install.sh                      → ~/.local
#   PREFIX=/usr/local sudo ./install.sh
# Uninstall: ./install.sh --uninstall (same PREFIX)
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$PREFIX/bin/apex" "$PREFIX/share/applications/apex.desktop" "$PREFIX/share/metainfo/cx.dixon.apex.metainfo.xml"
  for d in "$PREFIX"/share/icons/hicolor/*/apps; do rm -f "$d/apex.png" "$d/apex.svg"; done
  rm -rf "$PREFIX/share/doc/apex"
  echo "Removed Apex from $PREFIX (settings in \$XDG_CONFIG_HOME/apex were left alone)."
  exit 0
fi
mkdir -p "$PREFIX"
cp -r "$HERE/usr/." "$PREFIX/"
command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
echo "Installed to $PREFIX (binary: $PREFIX/bin/apex)."
