# Apex architecture

This document records the significant design decisions. It is updated as the
implementation progresses; the authoritative state of each feature is in
`FEATURE_MATRIX.md` and the milestone log is in `PROGRESS.md`.

## Crate layout

Dependencies flow strictly towards the core; nothing below `apex-ui` knows
about egui.

| Crate          | Responsibility                                                                 | Depends on                   |
|----------------|--------------------------------------------------------------------------------|------------------------------|
| `apex-buffer`  | Rope text store (ropey), offset-space conversions, grapheme/word motion, line endings, encodings | ropey, unicode-*, encoding_rs |
| `apex-edit`    | `Document` (text + revision + history + metadata), `Selection`/`SelectionSet`, `Transaction`, editing operations, in-document search, bracket matching | apex-buffer, regex |
| `apex-syntax`  | Tree-sitter grammars, highlighting, folding, symbol extraction (Milestone 7) | apex-edit, tree-sitter |
| `apex-project` | File I/O with atomic saves, platform paths, workspaces, file walking, Find in Files, Git, sessions (Milestones 4–8) | apex-edit, ignore, git2, notify |
| `apex-ui`      | egui editor widget, panes/tabs, menus, find bar, palette, settings, themes, key maps | all of the above, egui |
| `apex-app`     | Binary: CLI parsing, logging, single-instance IPC, window creation          | apex-ui, eframe, clap        |

The spec suggested seventeen crates; six keep the same boundaries without
empty shells. `apex-syntax` and `apex-project` are the natural seams for the
background-worker code.

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
View      = DocId + SelectionSet + scroll + layout cache + drag state
Pane      = ordered tabs (ViewIds) + active index + pinned set
Layout    = Leaf(PaneId) | Split { axis, children, fractions }
ApexApp   = docs, views, panes, layout, settings, theme, keymap, dialogs
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
each inserted run) or by mapping, so 1,000 cursors stay consistent.

### History

Undo entries hold `Vec<(forward, inverse)>` steps plus selections before and
after. Consecutive typing (and consecutive backward/forward deletes) within
one second with an unchanged caret count coalesce into one entry; any other
edit, or a caret move, seals the group. Redo is linear: a new edit after an
undo discards the redo stack. History is bounded by entry count and by
retained text bytes (default 10,000 entries / 64 MiB) and trims the oldest.

## Rendering

The editor is a custom widget (`apex_ui::editor`), not `egui::TextEdit`.

* Input and painting are separate passes. Input converts mouse and keyboard
  events into `Action`s; the application executes those (possibly mutating the
  document) and only then paints, so each frame shows the post-edit state.
* Only lines intersecting the viewport (plus one) are laid out. Each line is a
  single `egui::Galley` produced from a `LayoutJob` carrying syntax sections;
  galleys are cached per line in `LayoutCache`, invalidated by an epoch that
  bumps on any document, font, theme or width change.
* Tabs are expanded to spaces for layout (egui's shaper has fixed-advance
  tabs); a per-line `doc→galley` column map keeps hit-testing exact. Control
  characters are drawn as Unicode control pictures.
* Lines longer than 20,000 chars are laid out only to that limit and marked as
  truncated; the rest stays editable through keyboard navigation. Full
  horizontal virtualisation is a Milestone 11 item.
* Idle frames do no layout work: egui only repaints on input, plus a 0.5 s
  wake-up for caret blink while an editor is focused (can be disabled).

## Input pipeline

`Event::Key` → `Keymap::dispatch(KeyPress, contexts)` → `Command`. Chords
(`ctrl+k ctrl+b`) are supported via pending state. Text arrives as
`Event::Text`; a text event immediately following a bound key press is
swallowed so `Alt+Q` does not also type "q". Cut/Copy/Paste arrive as
dedicated egui events (egui-winit intercepts those shortcuts), and are routed
to the same commands as the menus.

Key bindings, menus, the context menu and (later) the palette and macros all
dispatch `Command` values with stable string identifiers (`edit.undo`), so
there is exactly one implementation per command.

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

## Security boundaries

* Files, project configuration and process output are untrusted. Decoding
  never panics; invalid sequences become U+FFFD and the user is warned before
  a lossy save.
* Regex search uses the `regex` crate (linear time, no backreferences) with a
  compile size limit, so a hostile pattern or document cannot hang the UI.
* Configuration is data (TOML) and cannot execute code. Unknown keys and type
  errors are reported, never fatal.
* Recovery, session and recent-file data live in a 0700 directory.
* Nothing in the editor contacts the network.

## Decisions log

* **ropey 1.6 over 2.0-beta** — stable API, no risk from a beta in the core.
* **git2 over gix** (planned) — simpler status/diff API; vendored libgit2 keeps
  the build self-contained. Only local operations are used.
* **rfd with the XDG portal backend on Linux** — native dialogs without a GTK
  build dependency.
* **Vertical motion by display column** rather than pixel x — deterministic,
  testable, and exact for the monospace fonts a code editor uses.
* **Whole-cache invalidation on edit** — relaying ~100 visible lines per
  keystroke costs well under a millisecond; a line-range invalidation can be
  added if profiling ever shows otherwise (marked `ponytail:` in the source).
