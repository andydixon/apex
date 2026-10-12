#!/usr/bin/env bash
# Build the PDF user guide into dist/apex-user-guide.pdf.
# Needs pdflatex with tcolorbox, enumitem, titlesec, fancyhdr, booktabs,
# tabularx, longtable, fancyvrb, microtype, hyperref (TeX Live "latex-extra").
# Two passes so the table of contents and page references settle.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
OUT="$ROOT/dist"
mkdir -p "$OUT"
cd "$HERE"
for _ in 1 2; do
  pdflatex -interaction=nonstopmode -halt-on-error -output-directory "$OUT" apex-user-guide.tex >/dev/null
done
# Keep only the PDF in dist; the auxiliary files are noise.
rm -f "$OUT"/apex-user-guide.{aux,log,out,toc}
echo "built $OUT/apex-user-guide.pdf"
