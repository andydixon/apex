# Progress log

Each entry records what was done, how it was verified, and the exact next step
so work can resume cleanly in a new session.

## Milestone 0 — Research and design (complete)

* Confirmed toolchain: Rust 1.99 stable, edition 2024, eframe/egui 0.36.2
  (new `App::ui`/`App::logic` lifecycle; `CharIndex`/`ByteIndex` typed cursor
  indices; `fonts_mut().layout_job`; fixed-advance tabs in the shaper).
* Dependency evaluation and decisions: see `ARCHITECTURE.md` (decisions log).
* Repository scaffold: six-crate workspace, `Makefile`, GitHub Actions CI for
  Linux/Windows/macOS, AGPL-3.0-or-later licence, eight TOML themes, default
  key map.
* Documents: `ARCHITECTURE.md`, `FEATURE_MATRIX.md`, this file.

## Milestone 1 — Functional editor (complete)

Implemented:

* `apex-buffer`: rope re-export, offset conversions, grapheme/word/subword
  motion, line-ending scan/normalise, encoding detect/decode/encode.
* `apex-edit`: selections, transactions, history, document, 40+ editing
  operations, in-document search, bracket matching.
* `apex-project`: atomic file save/load, platform directories.
* `apex-ui`: theme engine, settings with layered TOML and diagnostics, command
  registry, key map with chords, view model, custom editor widget (layout
  cache, mouse selection, carets, gutter, scrollbars, IME output), application
  shell (tabs, split panes, menus, status bar, find/replace bar, dialogs for
  unsaved changes, external changes and save conflicts).
* `apex-app`: CLI (`apex file.rs:120:8`), logging, eframe window.

Verification so far (all run locally):

```
cargo test -p apex-buffer     16 passed
cargo test -p apex-edit       38 passed
cargo test -p apex-project     4 passed
```

Verified by the scripted Xvfb smoke test (`tests/visual/smoke.sh`): typing,
undo, quick-add, column selection, find with regex error, split panes, atomic
save; screenshots reviewed.

## Milestones 2–7 (largely complete; see FEATURE_MATRIX.md)

* Property tests for undo/redo, position mapping, grapheme safety, encodings.
* Syntax: `apex-syntax` with 30 grammars, injections, folding regions, symbols;
  all grammars load and their queries compile in a test.
* Workspace: file explorer, index, watcher, sessions, recovery, Find in Files,
  project files; palette and Goto Anything; macros; jump history.

Test counts at this point: 109 passing (`cargo test --workspace`); clippy clean
with `-D warnings`; `cargo fmt --check` clean.

Screenshots reviewed: Rust and Markdown highlighting, sidebar with the repo
open, Goto Anything with preview, command palette, Find in Files results.

Next step: Milestone 3 leftovers (word wrap, folding gutter UI, minimap), then
Git (Milestone 8), build tasks and LSP (Milestone 9).
