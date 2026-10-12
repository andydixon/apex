# Building and testing

## Toolchain

Stable Rust 1.91 or newer (`rust-toolchain.toml` pins `stable` with
`rustfmt` and `clippy`), a C compiler for the tree-sitter grammars and
libgit2, and on Linux the windowing development packages listed in the
README. No other system libraries: the GUI stack (egui, wgpu, winit), git
(libgit2, vendored) and the parsers are built from crates.

## Everyday commands

| Make target | Cargo |
|---|---|
| `make build` | `cargo build` (dev profile: `opt-level = 1`, dependencies at 3) |
| `make release` | `cargo build --release` (fat LTO, one codegen unit), then `packaging/package.sh` → `dist/` (`Apex.app` + DMG on macOS, tarball + `.deb` on Linux) |
| `make test` | `cargo test --workspace` |
| `make lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `make fmt` / `make check` | `cargo fmt --all` / `cargo fmt --all -- --check` |
| `make bench` | `cargo bench --workspace` (see `benches/README.md`) |
| `make package` | `packaging/package.sh` for the current platform |
| `make doc` | `cargo doc --workspace --no-deps` |

`tests/check-targets.sh` type-checks and lints the macOS- and Windows-only
code from Linux (needs `rustup target add aarch64-apple-darwin
x86_64-pc-windows-gnu`; nothing is linked).

CI (`.github/workflows/ci.yml`) runs format check, clippy, tests and a
release build on Linux, Windows and macOS.

## Test layers

1. **Unit tests** next to the code in every crate (`cargo test -p apex-edit`
   and so on). The editing core is the most heavily covered: transactions,
   history, selections, every operation in `ops.rs`, search, brackets,
   snippets, Vim mode.
2. **Property tests** (`crates/apex-edit/tests/properties.rs`, proptest):
   undo/redo round trips, position mapping, grapheme safety, encodings.
3. **Fuzz targets** (`fuzz/`, cargo-fuzz on nightly): transactions,
   offsets, settings/keymap/theme parsing, search, sessions.
4. **Scripted end-to-end runs** under Xvfb with xdotool
   (`tests/visual/smoke.sh`, plus `tests/visual/shot.sh FILE OUT.png [keys…]`
   for one-off screenshots and `tests/visual/screenshots.sh` for the
   documentation set). They type into the real window and check the file
   that was saved. Requirements: `Xvfb`, `xdotool`, ImageMagick and a Vulkan
   driver; `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
   WGPU_BACKEND=vulkan` selects the lavapipe software renderer.
5. **Manual smoke test** (`manual-smoke-test.md`) before a release, on each
   platform.

Every scripted run uses `APEX_CONFIG_DIR`, `APEX_DATA_DIR` and
`APEX_RUNTIME_DIR` so it never touches your real configuration or the
running editor's single-instance socket.

## Logging

`APEX_LOG=debug` (or `--log debug`) prints to stderr with `tracing`;
`trace` includes every language-server message. `APEX_FRAME_TIME=1` shows
the frame interval in the status bar.

## Conventions

* British English in UI text and documentation.
* No `unwrap`/`expect` on data that came from a file, a process or the
  network; return errors or clamp.
* Everything below `apex-ui` is independent of egui and testable headlessly.
* Keep `FEATURE_MATRIX.md` and `PROGRESS.md` truthful: a feature is "V" only
  when a test or scripted run exercises it.
