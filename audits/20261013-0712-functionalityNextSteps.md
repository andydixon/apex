# Functionality and Next Steps Report

Companion to `20261013-0712-securityAudit.md`. Covers Apex at `main` after the
audit fixes. Evidence comes from reading the code, running the test suites,
and comments added during this pass (every module now has a header and
documented functions).

## 1. Executive Summary

Apex is feature-rich for a 0.0.1 editor. Fast editing, multiple cursors,
search, a full Git workflow, LSP, Vim mode and packaging for three platforms
are all present and tested on Linux. The highest-impact work now is
**verification on real platforms**, not new features. CI cannot run (GitHub
billing), the new macOS file-panel code has not run on a Mac, and Windows
has only been cross-compiled.

The next most valuable investments:

1. **A headless application harness.** Most interaction logic lives in
   `ApexApp`, which can only be tested through Xvfb scripts that click
   fixed coordinates. Those scripts broke twice during this audit for
   reasons unrelated to the code.
2. **Small correctness fixes** found while commenting (section 2). One was
   confirmed and fixed (whole-document sort moved the final newline to the
   top).
3. **Making trust discoverable.** Trust now gates language servers too, so
   users will see "not started: folder not trusted" more often. A one-click
   **Trust** action there would remove the friction.

## 2. Current Functional Risks

| Risk | Evidence | Impact | Status |
|---|---|---|---|
| macOS native file panels unverified on hardware | `crates/apex-ui/src/macos_dialogs.rs` written from a `sample` trace; type-checked for `aarch64-apple-darwin` only | Open and Save may still misbehave on a Mac | Needs a Mac run |
| CI does not run | every Actions job fails with "recent account payments have failed or your spending limit needs to be increased" | no Windows or macOS build or test signal | Needs billing fix |
| Whole-document sort, reverse and unique moved the trailing newline | `ops::transform_line_block`; regression test `sort_keeps_trailing_newline` | visible text corruption on a common action | **Fixed** (`a23a985`) |
| Status-bar menus 4 and 5 are unreachable | `show_status_bar` picks menus by glyphs (`⎇`, `✗`) that nothing emits | clicking the git label does not open the intended menu | Open |
| `--reuse-window` has no effect beyond a log line | `apex-app/src/main.rs` | the flag is documented but inert (reuse is already the default) | Open: document or remove |
| Doc comment and behaviour mismatches | `paste` (whole-line paste said to go above the caret line), `hard_wrap` (rewraps the whole document with no selection) | users misled by docs | Open: verify, then fix code or docs |
| Dead or unused state | `InsertOptions::auto_indent`, `ApplyOptions::remove_co_authors`, `lsp.rs` `restarts`, `lsp_ui.rs` `last_trigger` and `disabled_pairs`, a no-op `stops.get(&0)` block in `snippet::expand` | confusion during maintenance; options that look effective are not | Open: remove or wire up |
| "Run file" tasks need `sh` | `tasks::task_for_file` (Rust variant) | Rust run-file does not work on Windows | Open |
| Language servers now need trust | `lsp_ui::ensure_client` | existing users see no completion in untrusted folders | Intended; improve the prompt (section 6) |

## 3. Code Quality Improvements

* **`app.rs` is 4,600 lines.** It holds dialogs, the status bar, printing,
  URL opening, hex preview and date helpers next to the frame loop.
  Splitting out `dialogs.rs` (the `Dialog` enum and `show_dialogs`) and
  `platform.rs` (print, open URL, reveal, local time) would make reviews
  easier. Move code only, one module per commit, and run the end-to-end
  scripts after each step.
* **Trust checks are repeated in six places** (`git_trusted`, `gh_ready`,
  the branch, panel and history commands, and the new LSP gate). One
  `require_trust(root, then) -> bool` helper would make a missed gate (as in
  SA-01) harder to write. It would also suit a grep-based lint in CI: "any
  `Command::new` or `git_run` outside this list must call `require_trust`".
* **Commands carry packed string arguments** (`name␟remote`, `SEP`
  separators). A typed argument enum would remove parsing in
  `git_branches.rs` and `forge_ui.rs`.
* **Status-bar menus are chosen by glyph** (see section 2). Use an explicit
  enum.
* **Unused fields and options** (section 2): delete them or connect them.

## 4. Frontend Interaction and Error-Handling Improvements

Apex's interface is egui, not HTML, so this section covers the dialog and
event model.

* **Trust prompts:** when a language server is held back, show a status-bar
  button that opens the Trust dialog, as git commands already do.
* **Askpass dialog:** consider showing which remote and host asked, from
  the prompt text, so users do not type a password into the wrong prompt.
  Also clear the field on cancel (see SA-10).
* **Error messages from git** are reduced to one line, which is good. Add a
  "Show Git Output" link to every git error status so the full log is one
  click away.
