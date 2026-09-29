# macOS packaging

`packaging/macos/build.sh` produces `dist/Apex.app` and `dist/Apex-<version>-macos.dmg`.
Set `APEX_UNIVERSAL=1` for a universal (Apple Silicon + Intel) binary; both
Rust targets must be installed (`rustup target add aarch64-apple-darwin x86_64-apple-darwin`).

## "Open With" and document types

`Info.plist` declares text, source and script content types with
`LSHandlerRank Default`, every other file and folders as `Alternate`, so Apex
appears in Finder's **Open With** menu for any file without taking over
associations. macOS delivers those files as an `odoc` Apple Event rather than
command-line arguments; `crates/apex-app/src/macos.rs` registers a handler
with `NSAppleEventManager` before the window opens, and the editor polls the
resulting queue every frame. The same queue serves drops onto the Dock icon
and `open -a Apex file.rs`.

The handler compiles for `aarch64-apple-darwin` (checked with `cargo check`);
it has not been exercised on macOS hardware by the authors, so treat the first
run through Finder as a verification step (see `docs/development/manual-smoke-test.md`).

## Icons

`assets/icons/apex.icns` is generated from `assets/icons/apex.svg` by
`packaging/icons/generate.sh` (PNG payloads for 16 to 1024 px, plus the @2x
entries). `iconutil` is not needed.

## Signing and notarisation

The build script signs ad hoc (`codesign --sign -`) so the app launches on
Apple Silicon. For distribution outside your own machines:

```sh
# 1. Sign with a Developer ID certificate (hardened runtime, timestamp)
APEX_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" packaging/macos/build.sh

# 2. Notarise the DMG (store credentials once with `xcrun notarytool store-credentials`)
xcrun notarytool submit dist/Apex-*-macos.dmg --keychain-profile "apex" --wait

# 3. Staple the ticket so Gatekeeper works offline
xcrun stapler staple dist/Apex.app
xcrun stapler staple dist/Apex-*-macos.dmg
```

No entitlements are required: Apex does not use the sandbox, hardware
access or network APIs.

## Uninstall

Drag `Apex.app` to the Bin. Settings live in
`~/Library/Application Support/cx.dixon.Apex` (shown in **Help > About**) and can
be deleted separately.
