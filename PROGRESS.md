# Progress log

Each entry records what was done, how it was verified, and the exact next
step so work can resume cleanly in a new session. Test counts are copied
from real `cargo test` output; benchmark numbers live only in
`docs/performance/benchmarks.md` with the machine they were measured on.

## Milestone 0 — Research and design (complete)

* Confirmed toolchain: stable Rust (1.91+ required), edition 2024,
  eframe/egui 0.36.2 (new `App::ui` lifecycle; `CharIndex`/`ByteIndex` typed
  cursor indices; `fonts_mut().layout_job`; fixed-advance tabs in the shaper).
* Dependency evaluation and decisions: `ARCHITECTURE.md` (decisions log).
* Repository scaffold: six-crate workspace, `Makefile`, GitHub Actions CI for
  Linux/Windows/macOS, AGPL-3.0-or-later licence, eight TOML themes, default
  key map.

## Milestone 1 — Functional editor (complete)

`apex-buffer` (rope, offsets, motions, line endings, encodings), `apex-edit`
(selections, transactions, history, document, operations, search, brackets),
`apex-project` (atomic file I/O, platform directories), `apex-ui` (theme
engine, layered settings, command registry, key map with chords, view model,
custom editor widget, application shell with tabs, panes, menus, status bar,
find bar, dialogs), `apex-app` (CLI, logging, window).

Verified by unit tests and the scripted Xvfb smoke test
(`tests/visual/smoke.sh`): typing, undo, quick-add, column selection, find
with regex error, split panes, atomic save; screenshots reviewed.

## Milestone 2 — Editor correctness (complete)

Property tests (proptest) for undo/redo round trips, position mapping,
grapheme safety and encodings; 1,000-cursor test; selection normalisation.

## Milestone 3 — Rendering engine (complete)

Viewport-only layout with per-line cache and epoch invalidation, soft wrap
with visual-row motion, folding, minimap with throttled cache, indent guides,
rulers, whitespace rendering, large-line cap (20,000 chars), idle frames do
no work.

## Milestones 4–7 — Workspace, navigation, search, language features (complete)

Lazy project explorer, background index, file watcher, sessions, recovery,
Find and Replace in Files, project files, Goto Anything, command palette,
jump history, macros, prompts; tree-sitter highlighting for 30 grammars with
injections and background parsing, folding regions, symbols; snippets.

## Milestone 8 — Projects and git (complete)

libgit2 (vendored) status, gutter marks, branch label, hunk navigation and
revert, compare with index/HEAD/saved, file history, status palette.
Verified by screenshots (gutter marks, sidebar marks, branch label).

## Milestone 9 — LSP and build tools (complete)

Build tasks with trust gating, process groups, timeouts, ANSI sanitising and
problem matchers; LSP client over stdio with full-document sync, restart
backoff, stderr capture; completion, diagnostics, hover, signature help,
navigation, rename, code actions, formatting, workspace edits.

Verified against rust-analyzer under Xvfb: status/progress label, hover
tooltip for `Transaction` (screenshot), completion popup listing `elapsed()`
after typing `now.el` (screenshot), diagnostics count in the status bar. Two
bugs found and fixed during that verification: the hover request repeated
every frame, and a trigger character while a completion request was
outstanding never issued a new request (stale responses were also merged).
Responses now carry their request id.

## Milestone 10 — Polish and customisation (complete)

* Spell checking (Hunspell word lists, comments/strings by syntax scope,
  suggestions, user dictionary): `apex-project::spell` tests.
* Vim mode: `crates/apex-ui/src/vim.rs`, 12 engine tests covering motions,
  operators, text objects, registers, visual modes, insert variants, dot
  repeat, undo, search, ex commands, marks, multi-caret; scripted Xvfb
  session (`dw`, `jdd`, `A`, `V`, `:%s`, `:wq`) produced exactly the expected
  file. User guide: `docs/user-guide/vim-mode.md`.
* Settings editor: searchable panel for every key (coverage test
  `entries_cover_settings`), minimal `settings.toml`, live reload on save,
  `theme_mode = "auto"` implemented.
* Accessibility notes in `ARCHITECTURE.md`; high-contrast themes,
  `reduced_motion`, UI scaling; the editor text itself is not yet exposed to
  screen readers (documented limitation).
* Fixed: Ctrl+Q was bound to both Quit and macro recording.

## Milestone 11 — Performance and hardening (complete)

* Criterion benchmarks in `crates/*/benches` (offsets, multi-cursor editing,
  undo, transaction mapping, search over 10 MB, highlight of 100 KB and a
  2 MB line, load of 1 KB–100 MB and 1,000,000 lines, project search over
  2,000 files) and `benches/startup.sh` for start-up time and memory.
  Results: `docs/performance/benchmarks.md`.
* Fuzz targets (`fuzz/`): transactions, offsets, settings/keymap/theme,
  search, session/recovery. Built with cargo-fuzz on nightly; each target run
  for 60 s with no crashes (log in `docs/performance/benchmarks.md`).
* Large-file mode (threshold `large_file_mb`), profiling profile,
  `APEX_FRAME_TIME` readout.
* Two optimisations found by the benchmarks: injection layers are now
  re-queried only inside changed byte ranges (a keystroke in a 100 KB Rust
  file went from 12.9 ms to 0.25 ms of syntax work; test
  `injections_follow_edits_incrementally`), and `Transaction::map_pos` uses
  prefix sums with binary search (10,000 positions through 1,000 changes:
  58.7 ms to 0.4 ms).
* Security review: `docs/security/review.md`. One finding fixed during the
  review: project files could set `lsp_servers`; program-naming keys are now
  stripped from project settings (test `project_settings_cannot_name_programs`).

## Milestone 12 — Production release (in progress)

Done:

* Packaging: Linux tarball + `install.sh` + `.deb` (built and inspected on
  this machine; AppImage when `appimagetool` exists), macOS `Apex.app` with
  document types, icns, ad hoc signing and DMG, Windows zip and Inno Setup
  installer with Explorer integration; icon generated from
  `assets/icons/apex.svg` into PNG sizes, `.icns` and `.ico`.
* macOS "Open With": Apple Event handler (`crates/apex-app/src/macos.rs`),
  type-checked for `aarch64-apple-darwin`.
* Single instance with hand-over, `--new-window`, `--reuse-window`,
  `--wait`, `--diff` (unit test + scripted Xvfb session).
* Documentation: README with screenshots (`tests/visual/screenshots.sh`),
  user guide (11 pages), developer docs (building/testing, commands,
  grammars, release process), architecture, security review, benchmarks.
* CI builds packages on all three platforms and smoke-runs the fuzz targets.

Not done on the authors' machines (needs the platform):

* Running `packaging/macos/build.sh` and the Finder "Open With" path on macOS.
* Running `packaging/windows/build.ps1` and the Inno Setup installer on
  Windows.

Known gaps carried in `FEATURE_MATRIX.md`: one OS window per process,
screen-reader access to the editor text, whole-file loading.

## Test status

Last full run (`cargo test --workspace`, 12 October 2026):

| Crate | Passed |
|---|---|
| apex-app | 2 |
| apex-buffer | 16 |
| apex-edit | 42 (+ 8 property tests) |
| apex-project | 25 |
| apex-syntax | 15 (+1 ignored timing probe) |
| apex-ui | 38 |

`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo fmt --all -- --check` clean.

## Next step

Verify the macOS and Windows packaging scripts on those platforms (CI will
exercise them on the next push), then tag v0.0.1 following
`docs/development/release-process.md`.
