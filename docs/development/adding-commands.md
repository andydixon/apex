# Adding a built-in command

Every user action is a `Command` variant with a stable id. Menus, key
bindings, the command palette, macros and Vim's `:` commands all dispatch
through it, so a new command is available everywhere once it exists.

1. **Register it** in `crates/apex-ui/src/command.rs` inside the
   `commands!` table:

   ```rust
   ReverseWords => "edit.reverse_words", "Reverse Words in Selection", Edit;
   ```

   The id (`edit.reverse_words`) is what key maps and macros store; never
   change it once released. The name is shown in menus and the palette; the
   last field is the `Category`.

2. **Implement it.** Pure text operations belong in `apex-edit`
   (`ops.rs`): a function `fn(&Document, &SelectionSet, …) -> Option<EditResult>`
   that builds one `Transaction` for all carets and returns the new
   selections. Write its unit test next to it. Then handle the variant in
   the dispatch chain in `crates/apex-ui/src/app.rs` (`execute`), which
   tries the feature modules in order (`vim_command`, `settings_command`,
   `git_command`, `tasks_command`, `lsp_command`, `spell_command`,
   `execute_workspace`) before the big editing `match`:

   ```rust
   Command::ReverseWords => {
       let v = view!();
       self.run_edit(v, |doc, sels| ops::reverse_words(doc, sels));
   }
   ```

   `run_edit` applies the result to the document, records undo, updates the
   view and refuses read-only buffers. Commands that need an argument
   receive it as `arg: Option<String>`; commands that need a choice open
   the palette in a `PaletteMode::Prompt` and come back through
   `palette_accept`.

3. **Expose it.** Add it to a menu in `show_menu_bar` (app.rs) if it
   belongs in one; the command palette and **Keyboard Shortcuts** list pick
   it up automatically. Give it a default binding in
   `assets/keymap-default.toml` only if it deserves one; run the app once
   and check **Tools > Keyboard Shortcuts** reports no conflict (the key
   map parser also reports them at start-up in the log).

4. **Macros and Vim.** Commands are recorded in macros automatically. If
   the command makes sense as a Vim `:` command, add a line to
   `ex_command` in `vim.rs` and a test in the same file.

5. **Document it**: `FEATURE_MATRIX.md` row, user guide page, and
   `CHANGELOG.md`.

Conventions: commands must be safe to invoke with no document open (use
the `view!()` / `active_ids()` helpers, which return early), must not block
the UI thread (spawn work through the task and search workers and poll the
result in `poll_background`), and report failures with `set_status(…, true)`
rather than panicking.
