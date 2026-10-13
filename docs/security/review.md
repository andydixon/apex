# Security review

Scope: Apex 0.0.1 as built from this repository, including the Git and GitHub integration added after the 0.0.1 tag. This is a review of the
trust boundaries in the code, what each one relies on, and what is known to
be missing. It complements `SECURITY.md` (reporting, threat model) and
`docs/security/file-handling.md` (data integrity).

## What Apex is and is not

Apex is a local, offline editor. It never opens a network connection,
reads no credentials, has no accounts, telemetry or update checks, and
embeds no scripting runtime. The only ways for data to enter the process
are: files the user opens (and their directory listings), configuration
files in the user's own directories, language-server processes the user
has installed, build tasks the user runs, the clipboard, and the loopback
single-instance socket. Those are the boundaries reviewed below.

## 1. Untrusted file contents

**Threat**: a crafted file crashes the editor, hangs it, or makes it write
somewhere unexpected.

* Loading is bounded: `fileio::load` reads into memory, decodes with
  `encoding_rs` (never panics on malformed input; replacement characters
  are inserted and the file is marked lossy), and files above
  `large_file_mb` open in large-file mode with highlighting, minimap detail,
  word wrap and spell checking off. A 500 MB file is still one allocation;
  there is no memory-mapped or streamed mode (known limitation).
* Binary detection (NUL bytes / UTF-16 heuristics) sends binaries to a
  read-only hex preview; saving a hex preview is refused.
* Every offset conversion is total (`apex-buffer` fuzz target `offsets`),
  and every edit goes through `Transaction::clamped` so out-of-range
  positions from any source cannot index past the rope.
* Tree-sitter parsing is time-budgeted per frame and moves to a background
  thread for large files; pathological inputs degrade to unhighlighted text
  rather than a stalled UI. Grammars are compiled in (no grammar loading
  from disk), so there is no WASM or dynamic library loading.
* Regular expressions from the find bar, Find in Files and problem matchers
  are compiled with size limits (`search::Query`), and matches are capped.
* Rendering only lays out visible lines, and a single line is capped at
  `MAX_LAYOUT_CHARS` for shaping, so a multi-megabyte line cannot exhaust
  the text shaper.

**Residual risk**: tree-sitter grammars are C code compiled from
third-party crates; a grammar bug on crafted input could crash the process
(not escalate). Recovery snapshots limit the damage of a crash to the last
few seconds of typing.

## 2. Configuration files

**Threat**: a malformed `settings.toml`, key map or theme disables the
editor.

* All three parse with full diagnostics and fall back to defaults per key;
  values are clamped (`Settings::sanitise`). The `settings` fuzz target
  covers the three parsers. Unknown keys are reported, never silently
  applied.
* Project files (`*.apex-project.toml`) may come from a repository, so they
  are treated as untrusted: their `[settings]` cannot point at programs
  (no `lsp_servers` override from project files; see "Known limitations")
  and their `[[tasks]]` do nothing until the workspace is trusted (below).

## 3. Running programs: build tasks

**Threat**: opening a repository runs attacker-chosen commands.

* Tasks defined in a project file are inert until **Project > Trust
  Workspace** has been confirmed for that folder. The decision is stored in
  the user's data directory, keyed by canonical path, never in the
  repository, so cloning a repo cannot pre-trust itself.
* Detected tasks (`cargo build`, `make`, …) run the well-known tool names
  from `PATH`; `PATH` itself is not taken from the project. The exact
  command and directory are printed as the first line of the output.
* Commands are spawned without a shell unless `shell = true` is set
  explicitly in the task; arguments are passed as an argv vector.
  Environment: the editor's environment plus the task's `env`; nothing is
  injected from settings.
* Processes run in their own process group on Unix (`killpg` on cancel)
  and are killed with `taskkill /T` on Windows, so Cancel takes the whole
  tree; a `timeout_secs` cap is enforced.
* Output is sanitised (`tasks::sanitise_output`): ANSI/OSC escape sequences
  are stripped before display, so a build cannot forge UI text or exploit
  terminal-style escapes.

## 4. Language servers

**Threat**: a malicious or buggy server feeds the client bad data.

* Servers are started only from `known_servers()` (fixed argv for programs
  found on `PATH`) or the user's own `lsp_servers` setting. Project files
  cannot configure servers.
* Servers start only in a **trusted** folder. A server works on the whole
  folder and some execute it: rust-analyzer runs `cargo`, which runs build
  scripts, proc macros and the repository's `.cargo/config.toml`. Before
  October 2026 opening a single `.rs` file from an untrusted clone was
  enough to run its build script; `tests/visual/lsp-trust.sh` now checks
  that no server starts until the folder is trusted.
* The client parses JSON-RPC with a message size limit and ignores
  malformed frames; every position from the server is converted with
  clamped UTF-16 arithmetic; ranges are validated against the current
  document version before an edit is applied.
* `workspace/applyEdit` and rename results that touch files not open in
  the editor are listed and confirmed by the user before being written
  (atomic save, same path rules as any save).
* The server's stderr is captured for diagnostics only; the editor sends
  only document contents the user opened and the workspace folder list.
* A server that exits repeatedly is left stopped after three restarts.

## 5. Single-instance socket and `--wait`

**Threat**: another local process injects "open this file" requests.

* The listener binds `127.0.0.1` on an ephemeral port; the port and a
  256-bit token from the operating system's CSPRNG (`apex_project::token`) are written to `instance.json` in the per-user
  runtime directory with owner-only permissions (`ensure_private_dir`,
  0700/0600). A client must present the token; comparison is constant
  time and a mismatch closes the connection without a reply.
