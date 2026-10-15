# Changelog

All notable changes to Apex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Hex editor for binary files: edit the hex or text column (Tab switches),
  type hex digits continuously without moving the caret by hand, 32, 16 or
  8 bytes per row to fit the pane, arrow-key navigation and selection,
  undo/redo, copy as hex or text, and Save writes the bytes back exactly.
  Find matches hex (`de ad be ef` or `deadbeef`) as well as text.
- **Help > Check for Updates…**: asks GitHub for the latest release and
  shows its version, release date and a link to the release notes, with a
  Download button for this platform's installer. Never runs on its own.

## [0.2.0] - 2026-10-15

### Added
- **File > Open Remote…** and **Save to Remote…**: edit files on another
  machine over SSH through an in-app server browser. Uses your `ssh` with
  key or agent authentication. Clean tabs reload silently when the file
  changes on the server; a save that fails keeps the edits, marks the tab
  offline and finishes on its own once the server answers again.
  **Open Folder** in the browser shows a server folder as a tree in the
  sidebar's REMOTE section.
- SSH password and passphrase prompts for remote files. The answer is kept
  in locked memory (never swapped or written to disk) only while a file or
  folder from that server is open or the browser shows it, then wiped and
  the shared connection closed. Background checks never prompt.

### Changed
- Git credential answers now travel from the dialog to git in locked,
  wiped-on-release memory instead of ordinary strings.

### Fixed
- Windows menus use colours inverse to the window so they stay readable.

## [0.1.1] - 2026-10-13

### Added
- Git menu. Staging through your installed `git`: stage/unstage file or all,
  stage the change at the caret or only the selected lines, discard file
  changes; a GIT output tab logs every command. Git commands need a trusted
  workspace.
- Source Control panel in the lower half of the sidebar (`git_sidebar`):
  staged, unstaged and conflicted files with inline actions, a commit box
  (Ctrl+Enter, amend, sign-off, message history), undo last commit.
- Fetch, Pull (`git_pull_mode`), Push (publishes new branches after asking),
  Sync and Force Push with lease; ahead/behind counts; optional auto-fetch;
  password and passphrase prompts from git/ssh appear as an Apex dialog.
- Remote links (open file at lines, permalink, commit, branch, compare) for
  GitHub, GitLab, Bitbucket and Gitea; GitHub pull requests through the
  GitHub CLI: create, status and checks in the status bar, list, check out,
  diff.
- Side-by-side diff view for Open Changes, staged changes, Compare with
  Saved/Index/HEAD and `apex --diff`: synchronised scrolling, changed lines
  and words highlighted, long unchanged stretches folded, the working copy
  editable on the right, and per change revert, stage and unstage buttons.
- Branches (switch, create, rename, delete, merge), a log with lane graph
  and commit actions, line history, stash, and merge-conflict resolution in
  the editor with Continue/Abort for merges, rebases, cherry-picks and
  reverts.
- Blame (Ctrl+K Ctrl+G): gutter column with author and age, unsaved edits
  shown as not committed, click for the commit view; optional inline blame.
- Edit Commit History: edit authors, committers, dates and messages across
  the repository, bulk author replacement, date spreading, co-author line
  removal and a Claude/Codex filter; applies with `git filter-branch` after
  a backup tag, optionally force-pushing with lease.
- Open Folder by Path… and the `native_file_dialogs` setting: an in-app path
  prompt with Tab completion as an alternative to the system dialogs.

### Security
- Language servers start only in trusted folders: rust-analyzer runs a
  project's build scripts, so opening one file from an untrusted clone
  could run its code.
- "Rust: run file" no longer interprets the file name as shell and builds
  into the data directory instead of a fixed `/tmp` path.
- Opening a device or FIFO (for example a symlink to `/dev/zero` in a
  repository) is refused instead of reading forever or hanging.
- Printing uses a fresh, private, unguessable temporary directory.
- Single-instance and askpass tokens come from the OS random source and
  are compared in constant time; askpass reads are capped.
- Repository ref names can no longer be read as options by `git switch`,
  `merge` or `branch -d`.
- git2 0.21 (RUSTSEC-2026-0183 and RUSTSEC-2026-0184).

### Fixed
- Deleting a block of text could close the editor at once: when the
  document shrank below the scroll position (for example delete to the end
  of the file, undo, delete again), the next frame read a line that no
  longer existed. The scroll anchor is now kept inside the document.
- Left/Right after Split into Lines puts every caret at its own line's
  start or end; carets on blank lines no longer step onto the line above.
- Horizontal scrolling stops at the end of the widest visible line, so text
  never ends up hidden behind the minimap, and there is no sideways
  scrolling when nothing overflows.
- Git status, blame and history work in repositories reached through a
  symlink (including macOS temporary folders, where `/var` is
  `/private/var`).
- Windows builds: the SCSS grammar is taken from upstream, where its build
  script no longer passes a GCC-only flag to MSVC.
- CI: the fuzz job runs on nightly despite `rust-toolchain.toml`.
- Save Project As now uses the native dialog by default and honours
  `native_file_dialogs = false`, matching the other open and save commands.
- macOS: Open, Open Folder and Save As retain the main-run-loop AppKit
  sheet backend. Null panels and explicit panel aborts now fall back to
  the in-app path prompt; ordinary cancellation stays quiet. A stalled
  macOS panel service can still block panel creation.
- Returning focus to an editor view opened by a path prompt registers its
  AccessKit node first, preventing a missing-focus-node crash.

### Packaging
- Signed `.deb`, `.rpm` and Arch packages for x86-64 at
  [repo.dixon.cx](https://dixon.cx/repo) (`packaging/repo.sh`), and a
  Homebrew formula (`brew install andydixon/tap/apex`).

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
- Typing a command's full name in the palette picks it even when a
  recently used command would otherwise rank higher.
- Ctrl+Enter in the commit box was missed when a slow frame saw Ctrl already
  released; the key event's own modifiers are used.
- Open Folder / Open File / Save As no longer hang the editor on macOS:
  dialogs are started from a worker thread, so AppKit runs the panel from
  the main queue outside winit's event handler (starting it inside the
  handler hung before the panel appeared). A dialog that never answers no
  longer blocks the next one.
- macOS: double-clicking a document or Finder's Open With reached AppKit's
  default handler ("Apex cannot open files in the … format"); Apex's
  open-documents handler is now installed as launching finishes, after
  AppKit's own.
- The macOS Dock icon (and the window icon elsewhere) is Apex's icon instead
  of egui's default logo, which eframe installed at run time.
- `make release` now also produces the platform package (`Apex.app` and DMG
  on macOS, tarball and `.deb` on Linux) via `packaging/package.sh`.
- Macro recording no longer shadows Quit on Ctrl+Q (now Ctrl+Alt+R).
- Completion popup opens on language-server trigger characters and ignores
  stale responses; hover requests are no longer repeated every frame.
