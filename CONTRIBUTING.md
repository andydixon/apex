# Contributing to Apex

Thank you for considering a contribution.

## Ground rules

* Apex is a text editor, not an IDE, and has **no plugin system** by design.
  Features are built in and configured declaratively (TOML). Proposals for a
  scripting runtime or dynamically loaded extensions will be declined.
* Data integrity beats everything else. Any change touching saving, loading,
  undo or selections needs tests, and a regression test for every fixed
  data-loss defect.
* No `unwrap`/`expect` on data from files, processes or configuration.
* British English in documentation and user-facing text.
* The licence is AGPL-3.0-or-later; new dependencies must be compatible (MIT,
  Apache-2.0, BSD, MPL-2.0, zlib, OFL for fonts) and are listed by
  `cargo license`. Tree-sitter grammar crates are MIT/Apache-2.0.
* Nothing may contact the network at run time; nothing may send telemetry.

## Workflow

```sh
make check      # fmt --check, clippy -D warnings, tests
make run ARGS=README.md
```

Before opening a pull request, run the manual smoke test in
`docs/development/manual-smoke-test.md` if you touched `apex-ui`, and
`tests/visual/smoke.sh` on Linux. Keep `FEATURE_MATRIX.md` and
`CHANGELOG.md` current with your change.

## Documentation for developers

* `ARCHITECTURE.md` — crates, offset spaces, document model, rendering and
  input pipelines, worker model, syntax engine, persistence, security.
* `docs/development/building-and-testing.md`
* `docs/development/adding-commands.md`
* `docs/development/adding-grammars.md`
* `docs/development/release-process.md`
* `benches/README.md` — benchmarks, profiling and fuzzing.

## Adding a theme

Copy `assets/themes/apex-dark.toml`, change the values, and add it to
`BUILTIN_THEME_SOURCES` in `crates/apex-ui/src/theme.rs`. Partial themes
inherit missing colours from the dark or light base. User themes need no code
change: drop the file into `<config>/themes/`.
