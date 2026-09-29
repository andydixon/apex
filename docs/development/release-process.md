# Release process

1. **Freeze and verify** on the release branch:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo build --release
   cd fuzz && for t in transactions offsets settings search session; do cargo +nightly fuzz run $t -- -max_total_time=120; done; cd ..
   ```

   Run `docs/development/manual-smoke-test.md` on each platform you ship
   for, and `tests/visual/smoke.sh` on Linux. Re-run `benches/startup.sh`
   and `cargo bench --workspace`; record the numbers and the machine in
   `docs/performance/benchmarks.md` (replace, do not append estimates).

2. **Update the documents**: version in the workspace `Cargo.toml`
   (`[workspace.package] version`), `CHANGELOG.md` (move Unreleased to the
   version with the date), `FEATURE_MATRIX.md` statuses, `PROGRESS.md`
   test counts. Commit as `Release x.y.z`.

3. **Tag**: `git tag -a vx.y.z -m "Apex x.y.z"` and push the tag.

4. **Build packages** on each platform from the tag:

   | Platform | Command | Produces |
   |---|---|---|
   | Linux | `packaging/linux/build.sh` | tarball with `install.sh`, `.deb`, AppImage if `appimagetool` is installed |
   | macOS | `APEX_UNIVERSAL=1 APEX_SIGN_IDENTITY=… packaging/macos/build.sh`, then notarise (see `packaging/macos/README.md`) | `Apex.app`, DMG |
   | Windows | `powershell -File packaging\windows\build.ps1` with Inno Setup 6 installed | portable zip, `Apex-x.y.z-setup.exe` |

   Icons are regenerated with `packaging/icons/generate.sh` only when
   `assets/icons/apex.svg` changes; the generated files are committed so
   builds need no image tools.

5. **Publish**: attach the artefacts and the `CHANGELOG.md` section to the
   release. Include SHA-256 sums (`sha256sum dist/*`).

6. **After the release**: bump the version to the next `-dev`, add an
   Unreleased section to the changelog.

## Version metadata

`apex --version` and the About dialog read `CARGO_PKG_VERSION`; the macOS
`Info.plist` and the Windows resource (`crates/apex-app/build.rs`) take the
same value from `Cargo.toml` at build time, so there is one place to bump.

## Licence compliance

Before tagging, regenerate the dependency licence list
(`cargo install cargo-license && cargo license --json > docs/licenses.json`)
and check that nothing non-permissive has arrived. Bundled fonts and their
licences are listed in `README.md`.
