# Configuration

Everything is declarative data in the configuration directory (see
[Getting started](getting-started.md#where-things-live)). There is no plugin
system or scripting; the editor only ever reads these files.

## Settings

**Tools > Settings** (`Ctrl+,`) opens a searchable panel listing every
setting with a description. Changes apply immediately and are written to
`settings.toml`, which stores only the keys you changed. **Open settings.toml**
edits the file directly; saving it in the editor reloads the settings, and
problems (unknown keys, wrong types, out-of-range values) are reported in a
dialog rather than ignored or crashing.

Layers, lowest first: built-in defaults → `settings.toml` → the
`[settings]` table of the open project file.

Commonly changed keys:

```toml
theme = "Apex Light"
font_family = "JetBrains Mono"
font_size = 15
tab_width = 2
indent_width = 2
word_wrap = true
rulers = [80, 120]
trim_trailing_whitespace_on_save = true
vim_mode = true
spell_check = true
spell_language = "en_GB"
exclude_patterns = ["target", "node_modules", "*.min.js"]

[lsp_servers]
rust = ["rust-analyzer"]
python = ["pyright-langserver", "--stdio"]
```

## Key bindings

The defaults are in `assets/keymap-default.toml`; **Tools > Open Keymap File**
creates `<config>/keymap.toml`, whose bindings are merged on top:

```toml
[[bindings]]
keys = "primary+k primary+t"      # chords are space-separated
command = "edit.title_case"
context = "editor"                # global, editor, find, overlay, sidebar

[[bindings]]
keys = "f5"
command = "build.run"
arg = "Cargo: test"               # optional command argument

[[macos]]                         # platform-only bindings: [[linux]], [[windows]], [[macos]]
keys = "primary+shift+f"
command = "find.in_files"
```

`primary` is `Ctrl` on Linux and Windows and `Cmd` on macOS. Keys are
matched on the logical key with a physical-key fallback, so layouts that
move punctuation still work. **Tools > Keyboard Shortcuts** lists the
effective bindings and reports conflicts; the status bar shows a pending
chord.

## Themes

Eight themes are built in (Apex Dark, Apex Light, Graphite, Midnight, Solar
Dark/Light and high-contrast dark/light). **View > Theme** switches, and
`theme_mode = "auto"` follows the system's dark/light preference. User themes
are TOML files in `<config>/themes/`:

```toml
name = "My Theme"
dark = true

[ui]
background = "#1e2127"
text = "#d7dae0"
accent = "#61afef"
# … see assets/themes/apex-dark.toml for every key

[editor]
background = "#1e2127"
caret = "#528bff"
selection = "#3e4451"

[syntax]
keyword = { fg = "#c678dd", bold = false }
string = { fg = "#98c379" }
comment = { fg = "#5c6370", italic = true }
```

Unknown keys are reported; missing keys fall back to the base theme.

## Snippets

See [Editing](editing.md#snippets). Files in `<config>/snippets/<language>.toml`
override built-ins with the same trigger.

## Spell-check dictionaries

Hunspell `.dic` word lists are looked up in `/usr/share/hunspell`,
`/usr/share/myspell`, `~/Library/Spelling`, `<config>/dictionaries/` and
`/usr/share/dict/words`. Words you add go to `<config>/dictionaries/user.txt`.
Affix rules (`.aff`) are not applied, so inflected forms absent from the
list are flagged.
