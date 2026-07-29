# Apex

Apex is a native, high-performance text editor written in Rust with an
egui/eframe interface. It is inspired by Sublime Text's responsiveness and
workflow: fast start-up, excellent multi-cursor editing, powerful search and
navigation, configurable themes and key bindings, and careful handling of your
files. It runs on Linux (X11 and Wayland), Windows 10/11 and macOS.

Apex is deliberately a text editor, not an IDE: there is no plugin system, no
telemetry, no account and no network dependency.

> Status: early development. See `FEATURE_MATRIX.md` for what works today and
> `PROGRESS.md` for the milestone log.

## Features (current)

* Custom editing engine: rope buffer, viewport-only rendering, Unicode-correct
  carets (grapheme clusters, wide characters, tabs), transactional undo/redo.
* Multiple cursors: Ctrl+click, Ctrl+D quick-add, Alt+drag columns, add cursor
  above/below, split selection into lines, paste one line per cursor.
* Tabs, split panes (columns, rows, 2×2 grid, nested), several views of one file.
* Find and replace with regular expressions, whole word, case and in-selection
  modes; informative regex errors.
* Encodings (UTF-8, UTF-16, Windows-125x, ISO-8859, CJK) and line endings with
  detection, conversion and preservation.
* Safe saving: temp file + fsync + atomic rename, permission preservation,
  refuses to overwrite a file another program changed.
* Eight built-in colour schemes, user themes in TOML, live switching.
* Configurable key bindings with multi-step chords and per-platform defaults.

## Building

Requirements: stable Rust 1.88 or newer (edition 2024). On Linux you also need
the usual windowing development packages:

```sh
# Debian/Ubuntu
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev \
     libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
# Fedora
sudo dnf install gcc pkg-config libxkbcommon-devel wayland-devel libxcb-devel
```

Then:

```sh
cargo build --release
./target/release/apex path/to/file.rs
```

`make build`, `make test`, `make lint`, `make fmt`, `make check` and
`make release` wrap the corresponding cargo invocations.

## Running

```
apex                      # empty untitled buffer
apex file.rs              # open a file
apex file.rs:120:8        # open at line 120, column 8
apex a.rs b.rs            # several files
apex --help
```

Configuration lives in the platform configuration directory
(`~/.config/apex` on Linux, `~/Library/Application Support/cx.dixon.Apex` on
macOS, `%APPDATA%\dixon\Apex\config` on Windows). `Tools ▸ Settings…` creates
`settings.toml` with every option at its default; `Tools ▸ Open Key Bindings
File` creates `keymap.toml`. Themes go in a `themes/` sub-folder.

## Keyboard shortcuts (defaults, Linux/Windows; use ⌘ for Ctrl on macOS)

| Action | Keys |
|---|---|
| Command palette / Goto Anything | Ctrl+Shift+P / Ctrl+P (upcoming) |
| Go to line | Ctrl+G |
| Find / Replace | Ctrl+F / Ctrl+H |
| Quick add next occurrence | Ctrl+D |
| Select all occurrences | Alt+F3 |
| Add cursor above/below | Ctrl+Alt+↑ / Ctrl+Alt+↓ |
| Split selection into lines | Ctrl+Shift+L |
| Expand selection to line | Ctrl+L |
| Duplicate line | Ctrl+Shift+D |
| Delete line | Ctrl+Shift+K |
| Move line up/down | Ctrl+Shift+↑ / ↓ |
| Toggle comment | Ctrl+/ |
| Join lines | Ctrl+J |
| Undo / Redo | Ctrl+Z / Ctrl+Shift+Z |
| Zoom | Ctrl+= / Ctrl+- / Ctrl+0 |
| Split pane | Ctrl+\ ; layouts Alt+Shift+1..3, 5, 8 |
| Switch tabs | Ctrl+Tab, Ctrl+PgUp/PgDn, Ctrl+1..9 |

The full list is in `assets/keymap-default.toml`.

## Licence

Apex is licensed under the GNU Affero General Public License, version 3.0 or
later. See `LICENSE`. Bundled fonts are those shipped with egui (Hack —
MIT/Bitstream Vera; Ubuntu — Ubuntu Font Licence; Noto Emoji — OFL; emoji-icon-font — MIT).
