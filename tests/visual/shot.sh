#!/usr/bin/env bash
# Open FILE in Apex under Xvfb and save a screenshot to OUT.png.
# Usage: tests/visual/shot.sh FILE OUT.png [extra xdotool key names...]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FILE="$1"; OUT="$2"; shift 2
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
export WGPU_BACKEND=vulkan APEX_LOG=${APEX_LOG:-warn}
export APEX_CONFIG_DIR="${APEX_CONFIG_DIR:-$(dirname "$OUT")/config}" APEX_DATA_DIR="${APEX_DATA_DIR:-$(dirname "$OUT")/data}"
export DISPLAY=:${SHOT_DISPLAY:-81}
Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
XPID=$!
trap 'kill $APID 2>/dev/null; kill $XPID 2>/dev/null' EXIT
sleep 1
"$ROOT/target/debug/apex" "$FILE" > "$OUT.log" 2>&1 &
APID=$!
sleep 5
WID=$(xdotool search --name "Apex" | head -1)
xdotool windowfocus --sync "$WID"; sleep 0.3
for k in "$@"; do xdotool key --delay 80 "$k"; sleep 0.4; done
sleep 0.6
import -window root "$OUT"
