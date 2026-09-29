#!/usr/bin/env bash
# Build Linux artefacts into ./dist:
#   apex-<version>-linux-<arch>.tar.gz   binary, desktop entry, icons, licence, install.sh
#   apex_<version>_<deb-arch>.deb        when dpkg-deb is available
#   Apex-<version>-<arch>.AppImage       when appimagetool is on PATH
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"
case "$ARCH" in x86_64) DEB_ARCH=amd64 ;; aarch64) DEB_ARCH=arm64 ;; *) DEB_ARCH="$ARCH" ;; esac
DIST="${ROOT:?}/dist"
mkdir -p "$DIST"
[ -f assets/icons/png/apex-256.png ] || packaging/icons/generate.sh

cargo build --release -p apex-app
strip --strip-unneeded target/release/apex 2>/dev/null || true

# ---- staging tree (FHS layout under usr/) -------------------------------------
STAGE="${DIST:?}/stage"
rm -rf "${STAGE:?}"
install -Dm755 target/release/apex "$STAGE/usr/bin/apex"
install -Dm644 packaging/linux/apex.desktop "$STAGE/usr/share/applications/apex.desktop"
for s in 16 24 32 48 64 128 256 512; do
  install -Dm644 "assets/icons/png/apex-$s.png" "$STAGE/usr/share/icons/hicolor/${s}x${s}/apps/apex.png"
done
install -Dm644 assets/icons/apex.svg "$STAGE/usr/share/icons/hicolor/scalable/apps/apex.svg"
install -Dm644 LICENSE "$STAGE/usr/share/doc/apex/copyright"
install -Dm644 README.md "$STAGE/usr/share/doc/apex/README.md"
install -Dm644 packaging/linux/apex.metainfo.xml "$STAGE/usr/share/metainfo/cx.dixon.apex.metainfo.xml"

# ---- tarball ---------------------------------------------------------------------
TARNAME="apex-$VERSION-linux-$ARCH"
TARDIR="${DIST:?}/${TARNAME:?}"
rm -rf "${TARDIR:?}"
mkdir -p "$TARDIR"
cp -r "$STAGE/usr" "$TARDIR/"
cp packaging/linux/install.sh "$TARDIR/install.sh"
tar -C "$DIST" -czf "$DIST/$TARNAME.tar.gz" "$TARNAME"
rm -rf "${TARDIR:?}"

# ---- Debian package --------------------------------------------------------------
if command -v dpkg-deb >/dev/null; then
  PKG="${DIST:?}/deb"
  rm -rf "${PKG:?}"
  mkdir -p "$PKG/DEBIAN"
  cp -r "$STAGE/usr" "$PKG/"
  SIZE=$(du -sk "$PKG/usr" | cut -f1)
  cat > "$PKG/DEBIAN/control" <<CONTROL
Package: apex
Version: $VERSION
Section: editors
Priority: optional
Architecture: $DEB_ARCH
Installed-Size: $SIZE
Maintainer: Apex contributors <apex@dixon.cx>
Depends: libc6 (>= 2.31), libgcc-s1
Recommends: xdg-desktop-portal, hunspell-en-gb
Description: A native, high-performance text editor
 Apex is a Sublime-inspired editor written in Rust with multiple cursors,
 incremental syntax highlighting, project search, git awareness and an
 optional language-server client. It works entirely offline.
CONTROL
  cat > "$PKG/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q /usr/share/icons/hicolor || true
POST
  cp "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"
  chmod 755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"
  dpkg-deb --build --root-owner-group "$PKG" "$DIST/apex_${VERSION}_${DEB_ARCH}.deb"
  rm -rf "${PKG:?}"
fi

# ---- AppImage ------------------------------------------------------------------
if command -v appimagetool >/dev/null; then
  APPDIR="${DIST:?}/AppDir"
  rm -rf "${APPDIR:?}"
  mkdir -p "$APPDIR"
  cp -r "$STAGE/usr" "$APPDIR/"
  cp packaging/linux/apex.desktop "$APPDIR/apex.desktop"
  cp assets/icons/png/apex-256.png "$APPDIR/apex.png"
  ln -sf usr/bin/apex "$APPDIR/AppRun"
  ARCH="$ARCH" appimagetool "$APPDIR" "$DIST/Apex-$VERSION-$ARCH.AppImage"
  rm -rf "${APPDIR:?}"
else
  echo "appimagetool not found; AppImage skipped"
fi
rm -rf "${STAGE:?}"
ls -la "$DIST"
