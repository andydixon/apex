# Vim mode

Apex has a built-in Vim-style editing mode. It is a modal layer over the
editor's own document, selection and command model, not a Vim emulator: the
subset below is what is implemented and verified by tests
(`crates/apex-ui/src/vim.rs`). Anything not listed is not supported, and the
status bar says so when a key does nothing.

## Switching it on

* Settings: `vim_mode = true` in `settings.toml`.
* For the current session only: **Tools > Toggle Vim Mode** (command palette:
  "Toggle Vim Mode").

The status bar shows the mode (`-- NORMAL --`, `-- INSERT --`, `-- VISUAL --`,
`-- V-LINE --`), the keys of a partially typed command, and the command line
while you type `:` or `/` commands. The caret is a block outside insert mode.

Mouse, menus, the command palette, Goto Anything, Find and all `Ctrl`
shortcuts keep working. A selection made with the mouse or with Ctrl+D becomes
the visual selection as soon as you press a Vim key.

## Modes

| Keys | Effect |
|------|--------|
| `i` `a` `I` `A` `o` `O` | Insert before/after the cursor, at the first non-blank/end of line, on a new line below/above. A count repeats the inserted text (`3ihi<Esc>`). |
| `Esc` | Back to normal mode. The insert session is one undo step. |
| `v` `V` | Character-wise and line-wise visual mode. `o` swaps the ends. `Esc` or the same key leaves. |
| `:` | Command line (see below). In visual mode it starts with `'<,'>`. |
| `/` `?` | Search forward/backward (regular expression; case-sensitive when the pattern has capitals). |

Visual block mode (`Ctrl+V`) is not implemented; use the editor's column
selection (Ctrl+Shift+Up/Down) instead.

## Motions

`h` `j` `k` `l`, arrows, `0` `^` `$` `|`, `w` `W` `b` `B` `e` `E` `ge`,
`gg` `G` (with counts: `12G`), `{` `}`, `%`, `f` `F` `t` `T` + `;` `,`,
`H` `M` `L`, `Ctrl+F` `Ctrl+B` `Ctrl+D` `Ctrl+U`, `+` `-` `_` `Enter`,
`n` `N` `*` `#`, `'a` `` `a `` (marks set with `ma`).

Counts work with every motion (`3w`, `5j`). Vertical motions keep the
goal column.

## Operators

`d` `c` `y` `>` `<` `=` `gu` `gU` `g~`, each with a motion or text object,
doubled for whole lines (`dd`, `3dd`, `cc`, `yy`, `>>`, `guu`), and in visual
mode on the selection. `cw` behaves like `ce`, as in Vim. `d/pattern<CR>`
deletes up to the next match.

Shortcuts: `x` `X` `s` `S` `D` `C` `Y` `J` `r` `~` `p` `P`. `R` enters replace
(overwrite) mode until `Esc`.

## Text objects

`iw` `aw` `iW` `aW` `ip` `ap` `is` `as` `i(` `a(` (also `b`, `)`), `i{` `a{`
(`B`, `}`), `i[` `a[`, `i<` `a<`, `i"` `a"`, `i'` `a'`, `` i` `` `` a` ``,
`it` `at` (HTML/XML tags). Counts select enclosing levels (`2i(`).

## Registers

* `"a`–`"z` named; `"A`–`"Z` append.
* `""` unnamed (also the default), `"0` last yank, `"1`–`"9` recent deletes,
  `"-` small deletes, `"_` black hole.
* `"+` and `"*` are the system clipboard: `"+y` copies, `"+p` pastes through
  the editor's normal paste path (the text arrives from the window system,
  so it is inserted at the cursor like Ctrl+V).
* `:reg` lists the registers in the status bar.

## Repeat, undo

* `.` repeats the last change, including the text typed in an insert
  session. A count replaces the original count (`3.`).
* `u` undo, `Ctrl+R` redo (the editor's history; Ctrl+Z works too).

## Command line

`:w [file]`, `:wa`, `:q`, `:q!`, `:qa`, `:wq`, `:x`, `:e file`, `:N` (go to
line), `:s/pat/rep/[g]` on the current line, `:%s/…/…/g` on the whole file,
`:'<,'>s/…/…/` on the visual lines, `:noh`, `:set wrap|nowrap|nu|nonu|rnu|spell`,
`:sp`, `:vs`, `:only`, `:bn`, `:bp`, `:reg`.

`:q` closes the current tab (Apex keeps running); `:qa` quits.

Other keys: `ZZ` save and close, `ZQ` close without saving, `zz` centre,
`zc` `zo` `za` `zM` `zR` folds, `gd` go to definition, `gt` `gT` tabs,
`gv` reselect, `Ctrl+O` / `Ctrl+I` navigate back/forward, `Ctrl+E` / `Ctrl+Y`
scroll.

## Known limitations

* No visual block mode, no `q` macros (use the editor's macro recorder under
  Tools), no `:g`, `:normal`, mappings or `.vimrc`.
* With several carets, motions move the extra carets by the same distance as
  the primary caret rather than re-running the motion for each.
* `H` `M` `L` are approximated from the cursor line rather than the exact
  visible window.
* Hunspell-style `:set spell` toggles the editor's spell checker.
