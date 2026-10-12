# Apex architecture

This document records the significant design decisions. The authoritative
state of each feature is in `FEATURE_MATRIX.md` and the milestone log is in
`PROGRESS.md`.

## Crate layout

Dependencies flow strictly towards the core; nothing below `apex-ui` knows
about egui, so the engine, parsers, search, git and the Vim interpreter are
all tested headlessly.

| Crate          | Responsibility                                                                 | Depends on                   |
|----------------|--------------------------------------------------------------------------------|------------------------------|
| `apex-buffer`  | Rope text store (ropey), offset-space conversions, grapheme/word motion, line endings, encodings | ropey, unicode-*, encoding_rs |
| `apex-edit`    | `Document` (text + revision + history + metadata), `Selection`/`SelectionSet`, `Transaction`, editing operations, in-document search, bracket matching, snippets | apex-buffer, regex |
| `apex-syntax`  | Tree-sitter grammars, incremental highlighting with injections, folding, symbol extraction, declarative fallback syntaxes | apex-edit, tree-sitter |
| `apex-project` | File I/O with atomic saves, platform paths, file index and watcher, Find in Files, git, sessions and recovery, build tasks, LSP client, spell dictionaries | apex-edit, ignore, notify, git2, serde |
| `apex-ui`      | egui editor widget, panes/tabs, menus, find bar, palette, sidebar, settings editor, themes, key maps, Vim mode, git/tasks/LSP/spell UI | all of the above, egui |
| `apex-app`     | Binary: CLI parsing, logging, single-instance IPC, macOS Apple Events, window creation | apex-ui, eframe, clap, objc2 (macOS) |

The spec suggested seventeen crates; six keep the same boundaries without
empty shells. Within `apex-ui` each feature is a module adding methods to
`ApexApp` (`git_ui.rs`, `tasks_ui.rs`, `lsp_ui.rs`, `spell_ui.rs`,
`vim_ui.rs`, `settings_ui.rs`), and `execute()` tries each module's
`*_command` in turn before the core editing commands.

## Offset spaces

The canonical document position is a **char offset** (Unicode scalar index),
which is what ropey indexes natively. All other spaces are derived on demand:

* bytes — tree-sitter, regex, file I/O (`Rope::char_to_byte`, O(log n));
* grapheme clusters — caret motion and deletion (`apex_buffer::motion`, using
  `unicode-segmentation` over rope chunks so huge lines never get copied);
* display columns — vertical caret motion, column selection, rulers
  (`apex_buffer::position`, tabs expanded to the next stop, East Asian wide
  characters count 2, combining marks 0);
* UTF-16 code units — LSP (`char_to_utf16_position` and its inverse, clamped so
  a misbehaving server can never index out of range).

Policy: carets may only rest on grapheme boundaries; every operation that
produces a selection snaps through `Selection::clamped`. Vertical motion uses
display columns (not pixels), so it is exact for monospace fonts and
approximate for proportional fonts, which the user guide states.

## Document model

```
Document  = rope + revision + History + file metadata (+ bookmarks, marks)
View      = DocId + SelectionSet + scroll + folds + layout cache + drag state
Pane      = ordered tabs (ViewIds) + active index + pinned set + preview tab
Layout    = Leaf(PaneId) | Split { axis, children, fractions }
ApexApp   = docs, views, panes, layout, settings, theme, keymap, dialogs,
            feature state (git, tasks, lsp, spell, vim, settings panel)
```

Several views may show one document. The document keeps a bounded ring of
recently applied transactions tagged with revision numbers; a view that was
not the author of an edit remaps its selections through those transactions
(`Document::sync_selections`) the next time it is drawn. If a view falls
further behind than the ring covers, its selections are clamped instead, which
cannot corrupt anything because selections are only ever validated, never
trusted.

### Transactions

A `Transaction` is a sorted, disjoint list of `Change { start, end, text }` in
**pre-edit** char coordinates. It is applied last-to-first so earlier offsets
stay valid, yields its own inverse in post-edit coordinates, and emits byte
level `ByteEdit`s in the order a tree-sitter tree must be told about them.
Overlapping changes (which cannot arise from a normalised selection set) are
dropped deterministically with the earlier-starting change winning, and a
warning is logged.

`Transaction::map_pos(pos, Assoc)` maps positions across the edit; `Assoc`
chooses which side of an insertion a position sticks to. Operations compute
their post-edit selections either directly (typing, where the caret lands after
each inserted run) or by mapping, so 1,000 cursors stay consistent. The
`transactions` fuzz target checks apply → undo restores the text byte for
byte and that mapped positions stay in range.

### History

Undo entries hold `Vec<(forward, inverse)>` steps plus selections before and
after. Consecutive typing (and consecutive backward/forward deletes) within
one second with an unchanged caret count coalesce into one entry; any other
edit, a caret move or an explicit boundary (Vim insert sessions, snippet
expansion) seals the group. Redo is linear: a new edit after an undo discards
the redo stack. History is bounded by entry count and by retained text bytes
(default 10,000 entries / 64 MiB) and trims the oldest.