* **Panics in background jobs** are contained, but the user only sees the
  job vanish. Report "Git command failed unexpectedly; see the log" when a
  job thread ends without a `Finished` event.

## 5. Testing Roadmap

* **Headless `ApexApp` harness:** construct the app with an
  `egui::Context` and no window (eframe allows a `CreationContext` built
  for tests), then drive `execute(cmd, arg)` and inspect `dialog`, `docs`
  and the git job queue. This turns trust gating, branch argument handling
  and dialog flows into unit tests.
* **End-to-end robustness:**
  * Give each `tests/visual/*.sh` script a unique display number. This
    audit hit a collision: a leftover 1024×700 Xvfb on `:79` made
    `git.sh` miss two buttons.
  * Check the screen size at start-up.
  * Locate buttons from AccessKit or window-relative positions, not
    absolute pixels.
* **Fuzzing:** add targets for `git_cli::parse_patch`,
  `git_history::parse_log`, `forge::web_repo` and the LSP framing reader.
  All parse output from other programs.
* **Abuse cases** to keep as regression tests:
  * a symlink to `/dev/zero` (added)
  * a hostile run-file name (added)
  * a ref named `--orphan=x` (needs the harness)
  * a project file with `lsp_servers` (exists)
* **Platform runs:** once CI works, make the Windows and macOS jobs run
  the unit tests. Add a macOS smoke job that opens and cancels the file
  panel through AppleScript UI scripting.
* **Load:** keep the existing benchmarks. Add a 1 GB file open-and-scroll
  timing to catch regressions in large-file mode.

## 6. Recommended New Functionality

* **One-click trust from the status bar** when something was held back
  (language server, task, git). This is direct follow-up to SA-01.
* **Trust scope display:** list trusted folders under **Project > Trust
  Workspace**, with Remove. Say that subfolders are included (SA-12).
* **Multi-file commit view** in the log (each file as a side-by-side diff),
  listed as not done in the Git plan.
* **Inline PR review comments and Actions logs** through `gh`, also listed
  as not done.
* **Settings for recovery snapshots:** a switch to turn them off for
  sensitive folders (SA-13).

## 7. Next-Level Opportunities

* **Accessibility:** the custom text widget is not exposed to screen
  readers (listed in `FEATURE_MATRIX.md`). AccessKit text-range support
  would make Apex usable with VoiceOver and NVDA, and would also give the
  end-to-end tests stable element handles.
* **Language-server sandboxing:** on Linux, run servers under Landlock or
  bubblewrap with the workspace mounted, so trusting a folder does not mean
  trusting its build scripts with the whole home directory.
* **Release engineering:**
  * signed and notarised macOS builds
  * signed Windows installer
  * SHA-256 checksums
  * reproducible-build notes
  * `cargo deny` for licences and duplicates alongside `cargo audit`
* **Performance observability:** a hidden frame-time overlay (already
  measurable in benches) to catch slow frames on real hardware, especially
  macOS Retina.

## 8. Prioritised Action Plan

| When | Action | Why | Risk |
|---|---|---|---|
| Immediate | Fix GitHub billing so CI runs | no cross-platform signal at all today | none |
| Immediate | On a Mac: `git pull && make release`, then try Open Folder, Open File, Save As, Finder Open With and the Dock icon | the macOS panel fix is untested on hardware | low |
| Short term | Status-bar Trust button for held-back servers | the new LSP gate otherwise looks like a regression | low |
| Short term | Fix the glyph-based status-bar menus, `--reuse-window`, and the doc mismatches | small, user-visible correctness | low |
| Short term | Unique Xvfb displays and a screen-size check in `tests/visual/*.sh` | flaky tests hide real failures | none |
| Medium term | Headless `ApexApp` harness and unit tests for trust and dialogs | most logic is only end-to-end tested | moderate refactor |
| Medium term | Split `app.rs`; one `require_trust` helper | maintainability of security controls | moderate |
| Medium term | Fuzz targets for git and forge parsers | they parse external program output | none |
| Long term | Signed releases, accessibility of the text widget, server sandboxing | trust in distribution, reach, defence in depth | larger projects |

## 9. Caveats and Validation Needed

* **Code-reading only:** the unreachable status-bar menus, the paste and
  hard-wrap mismatches, and the unused fields were found by reading. Each
  should be confirmed in the running editor before changing behaviour.
* **Users may object to the LSP trust gate** if they are used to completion
  appearing at once. Watch for feedback and make the prompt prominent.
* **Windows has only been type-checked from Linux** with a stub C compiler.
  Nothing in this report has been run on Windows.
* **The macOS fix is based on one `sample` trace.** If the panel still does
  not appear, the next diagnostics are:
  * run unbundled with `cargo run --release -- --new-window`
  * `codesign --verify --deep --strict --verbose=2 /Applications/Apex.app`
  * the `com.apple.ViewBridge` log during the attempt
