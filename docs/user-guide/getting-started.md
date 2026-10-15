# Getting started

## Installing

* **Linux**: unpack `apex-<version>-linux-<arch>.tar.gz` and run `./install.sh`
  (installs to `~/.local`; `PREFIX=/usr/local sudo ./install.sh` for all
  users), or install the `.deb` with `sudo apt install ./apex_<version>_amd64.deb`.
  The AppImage runs from anywhere after `chmod +x`.
* **macOS**: open the DMG and drag `Apex.app` to Applications. Apex appears in
  Finder's **Open With** menu for any file.
* **Windows**: run `Apex-<version>-setup.exe`. The installer can add
  "Open with Apex" to the Explorer context menu, associate common text file
  types and put `apex` on the PATH. The zip is a portable alternative.

Building from source is described in the top-level `README.md`.

## The window

```
 Menu bar
 ┌───────────┬──────────────────────────────────────────┐
 │ Sidebar   │ Tabs                                      │
 │ Folders   ├──────────────────────────────────┬───────┤
 │ (project  │ Editor                           │Minimap│
 │  files)   │                                  │       │
 ├───────────┴──────────────────────────────────┴───────┤
 │ Output / Problems / Find results panel               │
 ├──────────────────────────────────────────────────────┤
 │ Status bar: position, git branch, language, encoding │
 └──────────────────────────────────────────────────────┘
```

Everything in the sidebar, the panel, the minimap and the status bar can be
hidden (**View** menu, or `Ctrl+K Ctrl+B` for the sidebar). `F11` is
fullscreen, `Shift+F11` distraction-free, `Ctrl+K Ctrl+Z` zen mode; `Escape`
leaves all three.

## Opening files

* `Ctrl+O` opens files, `Ctrl+Shift+O` a folder. Drag files or folders onto
  the window to open them.
* `Ctrl+P` (**Goto Anything**) fuzzy-finds any file in the open folders.
* **File > Open Recent** lists recent files and projects.
* Large files (over `large_file_mb`, 50 MB by default) open in a
  reduced-feature mode: no syntax highlighting, minimap detail or word wrap,
  so they stay responsive. Binary files open in the
  [hex editor](editing.md#hex-editor).

## The command line

```
apex                          open (or focus) the editor
apex file.rs                  open a file in the running editor
apex file.rs:120:8            open at line 120, column 8
apex .                        open a folder as a workspace
apex a.rs b.rs "my notes.md"  several files; spaces need quotes
apex --new-window file.rs     start a second, independent window/instance
apex --reuse-window file.rs   explicit form of the default behaviour
apex --wait file.rs           return only when the file's tab is closed
                              (for git commit, EDITOR=… and similar)
apex --diff old.rs new.rs     show a unified diff of two files
apex --version | --help
```

A running instance is found through `instance.json` in the per-user runtime
directory; a second invocation hands over its paths and exits. Set
`APEX_RUNTIME_DIR` to isolate instances (tests do this). Logging:
`--log debug` or `APEX_LOG=debug` prints to stderr.

## Where things live

| | Linux | macOS | Windows |
|---|---|---|---|
| Settings, key map, themes, snippets, dictionaries | `~/.config/apex` | `~/Library/Application Support/cx.dixon.Apex` | `%APPDATA%\dixon\Apex\config` |
| Sessions, recovery snapshots, macros | `~/.local/share/apex` | `~/Library/Application Support/cx.dixon.Apex` | `%LOCALAPPDATA%\dixon\Apex\data` |

**Help > About** shows the exact paths; `APEX_CONFIG_DIR` and
`APEX_DATA_DIR` override them.
