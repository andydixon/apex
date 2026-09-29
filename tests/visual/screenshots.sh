#!/usr/bin/env bash
# Produce the README/user-guide screenshots under Xvfb, reproducibly.
#   tests/visual/screenshots.sh [path/to/apex]   → docs/screenshots/*.png
# Needs Xvfb, xdotool, ImageMagick (import) and a Vulkan driver (lavapipe is fine).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${1:-$ROOT/target/debug/apex}"
OUT="$ROOT/docs/screenshots"
mkdir -p "$OUT"
WORK="$(mktemp -d)"
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json} WGPU_BACKEND=${WGPU_BACKEND:-vulkan} APEX_LOG=warn
export APEX_CONFIG_DIR="$WORK/config" APEX_DATA_DIR="$WORK/data" APEX_RUNTIME_DIR="$WORK/rt" DISPLAY=:${SHOT_DISPLAY:-86}
mkdir -p "$APEX_CONFIG_DIR" "$APEX_RUNTIME_DIR"
Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
XPID=$!
APID=""
cleanup() { kill $APID 2>/dev/null || true; kill $XPID 2>/dev/null || true; rm -rf "${WORK:?}"; }
trap cleanup EXIT
sleep 1

RUN=0
start() { # args...
  RUN=$((RUN + 1))
  export APEX_DATA_DIR="$WORK/data-$RUN"   # fresh session/recovery state per instance
  "$BIN" --new-window "$@" >"$WORK/app.log" 2>&1 &
  APID=$!
  local wid=""
  for _ in $(seq 1 80); do
    wid=$( (xdotool search --onlyvisible --pid "$APID" 2>/dev/null || true) | head -1)
    [ -n "$wid" ] && break
    sleep 0.5
  done
  [ -n "$wid" ] || { echo "window did not appear"; exit 1; }
  sleep 1.5
  xdotool windowfocus --sync "$wid" 2>/dev/null || { sleep 1; xdotool windowfocus --sync "$wid"; }
  sleep 0.5
}
stop() { kill $APID 2>/dev/null || true; wait $APID 2>/dev/null || true; APID=""; sleep 0.5; }
shot() { sleep "${2:-0.8}"; import -window root "$OUT/$1.png"; echo "wrote $1.png"; }
keys() { for k in "$@"; do xdotool key --delay 60 "$k"; sleep 0.25; done; }
type_text() { xdotool type --delay 40 "$1"; sleep 0.3; }

# 1. Editor with the repository open: Rust highlighting, sidebar, minimap, git gutter.
start "$ROOT" "$ROOT/crates/apex-edit/src/history.rs:86"
keys End Down Down Down End
type_text " // edited"
shot editor 1.2
# 2. Multiple cursors: quick-add the word under the caret, then type.
keys ctrl+z ctrl+Home
keys Down Down Down Down Down Down Down Down
keys ctrl+d ctrl+d ctrl+d ctrl+d
shot multi-cursor
keys Escape
# 3. Goto Anything.
keys ctrl+p
type_text "transac"
shot goto-anything 1.0
keys Escape
# 4. Command palette.
keys ctrl+shift+p
type_text "sort"
shot command-palette 1.0
keys Escape
# 5. Find in Files.
keys ctrl+shift+f ctrl+a
type_text "fn apply"
keys Return
shot find-in-files 3.0
keys Escape
# 6. Settings panel.
keys ctrl+comma
type_text "wrap"
shot settings 1.0
keys Escape
stop

# 7. Light theme with Markdown, split panes.
printf 'theme = "Apex Light"\n' > "$APEX_CONFIG_DIR/settings.toml"
start "$ROOT" "$ROOT/README.md" "$ROOT/crates/apex-ui/src/vim.rs"
keys ctrl+backslash
shot light-theme-split 1.5
stop

# 8. Vim mode.
printf 'vim_mode = true\n' > "$APEX_CONFIG_DIR/settings.toml"
start "$ROOT/crates/apex-edit/src/ops.rs"
type_text "20jVjj"
shot vim-mode 1.0
stop
echo "screenshots in $OUT"