## Rendering

The editor is a custom widget (`apex_ui::editor`), not `egui::TextEdit`.

* Input and painting are separate passes. Input converts mouse and keyboard
  events into `Action`s; the application executes those (possibly mutating the
  document) and only then paints, so each frame shows the post-edit state.
* Only lines intersecting the viewport (plus one) are laid out. Each line is a
  single `egui::Galley` produced from a `LayoutJob` carrying syntax sections;
  galleys are cached per line in `LayoutCache`, invalidated by an epoch that
  bumps on any document, font, theme, wrap-width or fold change.
* Highlighting is pulled lazily per visible line through a closure over the
  document's `SyntaxState`, so layout never waits for lines that are not on
  screen.
* Tabs are expanded to spaces for layout (egui's shaper has fixed-advance
  tabs); a per-line `doc→galley` column map keeps hit-testing exact. Control
  characters are drawn as Unicode control pictures.
* Soft wrap uses egui's wrapping with a per-view width; folds are char anchors
  in the view, and `next_visible_line`/`prev_visible_line` skip hidden lines
  so motions, selections and painting agree.
* Lines longer than 20,000 chars are laid out only to that limit and marked as
  truncated; the rest stays editable through keyboard navigation.
* The minimap draws from a throttled shape cache rebuilt at most a few times a
  second, so it never costs a full relayout per keystroke.
* Idle frames do no layout work: egui only repaints on input, when a
  background task posts a result, or on the 0.5 s caret-blink wake-up while
  an editor is focused.

## Input pipeline

```
egui events ─► Vim interpreter (when vim_mode, editor focused) ─► consumed?
            └► editor widget: Keymap::dispatch_event(key, physical, modifiers, contexts) ─► Command
            └► global contexts (find bar, dialogs, palette) ─► Command
Command + arg ─► ApexApp::execute ─► feature modules ─► editing core
```

Chords (`ctrl+k ctrl+b`) are supported via pending state; modifier-only
presses never break a chord. Logical keys are matched first with a
physical-key fallback for layouts that move punctuation. A text event that
immediately follows a bound key press is swallowed so `Alt+Q` does not also
type "q". Cut/Copy/Paste arrive as dedicated egui events and route to the
same commands as the menus. IME composition is shown via egui's `IMEOutput`
with the caret rectangle, and commits arrive as text.

## Command system

Every user action is a `Command` variant with a stable id, name and category
(`command.rs`, `commands!` macro). Menus, key bindings, the palette, the
context menu, macros and Vim `:` commands all go through `execute(cmd, arg)`,
so each command has exactly one implementation. Macros store command ids and
typed text, which is why they cannot become a scripting system.

## Vim mode

`vim.rs` is a modal interpreter over a `VimHost` trait (document, selections,
edit, command, status, clipboard). The application implements the trait with
`run_edit`/`execute`; tests implement it with a bare `Document`. Keys reach
the interpreter before the key map; it consumes what it understands and
leaves `Ctrl` shortcuts, the palette and dialogs alone. The editor's own
selection model is reused: visual mode is an inclusive selection, operators
act on char ranges, registers are a map, `.` replays recorded keys.

## Worker model

The UI thread never blocks on I/O or parsing. Background work uses plain
threads and `crossbeam-channel`, with the UI polling receivers once per frame
(`poll_background`, `lsp_tick`, `git_poll`, `tasks_poll`) and asking egui to
repaint when something arrives:

| Work | Where | Cancellation / bounding |
|---|---|---|
| File index for Goto Anything and Find in Files | `index::start_scan` (ignore walker) | generation id; superseded scans dropped |
| Find in Files | `search::start` | cancel flag, result cap, binary/large skips |
| Tree-sitter parse of large documents | `SyntaxState` background thread | per-frame budget in the foreground; generation id + edits-since-parse for the background result |
| Git status and hunks | `git::start_status` | debounced; results tagged with the repo path |
| Build tasks | `tasks::run` (process groups) | cancel kills the group; timeout |
| LSP server I/O | reader thread per server | message size limit; restart backoff |
| File watcher | `notify` | coalesced into a rescan timer |
| Second invocation / Apple Events | `open_requests` channel | polled every frame |
| Native file dialogs | `file_dialogs.rs`: worker thread running rfd (Linux, Windows) or `osascript` in its own process (macOS) | one at a time; no AppKit panel is ever run from inside winit's event handler |

Results are tagged with the generation or revision they were computed for and
discarded when stale, so a slow worker can never overwrite newer state.

## Syntax engine

