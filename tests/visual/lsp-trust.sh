#!/usr/bin/env bash
# Language servers must not start in an untrusted folder (a server such as
# rust-analyzer runs the project's build scripts). Opens a Rust file under
# Xvfb with a fake "server" that only leaves a marker file, first in an
# untrusted folder (no marker allowed), then with the folder trusted
# (marker expected).
#
# Usage: tests/visual/lsp-trust.sh [output-dir]
# Requires: Xvfb, xdotool, a Vulkan software driver (lavapipe).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/target/lsp-trust}"
rm -rf "$OUT"
mkdir -p "$OUT/proj" "$OUT/config" "$OUT/data"
BIN="$ROOT/target/debug/apex"
cargo build -q --bin apex
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
export WGPU_BACKEND=vulkan
export APEX_CONFIG_DIR="$OUT/config" APEX_DATA_DIR="$OUT/data" APEX_RUNTIME_DIR="$OUT/run" APEX_LOG=warn
export DISPLAY=:${LSP_DISPLAY:-81}

MARKER="$OUT/server-started"
printf 'fn main() {}\n' > "$OUT/proj/main.rs"
# The "server" proves it ran by touching the marker, then idles.
cat > "$OUT/config/settings.toml" <<EOF
restore_session = false
[lsp_servers]
rust = ["sh", "-c", "touch '$MARKER'; sleep 30"]
EOF

Xvfb "$DISPLAY" -screen 0 1024x700x24 >/dev/null 2>&1 &
XPID=$!
APID=""
trap 'kill $APID 2>/dev/null; kill $XPID 2>/dev/null' EXIT
trap 'exit 1' INT TERM
sleep 1

run_once() {
  "$BIN" --new-window "$OUT/proj" "$OUT/proj/main.rs" > "$OUT/app-$1.log" 2>&1 &
  APID=$!
  for _ in $(seq 1 60); do
    xdotool search --onlyvisible --name "Apex" >/dev/null 2>&1 && break
    sleep 0.5
  done
  sleep 6
  kill "$APID" 2>/dev/null || true
  wait "$APID" 2>/dev/null || true
}

run_once untrusted
if [ -e "$MARKER" ]; then
  echo "FAIL: the language server started in an untrusted folder"
  exit 1
fi
echo "ok: no server in the untrusted folder"

# Trust the folder the way Project > Trust Workspace records it.
printf '["%s"]\n' "$OUT/proj" > "$OUT/data/trusted.json"
run_once trusted
if [ ! -e "$MARKER" ]; then
  echo "FAIL: the language server did not start once the folder was trusted"
  exit 1
fi
echo "ok: server started after trusting the folder"
