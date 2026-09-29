# Navigation

## Goto Anything (`Ctrl+P`)

One box for everything. Type part of a file name or path to fuzzy-find it
among the open folders (matches are ranked, and the file is previewed as you
move through the list; `Escape` returns to where you were). Prefixes change
the target:

| Prefix | Finds |
|---|---|
| `:120` | Line 120 (`:120:8` for a column) in the current file |
| `@name` | Symbols in the current file (`Ctrl+R`) |
| `#name` | Symbols across the project (`Ctrl+Shift+R`), from the tree-sitter index or the language server |
| `>command` | Commands (the command palette, `Ctrl+Shift+P`) |
| `file@name` or `file:12` | A symbol or line in another file |

`Ctrl+G` is a plain go-to-line dialog.

## Command palette (`Ctrl+Shift+P`)

Every command with its shortcut, fuzzy-filtered, most-recent first. Commands
that take an argument prompt for it. Settings, themes, snippets, macros,
tasks and git status all have palette entries.

## Moving around

* `F12` go to definition, `Shift+F12` references (language server, or the
  project symbol index for languages without one). `Alt+-` / `Alt+Shift+-`
  go back and forward through the jump history.
* `Ctrl+M` jumps to the matching bracket; `Ctrl+Up`/`Ctrl+Down` scroll
  without moving the caret; `Ctrl+K Ctrl+C` centres the caret line.
* `Ctrl+PgUp`/`Ctrl+PgDn` and `Ctrl+1`…`Ctrl+9` switch tabs, `Ctrl+Shift+T`
  reopens the last closed tab.
* Folding: click the gutter arrows or use `Ctrl+Shift+[` / `Ctrl+Shift+]`,
  `Ctrl+K Ctrl+1`…`3` to fold to a level, `Ctrl+K Ctrl+J` to unfold all.
  Folds come from the syntax tree, or indentation for plain text.
* The minimap on the right shows the whole file; click or drag it to scroll.
* Breadcrumbs above the editor show the enclosing symbols.

## Panes and layouts

`Ctrl+\` splits the current pane; the **View > Layout** menu has single,
two and three columns, two rows and a 2×2 grid (`Alt+Shift+1`, `2`, `3`,
`8`, `5`). `Ctrl+K Ctrl+Left`/`Right` move focus between panes; drag tabs
between panes or use **Move Tab to Next Pane**. The same file can be open in
several panes with independent scroll positions and selections; edits show
up everywhere immediately.

Tabs: drag to reorder, right-click for close others / to the right, pin,
copy path, reveal in the file manager. A file opened from the sidebar with a
single click is a *preview* tab (italic title) that is replaced by the next
preview; editing it or double-clicking makes it permanent.
