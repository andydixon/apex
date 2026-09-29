# Editing

## Basics

| Keys | Action |
|---|---|
| `Ctrl+Z` / `Ctrl+Shift+Z` (or `Ctrl+Y`) | Undo / redo. Typing coalesces into one step; selections are restored with each step. |
| `Ctrl+X` `Ctrl+C` `Ctrl+V` | Cut, copy, paste. With no selection the whole line is cut or copied; pasting a copied line inserts it above the caret line. |
| `Ctrl+Shift+V` | Paste and re-indent to the caret's indentation. |
| `Ctrl+Alt+V` | Paste from the clipboard history (last 32 copies). |
| `Ctrl+Shift+D` | Duplicate the line or selection. |
| `Ctrl+Shift+K` | Delete the line. |
| `Ctrl+Shift+Up` / `Down` | Move lines up or down. |
| `Ctrl+J` | Join lines. |
| `Ctrl+Enter` / `Ctrl+Shift+Enter` | New line below / above, keeping indentation. |
| `Ctrl+/` / `Ctrl+Shift+/` | Toggle line / block comment for the file's language. |
| `Ctrl+]` / `Ctrl+[` | Indent / outdent. `Tab` indents a selection and inserts an indent otherwise. |
| `Ctrl+Backspace` / `Ctrl+Delete` | Delete word backwards / forwards. |
| `Ctrl+T` | Transpose characters. |
| `F9` / `Ctrl+F9` | Sort lines (case-sensitive / insensitive). The **Edit** menu also has unique, reverse, shuffle, trim trailing whitespace, sequence insertion and case conversions (`Ctrl+K Ctrl+U` upper, `Ctrl+K Ctrl+L` lower, `Ctrl+K Ctrl+T` title, plus snake, camel and kebab case). |
| `Alt+Q` | Hard-wrap the paragraph at `wrap_column` (80 when unset). |
| `Insert` | Overwrite mode (block caret). |
| `Ctrl+Space` | Open completion: words from open files, snippets and the language server when one is running. `Tab`/`Enter` accept, `Escape` dismisses. |
| `Ctrl+Shift+I` | Format the document with the language server. |

Brackets and quotes auto-pair (type over the closing one to skip it, or
turn `auto_pair` off). `Backspace` in leading whitespace removes one indent
level (`smart_backspace`). Indentation is detected per file; the status bar
shows it and the **Spaces: 4** entry switches tabs/spaces and converts.

## Bookmarks and marks

`Ctrl+F2` toggles a bookmark on the line, `F2`/`Shift+F2` jump between
bookmarks, `Ctrl+Shift+F2` clears them. Bookmarks move with the text as you
edit and are saved with the session.

## Snippets

Type a trigger and press `Tab` (for example `fn` in Rust, `def` in Python),
or choose **Edit > Insert Snippet**. `Tab`/`Shift+Tab` move through the
placeholders; `Escape` ends the session. Built-in snippets live in
`assets/snippets`; add your own as `<config>/snippets/<language>.toml`:

```toml
[[snippet]]
trigger = "log"
description = "Log a value"
body = "println!(\"${1:name} = {:?}\", ${1});$0"
```

Variables such as `$FILENAME`, `$SELECTION`, `$CLIPBOARD`, `$DATE` and
choices like `${1|a,b,c|}` follow the usual snippet syntax.

## Multiple cursors

This is the heart of the editor. Every cursor is a real caret: typing,
deleting, pasting, undo and all of the commands above act on all of them in
one transaction.

| Keys | Action |
|---|---|
| `Ctrl+Click` | Add a caret (or remove one under the pointer). |
| `Ctrl+D` | Select the word under the caret, then add the next occurrence each time. `Ctrl+K Ctrl+D` skips an occurrence. |
| `Alt+F3` | Select every occurrence. |
| `Ctrl+Shift+L` | Split a selection into one caret per line. |
| `Alt+Shift+Up` / `Down` | Add a caret on the line above / below (keeps the column across short lines). |
| `Ctrl+Shift+Alt+Up` / `Down`, or `Shift+Right-drag` / middle-drag | Column selection. |
| `Ctrl+U` | Undo the last selection change. |
| `Escape` | Back to a single caret. |
| `Ctrl+L` | Select the line (repeat to extend). |
| `Ctrl+Shift+M` / `Ctrl+M` | Select inside brackets / jump to the matching bracket. |
| `Ctrl+Shift+J` | Select the indentation block. |
| `Ctrl+Shift+A` / `Ctrl+Shift+Alt+A` | Expand / shrink the selection by syntax scope. |

Pasting with as many clipboard lines as there are carets distributes one
line per caret. The status bar shows the caret count and selected characters.

## Macros

**Tools > Macros**: `Ctrl+Alt+R` starts and stops recording, `Ctrl+Alt+Shift+R`
replays. A macro is a list of built-in commands and typed text; it can be
saved under a name and replayed from the command palette. Macros cannot run
programs or scripts.

## Spell checking

**Tools > Toggle Spell Check** (or `spell_check = true`) underlines
misspelled words in prose files and in comments and strings of code.
Right-click a word for suggestions, **Add to dictionary** or **Ignore**.
Dictionaries are Hunspell `.dic` word lists found in the system locations or
`<config>/dictionaries/<lang>.dic`; the status bar says when none is found.
