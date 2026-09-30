# Manual smoke test

Run after any change to the editor widget or application shell, and on each
platform before a release. About fifteen minutes. The scripted equivalent of
steps 1–11 is `tests/visual/smoke.sh` (Linux, Xvfb).

## Editing

1. `cargo run -- README.md` — window opens, the file is shown with line numbers,
   the title bar shows the file name, the status bar shows `Line 1, Column 1`.
2. Click in the text, type, press Enter: the caret follows, the tab shows a
   dirty dot, the title gains `•`.
3. Ctrl+Z / Ctrl+Shift+Z undo and redo in typing groups.
4. Double-click selects a word; triple-click a line; drag extends; Shift+click
   extends from the caret; Ctrl+click adds a caret; typing inserts at all.
5. Ctrl+D three times on a repeated word adds three selections; Ctrl+U removes
   the last; Esc collapses to one.
6. Alt+drag makes a column selection across three lines; type to replace.
7. Ctrl+F, type a word: the count updates live and matches are highlighted;
   Enter moves to the next; Alt+R enables regex and `(` shows an error.
8. Ctrl+H, replace all: the status bar reports the count.
9. Ctrl+S saves (temp file disappears, file content updated). Edit the file in
   another program: Apex reloads it when it regains focus if there were no
   local edits; otherwise it asks.
10. Ctrl+\ splits; both panes show the same document; edits in one appear in
    the other with selections intact. Drag the divider.
11. Ctrl+W on a dirty tab prompts Save / Don't Save / Cancel.

## Interface

12. View ▸ Theme ▸ switch themes; everything restyles without restart.
    Set `theme_mode = "auto"` and change the system appearance: the variant
    follows.
13. Ctrl+= / Ctrl+- zoom; Ctrl+0 resets. Ctrl+, opens Settings; change
    "Line numbers" — the gutter updates at once and `settings.toml` contains
    only that key. Open the file, change the value, save: the editor follows.
14. Ctrl+Shift+O opens this repository: the sidebar lists it lazily, Ctrl+P
    finds `history.rs`, `@` lists symbols, Ctrl+Shift+F finds `fn apply` with
    grouped results and clicking one jumps there.
15. Tools ▸ Build runs `cargo check`; output streams; an error line in the
    Problems tab jumps to the file. A project file with `[[tasks]]` asks for
    trust first.
16. With `rust-analyzer` installed: the status bar shows progress, hover shows
    documentation after a pause, typing `.` opens completion with server
    items first, F12 jumps to a definition.
17. Tools ▸ Toggle Vim Mode: `-- NORMAL --` appears, `dd` deletes a line,
    `ciw` changes a word, `:%s/a/b/g` substitutes, `u` undoes, `:wq` saves
    and closes the tab.

## Command line and platform

18. With the editor running: `apex README.md` from a terminal returns
    immediately and the file opens in the existing window;
    `apex --wait other.txt` returns when the tab is closed;
    `apex --diff a b` opens a diff tab; `apex --new-window` starts a second
    instance.
19. Close the window with unsaved changes: the prompt appears; Cancel keeps
    the window open. Kill the process (`kill -9`) with unsaved changes, start
    again: the recovery dialog offers the snapshot and Restore brings the
    text back.
20. **macOS** (packaged `Apex.app`): right-click a `.txt` file in Finder ▸
    Open With ▸ Apex opens it in the running instance; dropping a file on the
    Dock icon does too; `open -a Apex file.md` likewise. Gatekeeper accepts
    the ad hoc or notarised signature.
21. **Windows** (installer): "Open with Apex" appears in the Explorer context
    menu for files and folders; `apex file.txt` works in a new terminal when
    the PATH option was chosen; uninstall removes the entries.
22. **Linux**: the desktop entry appears in the launcher with the icon; a
    `.txt` file's "Open With" dialog lists Apex.
