# Manual smoke test

Run after any change to the editor widget or application shell. Takes about
five minutes.

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
12. View ▸ Colour Scheme ▸ switch themes; everything restyles without restart.
13. Ctrl+= / Ctrl+- zoom; Ctrl+0 resets.
14. Close the window with unsaved changes: the prompt appears; Cancel keeps
    the window open.
