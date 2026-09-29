#!/usr/bin/env bash
# Build Apex.app and a DMG into ./dist.
#
#   packaging/macos/build.sh              native architecture
#   APEX_UNIVERSAL=1 packaging/macos/build.sh
#                                         universal binary (needs both
#                                         aarch64-apple-darwin and x86_64-apple-darwin
#                                         targets installed via rustup)
#   APEX_SIGN_IDENTITY="Developer ID Application: …" packaging/macos/build.sh
#                                         sign with that identity instead of ad hoc
#
# The bundle declares document types (Info.plist) so Finder's "Open With" lists
# Apex; the paths reach the editor through the Apple Event handler in
# crates/apex-app/src/macos.rs. See README.md here for notarisation.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
DIST="${ROOT:?}/dist"
APP="${DIST:?}/Apex.app"
mkdir -p "$DIST"
rm -rf "${APP:?}"

if [ "${APEX_UNIVERSAL:-0}" = "1" ]; then
  cargo build --release --target aarch64-apple-darwin -p apex-app
  cargo build --release --target x86_64-apple-darwin -p apex-app
  mkdir -p "$DIST/universal"
  lipo -create -output "$DIST/universal/apex" \
    target/aarch64-apple-darwin/release/apex target/x86_64-apple-darwin/release/apex
  BIN="$DIST/universal/apex"
else
  cargo build --release -p apex-app
  BIN="target/release/apex"
fi

[ -f assets/icons/apex.icns ] || packaging/icons/generate.sh

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/apex"
chmod 755 "$APP/Contents/MacOS/apex"
cp assets/icons/apex.icns "$APP/Contents/Resources/apex.icns"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
cp LICENSE "$APP/Contents/Resources/LICENSE"

# Unsigned binaries do not launch on Apple Silicon; an ad hoc signature is the minimum.
if command -v codesign >/dev/null; then
  if [ -n "${APEX_SIGN_IDENTITY:-}" ]; then
    codesign --force --deep --options runtime --timestamp --sign "$APEX_SIGN_IDENTITY" "$APP"
  else
    codesign --force --deep --sign - "$APP"
  fi
fi

if command -v hdiutil >/dev/null; then
  STAGE="${DIST:?}/dmg"
  rm -rf "${STAGE:?}"
  mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "Apex $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DIST/Apex-$VERSION-macos.dmg"
  rm -rf "${STAGE:?}"
fi
echo "built $APP"
ls -la "$DIST"
