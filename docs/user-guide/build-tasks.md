# Build tasks

**Tools > Build** (`Ctrl+B`) runs the default task for the workspace;
**Build With…** (`Ctrl+Shift+B`) lists every task. Output streams into the
panel at the bottom; `Escape` or **Cancel Build** stops the process (the
whole process group is killed, so child processes go too).

## Where tasks come from

* **Detected**: `Cargo.toml` → cargo build/test/run/clippy/check;
  `go.mod` → go build/test/vet; `package.json` → npm test/build;
  `Makefile` → make, make test; `CMakeLists.txt` → configure/build;
  `pyproject.toml`/`setup.py`/`pytest.ini` → pytest. Single files get
  "run file" tasks for Python, Rust (outside `src/`), shell, JavaScript, Go
  and Lua.
* **Project file**: `[[tasks]]` entries (see below).

```toml
[[tasks]]
name = "Test (verbose)"
command = ["cargo", "test", "--", "--nocapture"]
cwd = "crates/apex-edit"          # relative to the workspace root
env = { RUST_BACKTRACE = "1" }
shell = false                     # true runs command[0] through sh -c / cmd /C
timeout_secs = 600                # 0 = no limit
problem_matcher = "rustc"         # rustc, gcc, go, python, generic, or a regex
```

A custom matcher is a regular expression with named groups `file`, `line`,
optional `col` and `message`, for example
`problem_matcher = "^(?P<file>[^:]+):(?P<line>\\d+): (?P<message>.*)$"`.

## Trust

Tasks that come from a project file can run arbitrary programs, so they are
disabled until you trust the workspace: the first attempt shows the command
and asks. **Project > Trust Workspace** manages the decision, which is
remembered per folder in the data directory and never inside the
repository. Detected tasks (cargo, make and friends) run without a prompt;
the first line of the Output panel is always the exact command and
directory used.

Commands are never passed through a shell unless `shell = true`, and
output is sanitised of terminal escape sequences before it is displayed.
A task inherits Apex's environment (as a command typed in a terminal
would) plus its own `env`; `env` cannot set the dynamic loader or shell
start-up variables (`LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`).

## Problems

Matched errors and warnings appear in the **Problems** tab with file, line
and message; click one to jump there, and the lines are underlined in the
editor until the next build. Language-server diagnostics share the same
list. The status bar shows the count.