`apex-syntax` compiles 30 tree-sitter grammars in. Each `LanguageDef`
carries highlight, injection and tags queries plus fold node kinds. Parsing is
incremental: byte edits from each transaction are applied to the old tree,
and the parser runs with a time budget per frame, finishing on a background
thread for large documents. Highlight spans are computed per line on demand
and cached; injection layers (Markdown code fences, HTML script/style) are
parsed one level deep and painted over the host layer. Symbols come from the
tags queries; folding from the tree or, without one, from indentation.
Languages without a grammar use a small declarative syntax (`SimpleSyntax`).

## Persistence

| Data | Location | Format | Notes |
|---|---|---|---|
| Settings, key map, themes, snippets, dictionaries | config dir | TOML / text | layered: defaults → user → project `[settings]` |
| Session | data dir `session.json` | JSON | atomic write, 0600, versioned, damaged files renamed |
| Recovery snapshots | data dir `recovery/` | JSON | every `recovery_interval_ms` for dirty buffers |
| Recent files/projects, macros, trust decisions | data dir | JSON | private directory |
| Single-instance endpoint | runtime dir `instance.json` | JSON | port + token + pid, 0600 |

All writes go through `fileio::save_atomic`.

## File handling

`apex_project::fileio::save` writes to a `create_new` temporary file in the
target directory, fsyncs, copies permissions (and ownership where permitted),
renames over the destination and fsyncs the directory. Symlinks are resolved
first so the link is preserved and the target rewritten. Hard links cannot be
preserved by a rename-based save; in-place writing is available as an option.
Before writing, the on-disk identity (size, mtime, and inode/device on Unix)
is compared with what was loaded; a mismatch refuses the save and the UI
offers Overwrite / Save As / Cancel.

Binary detection (NUL byte in the first 8 KiB, unless the bytes look like
UTF-16) opens a read-only hexadecimal preview instead of decoding.

## Platform integration

* **Single instance**: `apex-app/src/ipc.rs`; loopback TCP with a token file
  (works identically on all three platforms). `--wait` keeps the client
  connection open until the editor reports the tab closed.
* **macOS**: Finder "Open With", Dock drops and `open -a` deliver files as
  `odoc` Apple Events, which winit does not forward; `macos.rs` registers an
  `NSAppleEventManager` handler before the event loop starts. Document types
  are declared in `packaging/macos/Info.plist`.
* **Windows**: `build.rs` embeds the icon and version resource with
  `winresource`; the Inno Setup script adds the Explorer context menu entries.
* **Linux**: desktop entry, AppStream metadata and hicolor icons are installed
  by the tarball's `install.sh`, the `.deb` and the AppImage.

## Accessibility

eframe enables AccessKit, so menus, buttons, text fields, dialogs and the
sidebar tree are exposed to screen readers with roles and names, keyboard
focus is visible, and the whole interface scales with `ui_scale` and the font
settings. High-contrast dark and light themes are built in, and
`reduced_motion` disables animations and smooth scrolling. Diagnostics use
colour plus underline style (wavy for errors/warnings, dotted for spelling)
and text in the Problems panel.

Limitation: the custom editor widget does not yet publish its text, caret and
selection through AccessKit, so screen readers cannot read the document
itself. egui's AccessKit integration exposes only built-in widgets; doing this
for the editor needs a per-line text node tree with character boxes, which is
tracked as future work and documented in `FEATURE_MATRIX.md`.

## Security boundaries

See `docs/security/review.md` for the full review. In short:

* Files, project files, process output and server messages are untrusted:
  decoding never panics, positions are clamped, regexes are size-limited,
  escape sequences are stripped.
* Project-file tasks run only after the workspace is trusted; the decision is
  stored outside the repository. Project `[settings]` cannot name programs.
* Configuration is data (TOML) and cannot execute code.
* Sessions, recovery, recent lists and the instance endpoint live in private
  directories; the IPC endpoint requires a random token.
* Nothing in the editor contacts the network.

## Decisions log

* **ropey 1.6 over 2.0-beta** — stable API, no risk from a beta in the core.
* **git2 (vendored libgit2) over gix** — simpler status/diff API; the build
  stays self-contained. Only local operations are used.
* **rfd with the XDG portal backend on Linux** — native dialogs without a GTK
  build dependency.
* **Vertical motion by display column** rather than pixel x — deterministic,
  testable, and exact for the monospace fonts a code editor uses.
* **Whole-cache invalidation on edit** — relaying ~100 visible lines per
  keystroke costs well under a millisecond; a line-range invalidation can be
  added if profiling ever shows otherwise (marked `ponytail:` in the source).
* **Loopback TCP + token for single instance** instead of Unix sockets and
  named pipes — one implementation for three platforms, same security
  properties once the token file is private.
* **Vim as an interpreter over the existing selection model** rather than a
  separate editing path — every Vim operator reuses the tested operations and
  the same undo history.
* **One window per process** — egui's multi-viewport support would share
  state, but every pane/tab method assumes a single window; `--new-window`
  starts a separate instance instead. Revisit if demand appears.
* **Criterion with default features off** — no plotting dependencies; the
  reports are text.
