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

## Milestone 13 — Git and GitHub integration (complete)

Plan: `docs/plans/git-integration.md` (phases G1–G8). Reads stay on libgit2;
everything that writes or reaches a remote runs the user's `git` (or `gh`)
as a background job, trust-gated, with output in the GIT tab.

* G1 staging (file, hunk, selected lines via partial patches), Git menu.
* G2 Source Control panel in the lower sidebar; commit, amend, sign-off,
  undo commit.
* G3 side-by-side diff: two editor views, aligned scrolling, line and word
  tints, per-change revert/stage/unstage.
* G4 blame column, inline blame, commit view.
* G5 branches, log with lane graph, line history, stash, merge conflicts
  with continue/abort.
* G6 fetch/pull/push/sync/force-push with lease, askpass dialog.
* G7 remote links (GitHub, GitLab, Bitbucket, Gitea) and GitHub pull
  requests through `gh`.
* G8 Edit Commit History (requested): metadata/message editing, bulk author
  replace, date spread, co-author removal, Claude/Codex filter.

Verification: unit tests against real `git` in temporary repositories
(partial staging round trips, askpass via `git credential fill`, history
rewrite and backup tag, merge state and conflict parsing, log graph);
`tests/visual/git.sh` drives the real editor under Xvfb through staging,
commit/amend, blame, side-by-side staging, push/pull against a local bare
remote, GitHub flows against a fake `gh`, branches, stash, a full conflict
resolution and a history rewrite, checking every result with `git`.
Not verified here: real GitHub (needs `gh auth login`). The current macOS
file-dialog status is recorded under Milestone 14.

Fixed along the way: the backup tag of a history rewrite was itself
rewritten (Nexus had the same flaw); remote-tracking refs are no longer
rewritten; Ctrl+Enter used the frame's modifiers instead of the event's;
exact command names now win in the palette; Finder "Open With" handler is
installed after AppKit's own; the Dock icon is Apex's.

## Milestone 14 — macOS dialogs, comments, guide and security audit (complete)

* macOS Open, Open Folder and Save As retain the AppKit sheet backend
  queued through `CFRunLoopPerformBlock` (`macos_dialogs.rs`). Null panels
  and explicit panel aborts return an error to the existing path prompt;
  ordinary cancellation does nothing. A system panel-service stall can
  still block AppKit during panel creation.
  Live investigation on 13 October 2026 found panel creation waiting for
  60 seconds before returning null in Apex; plain Cocoa probes also
  stalled, and the user reported Webex hanging and crashing on Open.
  After a macOS reboot, the original Apex native Open dialog worked in a
  live test, confirmed by the user. This supports a shared macOS service
  fault; its exact cause remains unestablished.
  Live checks of the final debug backend opened two selected files into
  two tabs without an AccessKit crash, saved the exact expected contents
  through native Save As, and opened a folder with its sidebar populated.
  Cancelling native Save As preserved dirty text. Closing a dirty tab,
  choosing Save and cancelling the native panel preserved the tab; choosing
  a native save path wrote the expected contents and then closed it.
  Native Open Project showed its single-file sheet and cancelled quietly;
  a subsequent native Open request also opened and cancelled successfully.
  The rebuilt, signed `dist/Apex.app` selected a file through native Open
  and wrote the exact expected contents through native Save As. The macOS
  package script also produced `dist/Apex-0.0.1-macos.dmg`.
  Save Project As now uses the same native-first routing. With the setting
  omitted, its native Save panel wrote a project at the exact selected path
  (including a name without an extension); Open Project loaded its roots.
  Cancelling the native Save panel wrote no file. With
  `native_file_dialogs = false`, Open and Save Project As used the path
  prompt, and saving a new project path wrote the expected TOML file.
  Built-in defaults and configuration layering have a regression test.
  AppKit null-panel and abort fault injection, and Finder/Dock integration
  checks remain pending.
* Returning focus after a path prompt opens a view now creates its
  AccessKit node before requesting focus. A headless regression test
  checks that the focused node exists in the accessibility update.
* Every module has a header, and types and functions carry doc comments.
* PDF user guide in LaTeX (`make guide`, `docs/user-guide/latex/`).
* Security audit (`audits/20261013-0712-*`): language servers now need a
  trusted folder; run-file shell injection; device and FIFO loading;
  private temporary files; CSPRNG tokens; `--` before ref names; git2 0.21.
  New checks: `tests/visual/lsp-trust.sh` and five unit tests. Also fixed:
  whole-document sort moved the final newline to the top.
* `tests/visual/git.sh`: all 16 checks pass on git2 0.21.

## Test status

Last full run (`cargo test --workspace`, 13 October 2026, after the dialog
changes): 184 passed, one timing probe ignored. The successful run set
`TMPDIR` to its canonical physical path. With the default macOS `TMPDIR`,
two existing apex-project Git alias tests repeat failures caused by
`/var` versus `/private/var` path spelling; those failures are unresolved.

| Crate | Passed |
|---|---|
| apex-app | 2 |
| apex-buffer | 16 |
| apex-edit | 43 (+ 8 property tests) |
| apex-project | 51 |
| apex-syntax | 15 (+1 ignored timing probe) |
| apex-ui | 49 |

`cargo clippy --workspace --all-targets -- -D warnings` passed on 13 October
2026. `cargo fmt --all -- --check` and `git diff --check` passed too.

## Next step

1. macOS: test reported panel failures with fault injection, Finder "Open With" and the Dock icon using
   `docs/development/manual-smoke-test.md`.
2. Try the GitHub features against a real repository after `gh auth login`.
3. Tag the next release following `docs/development/release-process.md`.
