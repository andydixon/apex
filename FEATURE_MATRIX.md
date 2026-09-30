# Feature matrix

Status key: **V** implemented and verified by automated tests or scripted
end-to-end runs · **I** implemented, verified manually (screenshots or
interactive use) · **P** partially implemented, limits noted · **–** not
implemented.

Updated: 12 October 2026, after Milestones 10–12 (Vim mode, settings editor,
packaging, benchmarks, fuzzing, documentation).

## Document engine (`apex-buffer`, `apex-edit`)

| Feature | Status | Notes |
|---|---|---|
| Rope storage, insert/delete/replace | V | ropey; transactions in pre-edit coordinates |
| Byte/char/line/UTF-16/display-column conversion | V | fuzzed (`offsets`) |
| Grapheme-aware motion (emoji, ZWJ, combining, CRLF) | V | chunk-aware over large ropes |
| Word, subword, paragraph and bracket motion | V | |
| Revision tracking, multi-view selection resync | V | ring of recent transactions |
| Transactions with inverse and byte edits for tree-sitter | V | fuzzed (`transactions`: apply/undo round trip) |
| Undo/redo with typing coalescing, bounded memory, selection restore | V | linear redo (documented) |
| Multi-cursor atomic edits | V | 1,000-cursor test and benchmark |
| Selection normalisation (overlap merge, dedupe, primary tracking) | V | |
| Bookmarks and named marks following edits | V | |
| 40+ editing operations (lines, case, comments, sort, wrap, surround, sequences…) | V | unit-tested in `ops.rs` |
| Snippets with tab stops, choices, variables, nested placeholders | V | |
| In-document search: regex with size limits, whole word, case, in-selection, replacement templates, case preservation | V | fuzzed (`search`) |
| Property-based tests (undo, mapping, graphemes, encodings) | V | proptest |

## File handling (`apex-project::fileio`)

| Feature | Status | Notes |
|---|---|---|
| New / Open / Open multiple / Save / Save As / Save All / Revert | V | smoke test saves and checks the file |
| Open recent, reopen closed tab | I | recent list persisted privately |
| Close tab / others / to right / all, confirm on unsaved | I | |
| Drag-and-drop to open | I | |
| Encodings: UTF-8, UTF-8 BOM, UTF-16 LE/BE, legacy set; detect, reload with, save with, lossy warning | V | |
| Line endings: detect, preserve, convert, mixed flag | V | |
| Binary detection and read-only hex preview | V | |
| Atomic save (temp + fsync + rename), permission/ownership preservation, symlink write-through | V | |
| External modification detection, reload/keep, save conflict dialog | I | checked on focus and every 2 s |
| Deleted-on-disk handling, read-only files | I | |
| Large-file mode (over `large_file_mb`): no highlighting/minimap/wrap/spell | I | whole file in memory; 2 GiB cap |
| Rename / move / delete (to bin, confirmed) / duplicate, new file/folder | I | |
| Folders as workspace roots, multiple roots | I | `apex .` or drag a folder in |
| Project files (folders, `[settings]`, `[[tasks]]`) | V | program-naming keys blocked from project settings (tested) |

## Editor interface (`apex-ui`)

