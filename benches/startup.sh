#!/usr/bin/env bash
# Whole-application measurements that need a window: start-up time to first
# frame, time to open large files, and resident memory afterwards.
#
#   benches/startup.sh [path/to/apex]      (default: target/release/apex)
#
# Runs under Xvfb with the lavapipe software Vulkan driver when no display is
# available, so numbers from CI are slower than on a desktop GPU. Each case
# starts a fresh process with an isolated config/data/runtime directory and
# reports the median of three runs. Output is plain text; paste it into
# docs/performance/benchmarks.md with the machine description.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/apex}"
[ -x "$BIN" ] || { echo "build first: cargo build --release (missing $BIN)"; exit 1; }
WORK="$(mktemp -d)"
trap 'kill $XPID 2>/dev/null || true; rm -rf "${WORK:?}"' EXIT

if [ -z "${DISPLAY:-}" ]; then
  export DISPLAY=:87
  export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json} WGPU_BACKEND=${WGPU_BACKEND:-vulkan}
  Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
  XPID=$!
  sleep 1
else
  XPID=""
fi

gen() { # name bytes line
  local p="$WORK/$1"; python3 - "$p" "$2" "$3" <<'PY'
import sys
p, n, line = sys.argv[1], int(sys.argv[2]), sys.argv[3] + "\n"
with open(p, "w") as f:
    reps = max(1, n // len(line))
    f.write(line * reps)
PY
  echo "$p"
}
F1K=$(gen 1k.rs 1024 "let value = compute(alpha, beta) + offset; // comment")
F100K=$(gen 100k.rs 102400 "let value = compute(alpha, beta) + offset; // comment")
F10M=$(gen 10m.rs 10485760 "let value = compute(alpha, beta) + offset; // comment")
F100M=$(gen 100m.rs 104857600 "let value = compute(alpha, beta) + offset; // comment")
FLINES="$WORK/million.txt"; python3 -c "print('\n'.join(f'line {i}' for i in range(1000000)))" > "$FLINES"
FLONG="$WORK/longline.txt"; python3 -c "print('\"item\", 1, 2.5, ' * 300000)" > "$FLONG"

measure() { # label file
  local label="$1" file="$2" times=() rss=""
  for _ in 1 2 3; do
    local cfg="$WORK/cfg-$RANDOM"; mkdir -p "$cfg"
    local start end wid=""
    start=$(date +%s%N)
    APEX_CONFIG_DIR="$cfg/c" APEX_DATA_DIR="$cfg/d" APEX_RUNTIME_DIR="$cfg/r" APEX_LOG=error "$BIN" --new-window "$file" >/dev/null 2>&1 &
    local pid=$!
    for _ in $(seq 1 600); do
      wid=$( (xdotool search --onlyvisible --pid "$pid" 2>/dev/null || true) | head -1)
      [ -n "$wid" ] && break
      sleep 0.05
    done
    end=$(date +%s%N)
    # Let the first frames and the file load settle, then read memory.
    sleep 2
    rss=$(awk '/VmRSS/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    times+=( $(( (end - start) / 1000000 )) )
  done
  local sorted; sorted=$(printf '%s\n' "${times[@]}" | sort -n | sed -n 2p)
  printf '%-28s window after %6s ms (runs: %s)   RSS after load %6s MB\n' "$label" "$sorted" "${times[*]}" "$(( rss / 1024 ))"
}

echo "apex $("$BIN" --version | awk '{print $2}')  $(uname -srm)  $(nproc) cpus  display $DISPLAY"
measure "empty window" "$WORK"
measure "1 KB file" "$F1K"
measure "100 KB file" "$F100K"
measure "10 MB file" "$F10M"
measure "100 MB file (large mode)" "$F100M"
measure "1,000,000 lines" "$FLINES"
measure "single 4.5 MB line" "$FLONG"
if [ "${APEX_BENCH_500MB:-0}" = "1" ]; then
  F500M=$(gen 500m.rs 524288000 "let value = compute(alpha, beta) + offset; // comment")
  measure "500 MB file (large mode)" "$F500M"
fi
