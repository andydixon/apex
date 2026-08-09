# Feature matrix

Status key: **V** implemented and verified by automated tests · **I** implemented, verified manually or not yet verified · **P** partially implemented · **–** not implemented.

Updated: after Milestones 4–7 (workspace, navigation, search, language features).

## Document engine

| Feature | Status | Notes |
|---|---|---|
| Rope storage, insert/delete/replace | V | `apex-buffer`, `apex-edit::transaction` |
| Byte/char/line/UTF-16/display-column conversion | V | `apex_buffer::position` |
| Grapheme-aware motion (emoji, ZWJ, combining, CRLF) | V | chunk-aware over large ropes |
| Word and subword motion | V | |
| Revision tracking, multi-view selection resync | V | |
| Transactions with inverse and byte edits | V | |
| Undo/redo with typing coalescing, bounded memory | V | redo is linear (documented) |
| Multi-cursor atomic edits, 1,000-cursor test | V | |
| Selection normalisation (overlap merge, dedupe) | V | |
| Bookmarks and named marks following edits | V | |
| Property-based tests | – | Milestone 2 |

## File handling

| Feature | Status | Notes |
|---|---|---|
| New / Open / Open multiple / Save / Save As / Save All / Revert | I | |
| Open recent, reopen closed tab | I | recent list persisted privately |
| Close tab / others / to right / all, confirm on unsaved | I | |
| Drag-and-drop to open | I | |
| Encodings: UTF-8, UTF-8 BOM, UTF-16 LE/BE, legacy set | V | detection, reload with, save with |
| Line endings: detect, preserve, convert, mixed flag | V | |
| Binary detection and hex preview | V | read-only |
| Atomic save with fsync, permission preservation, symlink write-through | V | |
| External modification detection, conflict dialog | I | checked on focus and every 2 s |
| Deleted-on-disk handling | I | buffer kept, save recreates |
| Read-only files | I | |
| Huge files | P | 2 GiB cap; large-file mode flag only |
| Rename / move / delete (confirmed) / duplicate file, new file/folder | I | sidebar context menu and Project menu |
| Folder as workspace root, multiple roots, remove root | I | `apex .` or drag a folder in |
| Project files (`.apex-project`, relative folders, settings overrides) | I | |

## Editor interface

| Feature | Status | Notes |
|---|---|---|
| Custom viewport widget, visible-line layout cache | I | |
| Tabs with dirty dot, close, middle-click, overflow scroll, context menu | I | |
| Split panes (columns, rows, grid, nested), draggable dividers | I | |
| Multiple views of one document | I | |
| Status bar: position, selection summary, indent, encoding, EOL, language | I | |
| Menu bar | I | |
| Find/replace bar with regex, case, whole word, in-selection, count | I | |
| Themes (8 built-in, user TOML themes, live switching) | V | |
| Zoom, UI scale, line numbers, relative numbers, current line, rulers, indent guides, whitespace | I | |
| Bracket matching highlight | V | text-based |
| Distraction-free / zen / low-distraction modes | P | layout only |
| Minimap (token colours, viewport, click/drag, selection/search/git markers) | I | throttled rebuild |
| Word wrap (visual-row caret motion, per-view toggle) | I | |
| Code folding (syntax/indent regions, gutter controls, fold current/all/levels/comments) | I | selections never edit hidden text |
| Sidebar / project explorer (lazy tree, filter, follow active, inline rename, context menu) | I | |
| Preview tabs (single click), permanent on edit or double click | I | |
| Tab drag to reorder and between panes | I | |
| Command palette (fuzzy, shortcuts, recent weighting, highlighted matches) | V | nucleo matcher |
| Goto Anything (`file`, `file:line`, `:line`, `@symbol`, `#text`), instant preview, cancel restores | V | |
| Go to symbol (tags queries), project-wide symbols (background index) | I | |
| Jump back / forward history | I | |
| Syntax highlighting: tree-sitter, 30 grammars, incremental, injections, background parse | V | Kotlin query written in-house; Dockerfile via fallback |
| Language detection (name, extension, shebang, content) and per-file override | V | |
| Expand selection to syntax scope | I | |
| Find in Files: streaming, cancellable, include/exclude, gitignore, hidden, grouped results | V | |
| Replace in files with preview, per-file exclusion, external-change detection, partial-failure report | I | not atomic across files (documented) |
| Session restore (tabs, selections, scroll, layout, roots) | V | `session.json`, 0600 |
| Crash recovery journal and recovery dialog | I | snapshots every 5 s for dirty buffers |
| Macros: record, replay, save named, run | I | structured command steps only |
| File watcher refresh of sidebar/index and external change detection | I | |

## Multi-cursor

| Feature | Status | Notes |
|---|---|---|
| Ctrl+click add/remove, Alt+drag column, Shift+click extend | I | |
| Double/triple-click word/line and drag | I | |
| Add cursor above/below, quick add next/skip/all, undo selection | V | |
| Split into lines, column select by keyboard | V | |
| Paste one line per cursor | V | |

## Key bindings and commands

| Feature | Status | Notes |
|---|---|---|
| Command registry with stable ids | V | 240+ commands |
| TOML key maps, chords, contexts, per-platform sections, conflict detection | V | |
| User overrides from `<config>/keymap.toml` | I | |
| Vim mode | – | Milestone 10 |

## Known limitations (current)

* Lines beyond 20,000 characters are not drawn past that point.
* No syntax highlighting, folding, minimap, sidebar, palette or project search yet.
* Vertical caret motion is column-based; with a proportional editor font it is approximate.
* Folders passed on the command line are ignored until the workspace milestone.