* Requests are one JSON line, capped at 1 MiB, with a read timeout; they
  can only ask the editor to open paths, show a diff, or wait for a tab to
  close. No command execution is reachable through the socket.
* On Windows the runtime directory inherits the user profile's ACLs; the
  token is the protection there.

**Residual risk**: a process running as the same user can read the token
and open files in the editor — equivalent to what it could do by running
`apex` itself.

## 6. Sessions, recovery and the user dictionary

* Written atomically to the user's data directory with private
  permissions; parsed with serde and rejected (renamed `*.damaged`) when
  invalid, so corruption cannot stop start-up (`session` fuzz target).
* Recovery snapshots contain unsaved text; they live only in that private
  directory and are deleted when the file is saved or the restore is
  declined.

## 7. Git and GitHub

* **Reading** (status, gutter marks, blame, log, branches, diffs) uses
  libgit2 and runs no programs, so it works in untrusted folders.
* **Writing and network** (staging, commits, branches, merges, stash,
  fetch/pull/push, history rewriting) run the user's `git`, and GitHub
  features run the GitHub CLI `gh`. `git` executes repository hooks and
  config-named programs (`core.fsmonitor`, `core.sshCommand`, filters), so
  every such command requires the folder to be **trusted** (the same
  prompt as build tasks). Arguments are passed as an argv list, never
  through a shell; commit messages and patches go through stdin.
* Commands run with `GIT_TERMINAL_PROMPT=0`, `GH_PROMPT_DISABLED=1`, no
  stdin and, on Unix, in a new session (`setsid`) so nothing can wait on a
  terminal the user cannot see; a job can be cancelled (process group
  killed).
* **Credentials**: Apex never stores passwords or tokens. When git or ssh
  needs one, it runs Apex itself as `GIT_ASKPASS`/`SSH_ASKPASS`; the helper
  connects to a loopback listener opened for that one job (random port,
  256-bit CSPRNG token compared in constant time, reads capped at 64 KiB), the prompt is shown in a dialog,
  and the answer is written back once. The listener closes when the job
  ends; an unanswered prompt times out after ten minutes. `gh` keeps its
  own login.
* **Destructive operations ask first**: discard changes, undo commit,
  force push (with lease), branch delete, stash drop, abort merge/rebase,
  revert commit, detached checkout. **Edit Commit History** refuses a dirty
  tree, tags a backup that keeps pointing at the old history, rewrites only
  local branches and tags, and needs `FORCE` typed before a force push.
  Its filter scripts are written to a private (0700) temporary directory
  with every value single-quoted for `sh`, and deleted afterwards.
* **Links**: remote URLs are built from the configured remote and only
  `https://` links are passed to the system's opener; path segments are
  percent-encoded.

## 8. Dependencies and build

* All crates are from crates.io with permissive licences compatible with
  AGPL-3.0-or-later (`CONTRIBUTING.md` states the rule; `cargo tree` and
  `cargo license` list them). libgit2 is built from the vendored source in
  the `git2` crate; tree-sitter grammars are compiled from their crates.
* Release builds use `panic = "unwind"` so a panic in a background thread
  (syntax, search, git) is contained by that thread; the UI thread wraps
  background results and reports failures as status messages.

## Known limitations (security-relevant)

1. **No sandboxing of child processes.** Build tasks and language servers
   run with the user's full privileges, as in every mainstream editor;
   trust gating is the mitigation.
2. **Project files cannot be fully isolated.** `[settings]` from a project
   can change editing behaviour (indentation, encoding, formatting on
   save). Keys that name programs are deliberately not honoured from
   project files.
3. **Whole-file loading.** Very large files cost their size in memory.
4. **No signature verification of dictionaries, themes or snippets.** They
   are plain data in the user's own directory; a malicious snippet can only
   insert text.
5. **macOS Apple Event handler** accepts file URLs from the system; any app
   that can send Apple Events to Apex (same user session) can ask it to
   open files, which is the platform's normal behaviour.
6. **Windows runtime directory permissions** rely on the profile ACLs
   rather than explicit 0600 modes.
7. **Tasks inherit the editor's environment.** A build task sees every
   variable Apex was started with (as a command typed in a terminal
   would); only loader and shell start-up variables in a task's own `env`
   are refused.
8. **Trusted repositories run hooks.** Trusting a folder lets its hooks
   run whenever Apex runs `git` there, exactly as on the command line.

## Audit of October 2026

`audits/` holds a full security audit and a functionality report. Fixed
in that pass: language servers in untrusted folders (above); a shell
injection through the file name in the "Rust: run file" task, which also
built into a fixed `/tmp` path; opening a symlink to `/dev/zero` or a
FIFO (unbounded read or a hung editor); a predictable print temp
directory open to a symlink attack; tokens derived from a hasher rather
than a CSPRNG; option injection through repository ref names; and two
git2 soundness advisories (RUSTSEC-2026-0183, RUSTSEC-2026-0184).

## Verification performed

* `cargo test --workspace` (unit, property and integration tests) and
  `cargo clippy -- -D warnings` on every commit.
* Fuzz targets built with `cargo +nightly fuzz build`; see
  `docs/performance/benchmarks.md` for the run log of each target.
* Scripted Xvfb sessions for the editing path, Vim mode, single-instance
  IPC and `--wait`.
