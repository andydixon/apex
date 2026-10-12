#!/usr/bin/env bash
# Type-check and lint (clippy, -D warnings) the macOS- and Windows-only code
# from Linux. Needs `rustup target add aarch64-apple-darwin
# x86_64-pc-windows-gnu`; C sources in build scripts are not compiled (stub
# compiler/archiver), so nothing is linked. Pass targets to check only some.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT
cat > "$STUB/cc" <<'SH'
#!/bin/sh
out=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift;; -o*) out="${1#-o}";; esac
  shift
done
[ -n "$out" ] && : > "$out"
exit 0
SH
cat > "$STUB/ar" <<'SH'
#!/bin/sh
for a in "$@"; do case "$a" in *.a) printf '!<arch>\n' > "$a"; break;; esac; done
exit 0
SH
chmod +x "$STUB/cc" "$STUB/ar"
cd "$ROOT"
TARGETS=("${@:-aarch64-apple-darwin}")
[ $# -eq 0 ] && TARGETS=(aarch64-apple-darwin x86_64-pc-windows-gnu)
for t in "${TARGETS[@]}"; do
  v="${t//-/_}"
  export "CC_$v=$STUB/cc" "CXX_$v=$STUB/cc" "AR_$v=$STUB/ar"
  CARGO_TARGET_DIR="$ROOT/target/check-$t" cargo clippy -q --target "$t" --workspace --all-targets -- -D warnings
  echo "$t: type check passed"
done
