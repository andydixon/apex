#!/usr/bin/env sh
# Build release artefacts for the current platform into ./dist.
set -eu
cd "$(dirname "$0")/.."
mkdir -p dist
case "$(uname -s)" in
  Linux)  sh packaging/linux/build.sh ;;
  Darwin) sh packaging/macos/build.sh ;;
  *)      echo "Use packaging/windows/build.ps1 on Windows" ; exit 1 ;;
esac
