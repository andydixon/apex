#!/usr/bin/env bash
# Scripted smoke test: drives the real editor under Xvfb with xdotool and
# verifies the saved file. Produces screenshots in $OUT for human review.
#
# Usage: tests/visual/smoke.sh [output-dir]
# Requires: Xvfb, xdotool, ImageMagick `import`, a Vulkan software driver (lavapipe).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/target/smoke}"
mkdir -p "$OUT"
BIN="$ROOT/target/debug/apex"
[ -x "$BIN" ] || cargo build -q --bin apex
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
export WGPU_BACKEND=vulkan
export APEX_CONFIG_DIR="$OUT/config" APEX_DATA_DIR="$OUT/data" APEX_LOG=warn
export DISPLAY=:${SMOKE_DISPLAY:-78}

FILE="$OUT/sample.txt"
printf 'alpha beta\nalpha gamma\nalpha delta\n' > "$FILE"

Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
XPID=$!
trap 'kill $APID 2>/dev/null; kill $XPID 2>/dev/null' EXIT
sleep 1
"$BIN" "$FILE" > "$OUT/app.log" 2>&1 &
APID=$!
shot() { import -window root "$OUT/$1.png"; }
key() { xdotool key --delay 60 "$@"; sleep 0.3; }
type_text() { xdotool type --delay 40 "$1"; sleep 0.3; }

# Wait for the window to be mapped (slow under software rendering), then focus it:
# without a window manager the X window must be focused explicitly.
WID=""
for _ in $(seq 1 60); do
  WID=$( (xdotool search --onlyvisible --name "Apex" 2>/dev/null || true) | head -1)
  [ -n "$WID" ] && break
  sleep 0.5
done
[ -n "$WID" ] || { echo "Apex window did not appear"; exit 1; }
sleep 1
xdotool windowfocus --sync "$WID" 2>/dev/null || { sleep 1; xdotool windowfocus --sync "$WID"; }
sleep 0.3
# Focus the editor by clicking in the text area, then go to the document start.
xdotool mousemove 400 300 click 1; sleep 0.3
key ctrl+Home
shot 01-opened

# Type at the start, verify multi-line typing and undo.
type_text "# heading"
key Return
shot 02-typed
key ctrl+z
key ctrl+z

# Quick-add: select "alpha" three times then type a replacement.
key ctrl+Home
key ctrl+d ctrl+d ctrl+d
shot 03-quick-add
type_text "omega"
key Escape
shot 04-replaced

# Column selection with Ctrl+Shift+Alt+Down from the end of line 1.
key ctrl+Home
key End
key ctrl+shift+alt+Down
key ctrl+shift+alt+Down
type_text " !"
key Escape
shot 05-column

# Find bar with regex error, then close it.
key ctrl+f
type_text "gam"
shot 06-find
key alt+r
type_text "("
shot 07-regex-error
key Escape

# Split pane and theme switch via menus are exercised with shortcuts.
key ctrl+backslash
shot 08-split
key alt+shift+1

# Save and verify.
key ctrl+s
sleep 1
shot 09-saved
kill $APID; wait $APID 2>/dev/null || true
# Column selection keeps display column 10 on every line (it does not snap to
# line ends), so the longer lines get the insertion before their final letter.
EXPECTED=$'omega beta !\nomega gamm !a\nomega delt !a\n'
ACTUAL="$(cat "$FILE"; printf x)"; ACTUAL="${ACTUAL%x}"
if [ "$ACTUAL" == "$EXPECTED" ]; then
  echo "SMOKE OK: saved file matches expected content"
else
  echo "SMOKE FAILED: file content was:"; cat -A "$FILE"; exit 1
fi
# No stray temp files from the atomic save.
if ls "$OUT"/.sample.txt.apex-* >/dev/null 2>&1; then echo "SMOKE FAILED: temp file left behind"; exit 1; fi
grep -i "panick" "$OUT/app.log" && { echo "SMOKE FAILED: panic in log"; exit 1; }
echo "screenshots in $OUT"
