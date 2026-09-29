#!/usr/bin/env bash
# Rasterise assets/icons/apex.svg into every size the packages need:
#   assets/icons/png/apex-<size>.png   (Linux hicolor, AppImage, documentation)
#   assets/icons/apex.icns             (macOS bundle; written by icns.py, no macOS tools needed)
#   assets/icons/apex.ico              (Windows executable and installer)
# Needs rsvg-convert (librsvg) or ImageMagick, and Python 3.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SVG="$ROOT/assets/icons/apex.svg"
OUT="$ROOT/assets/icons"
mkdir -p "$OUT/png"
render() { # size out
  if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w "$1" -h "$1" "$SVG" -o "$2"
  else
    convert -background none -density 384 "$SVG" -resize "$1x$1" "$2"
  fi
}
for s in 16 24 32 48 64 128 256 512 1024; do
  render "$s" "$OUT/png/apex-$s.png"
done
python3 "$ROOT/packaging/icons/icns.py" "$OUT/png" "$OUT/apex.icns"
python3 "$ROOT/packaging/icons/ico.py" "$OUT/png" "$OUT/apex.ico"
echo "icons written to $OUT"
