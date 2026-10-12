# Changelog

All notable changes to Apex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.0.1] - 2026-10-12

### Added
- Rope-based document engine with transactional, coalescing undo/redo.
- Unicode-correct caret motion (grapheme clusters, wide characters, tabs).
- Multi-cursor editing: Ctrl+click, quick add next/skip/all, column selection,
  add cursor above/below, split into lines, per-cursor paste.
- Custom egui editor widget with viewport-only layout and per-line caches,
  word wrap, folding, minimap, indent guides, rulers, whitespace rendering.
- Tabs (drag, pin, preview), split panes (columns, rows, grid, nested),
  multiple views per document, distraction-free/zen/low-distraction modes.
- Find and replace (regex, case, whole word, in selection); Find and Replace
  in Files with gitignore and glob filters.
- Goto Anything, command palette, go to symbol/project symbol, jump history,
  bookmarks, breadcrumbs.
- Tree-sitter highlighting for 30 languages with injections and incremental,
  budgeted, background parsing; symbols and folding from the syntax tree.
- Encodings (UTF-8/16 and legacy) and line-ending detection, conversion.
- Atomic saves with external-modification detection and conflict dialogs;
  large-file mode; hex preview for binaries.
- Project explorer with lazy loading and a file watcher; project files;
  sessions and crash recovery.
- Git: gutter marks, branch label, hunk navigation and revert, compare with
  index/HEAD/saved, file history, status palette.
- Build tasks with trust gating, problem matchers and a Problems panel.
- Optional LSP client: diagnostics, completion, hover, signature help,
  navigation, rename, code actions, formatting, workspace edits.
- Snippets with tab stops; completion popup merging words, snippets and
  server items.
- Spell checking with Hunspell word lists, suggestions and a user dictionary.
- Eight built-in themes, user TOML themes, `theme_mode = "auto"`; TOML key
  maps with chords and platform sections; macros.
- Searchable settings editor that writes a minimal `settings.toml` and
  reloads it live.
- Built-in Vim mode: normal/insert/visual/visual-line/replace, counts,
  operators, text objects, registers, marks, search, `.`, `:` commands.
- Command line: `file:line:col`, folders, single instance with hand-over,
  `--new-window`, `--reuse-window`, `--wait`, `--diff`.
- macOS Apple Event handler so Finder "Open With" reaches the editor;
  `Info.plist` document types; Windows icon and version resource.
- Packaging scripts: Linux tarball/`.deb`/AppImage, macOS app bundle and
  DMG, Windows zip and Inno Setup installer; icon generation from SVG.
- Criterion benchmarks, whole-application start-up/memory script, fuzz
  targets, security review, user guide and developer documentation.

### Fixed
- Open Folder / Open File / Save As no longer freeze the editor on macOS:
  native dialogs are a sheet polled from the frame loop there, and run on a
  worker thread on Linux and Windows, so the window stays responsive.
- `make release` now also produces the platform package (`Apex.app` and DMG
  on macOS, tarball and `.deb` on Linux) via `packaging/package.sh`.
- Macro recording no longer shadows Quit on Ctrl+Q (now Ctrl+Alt+R).
- Completion popup opens on language-server trigger characters and ignores
  stale responses; hover requests are no longer repeated every frame.