| Feature | Status | Notes |
|---|---|---|
| Custom viewport widget, visible-line layout cache, no per-line widgets | V | |
| Tabs: dirty dot, close, middle-click, overflow, context menu, drag reorder and between panes, pin, preview tabs | I | |
| Split panes (2/3 columns, rows, 2×2, nested), draggable dividers, duplicate view | V | smoke test splits |
| Status bar: position, selection summary, indent, encoding, EOL, language, git, LSP, spell, Vim mode, problems | I | |
| Menu bar, context menus, about dialog | I | |
| Find/replace bar: regex, case, whole word, in selection, match count, highlight in text/scrollbar/minimap, regex errors | V | smoke test |
| Themes: 8 built-in, user TOML themes, live switching, `theme_mode = "auto"` follows the system | I | parsers fuzzed |
| Zoom, UI scale, line numbers, relative numbers, current line, rulers, indent guides, whitespace | I | |
| Bracket matching highlight, auto-pair, smart backspace, auto-indent, indentation detection | V | |
| Distraction-free / zen / low-distraction / fullscreen modes, typewriter scrolling | I | |
| Minimap (token colours, viewport, click/drag, selection/search/git markers), throttled | I | |
| Word wrap (visual-row caret motion, per-view toggle, wrap column) | I | |
| Code folding (syntax/indent regions, gutter, current/all/levels/comments) | I | selections never edit hidden text |
| Sidebar / project explorer (lazy tree, filter, follow active, rename, context menu, git status) | I | 100k-file trees stay lazy |
| Command palette (fuzzy, shortcuts, recent weighting, prompts for arguments) | V | nucleo matcher |
| Goto Anything (`file`, `file:line`, `:line`, `@symbol`, `#project`, `>command`), instant preview, cancel restores | V | |
| Symbols from tags queries, project-wide symbol index, breadcrumbs | I | |
| Jump back / forward history | I | |
| Syntax highlighting: tree-sitter, 30 grammars, incremental, injections, background parse for large files | V | Kotlin query in-house; Dockerfile via declarative fallback |
| Language detection (name, extension, shebang, content) and per-file override | V | |
| Expand/shrink selection by syntax scope | I | |
| Find in Files: streaming, cancellable, include/exclude, gitignore, hidden, grouped results, context | V | benchmark `find_in_files` |
| Replace in Files with per-file preview, external-change detection, partial-failure report | I | not atomic across files |
| Session restore (tabs, selections, scroll, layout, roots, bookmarks) | V | `session.json`, fuzzed |
| Crash recovery snapshots and dialog | V | snapshots every 5 s; `--wait`/IPC scripts exercise shutdown |
| Macros: record, replay, save named, run | I | structured command steps only |
| File watcher: sidebar/index refresh, external changes | I | |
| Settings editor: searchable panel, immediate apply, minimal `settings.toml`, live reload on save | V | coverage test for every key |
| Completion popup: words, snippets, language server; merged and ranked | I | |
| Spell checking (prose, comments, strings), suggestions, user dictionary | V | tokeniser and dictionary tests |
| Vim mode: normal/insert/visual/visual-line/replace, counts, operators, text objects, registers, marks, search, `.`, `:` commands | V | 12 engine tests + scripted Xvfb session |
| Print (File ▸ Print…) | I | plain text to the default printer via `lp`/`lpr` (Unix, macOS) or PowerShell `Out-Printer` (Windows); no page dialog |
| Screen-reader output for the editor text | – | AccessKit exposes menus, buttons and dialogs; the custom text widget is not yet described (see ARCHITECTURE.md › Accessibility) |

## Multi-cursor

| Feature | Status | Notes |
|---|---|---|
| Ctrl+click add/remove, Alt+drag column, Shift+click extend | I | |
| Double/triple-click word/line and drag | I | |
| Add cursor above/below, quick add next/skip/all, undo selection | V | smoke test |
| Split into lines, column select by keyboard | V | |
| Paste one line per cursor | V | |
| Vim motions/operators across all carets | P | motions move secondary carets by the primary's delta |

## Git (`apex-project::git`)

| Feature | Status | Notes |
|---|---|---|
| Repository discovery, branch/detached label | I | libgit2 vendored |
| Gutter marks (added/modified/deleted) against index or HEAD, live while typing | I | |
| Sidebar file status | I | |
| Next/previous change, revert hunk (confirmed), show hunk diff | I | |
| Compare with saved/index/HEAD, `--diff` of two files | V | IPC script opens a diff tab |
| File history, open a past version, status palette | I | |

## Build tasks and language servers

