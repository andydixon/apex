# Language servers

Apex has an optional built-in Language Server Protocol client. It is off
until a server is found or configured; everything in the editor works
without one (syntax highlighting, folding, symbols and word completion are
built in and do not depend on a server).

## Starting a server

With `lsp_enabled = true` (the default) a known server is started
automatically the first time a file of its language is opened, if the
program is on `PATH`:

| Language | Server |
|---|---|
| Rust | `rust-analyzer` |
| Python | `pyright-langserver`, `pylsp` |
| TypeScript, JavaScript | `typescript-language-server` |
| Go | `gopls` |
| C, C++ | `clangd` |
| Lua | `lua-language-server` |
| Bash | `bash-language-server` |
| JSON, CSS, HTML | the `vscode-*-language-server` packages |
| YAML | `yaml-language-server` |
| Markdown | `marksman` |
| TOML | `taplo` |
| Java, Kotlin, Swift, PHP, Ruby, C# | `jdtls`, `kotlin-language-server`, `sourcekit-lsp`, `intelephense`, `solargraph`, `omnisharp` |

Other servers, or different commands, go in settings:

```toml
[lsp_servers]
zig = ["zls"]
python = ["ruff", "server"]
```

The status bar shows the server's state and progress ("rust-analyzer:
402/403 indexing"). **Tools > Restart Language Server** restarts it; a
server that exits three times in a row is left stopped and its last stderr
lines are shown in the status message. **Tools > Toggle Language Server**
turns the client off for the session.

## What you get

* Diagnostics underlined in the editor and listed in the Problems tab;
  hover shows the message.
* Completion merged with word and snippet completions (`Ctrl+Space`, or
  automatically after typing; server items first).
* Hover documentation after the pointer rests on a symbol, signature help
  while typing arguments.
* `F12` definition, `Shift+F12` references, declaration and implementation
  (Goto menu), document and workspace symbols in Goto Anything.
* **Rename Symbol** (`F7`), **Code Actions** (`Ctrl+K Ctrl+I`) and **Format
  Document** (`Ctrl+Shift+I`, also `format_on_save`). Workspace edits that
  touch files not open in the editor are previewed and confirmed before
  they are applied.

Documents are synchronised with full-text updates debounced at 250 ms;
positions are converted between the editor's character offsets and the
protocol's UTF-16 columns. Servers run as child processes with only the
workspace folders as roots; no network connections are made by the client.
