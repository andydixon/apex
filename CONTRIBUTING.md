# Contributing to Apex

Thank you for considering a contribution.

## Ground rules

* Apex is a text editor, not an IDE, and has **no plugin system** by design.
  Features are built in and configured declaratively (TOML). Proposals for a
  scripting runtime or dynamically loaded extensions will be declined.
* Data integrity beats everything else. Any change touching saving, loading,
  undo or selections needs tests, and a regression test for every fixed
  data-loss defect.
* British English in documentation and user-facing text.
* The licence is AGPL-3.0-or-later; new dependencies must be compatible (MIT,
  Apache-2.0, BSD, MPL-2.0, zlib, OFL for fonts). Run `cargo deny check` if you
  add one.

## Workflow

```sh
make check      # fmt --check, clippy -D warnings, tests
make run ARGS=README.md
```

Before opening a pull request, run the manual smoke test in
`docs/development/manual-smoke-test.md` if you touched `apex-ui`.

## Code organisation

See `ARCHITECTURE.md`. Dependencies flow towards `apex-buffer`; nothing below
`apex-ui` may depend on egui. Public interfaces, invariants and threading
assumptions are documented in the source.

## Adding a command

1. Add a variant to the `commands!` macro in `crates/apex-ui/src/command.rs`
   with a stable id (`category.name`), a name and a category.
2. Implement it in `ApexApp::execute`.
3. Bind it in `assets/keymap-default.toml` and add it to a menu if appropriate.
4. Update `FEATURE_MATRIX.md`.

## Adding a theme

Copy `assets/themes/apex-dark.toml`, change the values, and add it to
`BUILTIN_THEME_SOURCES` in `crates/apex-ui/src/theme.rs`. Partial themes
inherit missing colours from the dark or light base.