| Feature | Status | Notes |
|---|---|---|
| Detected tasks (cargo, go, npm, make, cmake, pytest, run-file) | I | |
| Project-file tasks with trust gating remembered outside the repository | I | |
| Process groups, cancel, timeout, ANSI/OSC sanitising | V | unit tests |
| Problem matchers (rustc, gcc, go, python, generic, custom regex), Problems panel | V | |
| LSP client: stdio JSON-RPC framing, size limits, restart backoff, stderr capture | V | framing tests |
| Diagnostics, completion (trigger characters, stale-response handling), hover, signature help | V | verified against rust-analyzer under Xvfb (screenshots) |
| Definition/references/declaration/implementation, document/workspace symbols | I | |
| Rename, code actions, formatting (and format on save), workspace-edit confirmation | I | |
| Known servers started from PATH; user-configured servers | I | |

## Key bindings and commands

| Feature | Status | Notes |
|---|---|---|
| Command registry with stable ids | V | 240+ commands |
| TOML key maps, chords, contexts, per-platform sections, conflict detection, physical-key fallback | V | fuzzed with the settings target |
| User overrides from `<config>/keymap.toml` | I | |

## Command line, instances, platform integration

| Feature | Status | Notes |
|---|---|---|
| `apex file:line:col`, folders, multiple files, paths with spaces | V | |
| Single instance: second invocation hands over paths; `--new-window`; `--reuse-window` | V | IPC unit test + Xvfb script |
| `--wait` until the tab closes | V | |
| `--diff old new` | V | |
| macOS "Open With" / Dock / `open -a` via Apple Events | P | handler compiles for `aarch64-apple-darwin`; not yet exercised on macOS hardware |
| Windows "Open with Apex" context menu, associations, PATH (installer) | P | Inno Setup script written; installer not yet built on Windows by the authors |
| Multiple OS windows sharing one application state | – | one window per process; `--new-window` starts a separate instance (documented limitation) |

## Packaging and distribution

| Feature | Status | Notes |
|---|---|---|
| Linux tarball + `install.sh`, `.deb` | V | built and inspected (`packaging/linux/build.sh`) |
| AppImage | P | produced when `appimagetool` is present; not built by the authors |
| Desktop entry, AppStream metadata, hicolor icons (16–512, SVG) | V | |
| macOS `Apex.app` with document types, icns, ad hoc signature, DMG | P | script written; needs a macOS host to run |
| Windows portable zip, Inno Setup installer, embedded icon and version resource | P | script written; needs a Windows host to run |
| Icon generation from SVG (PNG sizes, `.icns`, `.ico`) | V | `packaging/icons/generate.sh`; files committed |
| CI on Linux, Windows, macOS: fmt, clippy, tests, release build, packaging scripts | I | `.github/workflows/ci.yml` |

## Performance and hardening

| Feature | Status | Notes |
|---|---|---|
| Criterion benchmarks (offsets, editing, search, highlight, load, project search) | V | `cargo bench --workspace`; results in `docs/performance/benchmarks.md` |
| Whole-application start-up/large-file/memory script | V | `benches/startup.sh` |
| Fuzz targets (transactions, offsets, settings, search, session) | V | `fuzz/`, run log in `docs/performance/benchmarks.md` |
| Profiling profile, frame-time readout | V | `--profile profiling`, `APEX_FRAME_TIME=1` |
| Security review | V | `docs/security/review.md` |

## Known limitations

* Lines beyond 20,000 characters are not laid out past that point (they
  are editable; the rest of the line is not drawn).
* Files are loaded whole; a 500 MB file needs that much memory.
* Vertical caret motion is column-based; with a proportional editor font it
  is approximate.
* The spell checker does not apply Hunspell affix rules.
* The custom text widget is not yet exposed to screen readers.
* No visual block mode in Vim mode; no `q` macros (use the editor's).
* One OS window per process.
* macOS and Windows packaging and the macOS Open With handler have not been
  run by the authors on those platforms.
