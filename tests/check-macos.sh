#!/usr/bin/env bash
# Type-check (cargo check + clippy) the macOS-only code from Linux.
# Needs only `rustup target add aarch64-apple-darwin`: C sources in build
# scripts are not compiled (stub compiler/archiver), so nothing is linked.
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
export CC_aarch64_apple_darwin="$STUB/cc" CXX_aarch64_apple_darwin="$STUB/cc" AR_aarch64_apple_darwin="$STUB/ar"
export CARGO_TARGET_DIR="$ROOT/target/mac-check"
cd "$ROOT"
cargo clippy --target aarch64-apple-darwin --workspace -- -D warnings
echo "macOS type check passed"
