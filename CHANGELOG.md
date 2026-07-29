# Changelog

All notable changes to Apex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Rope-based document engine with transactional, coalescing undo/redo.
- Unicode-correct caret motion (grapheme clusters, wide characters, tabs).
- Multi-cursor editing: Ctrl+click, quick add next/skip/all, column selection,
  add cursor above/below, split into lines, per-cursor paste.
- Custom egui editor widget with viewport-only layout and per-line caches.
- Tabs, split panes (columns, rows, grid, nested), multiple views per document.
- Find and replace (regex, case, whole word, in selection).
- Encodings (UTF-8/16 and legacy) and line-ending detection, conversion.
- Atomic saves with external-modification detection and conflict dialogs.
- Eight built-in themes, user TOML themes, TOML key maps with chords.
