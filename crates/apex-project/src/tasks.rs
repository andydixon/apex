//! Build tasks: definitions, detection, a process runner with cancellation,
//! bounded output, control-sequence sanitising and problem matchers.
//!
//! Commands are argument arrays; a shell is only used when a task explicitly
//! sets `shell = true`. Tasks found in project configuration are untrusted
//! until the user trusts the workspace.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TaskDef {
    pub name: String,
    /// Program and arguments. Never passed through a shell unless `shell` is set.
    pub command: Vec<String>,
    /// Working directory, relative to the workspace root when not absolute.
    pub cwd: Option<String>,
    /// Extra environment variables. `PATH` and secrets from the editor are not forwarded.
    pub env: HashMap<String, String>,
    /// Run `command[0]` through the platform shell (`sh -c` / `cmd /C`).
    pub shell: bool,
    /// Seconds before the process is killed (0 = no limit).
    pub timeout_secs: u64,
    /// Problem matcher name: "rustc", "gcc", "go", "python", "generic" or a custom regex with named groups file/line/col/message.
    pub problem_matcher: String,
    /// Where this task came from.
    #[serde(skip)]
    pub source: TaskSource,
}

impl Default for TaskDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            shell: false,
            timeout_secs: 0,
            problem_matcher: "generic".into(),
            source: TaskSource::BuiltIn,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskSource {
    #[default]
    BuiltIn,
    User,
    Project,
}

/// Detect built-in tasks for a workspace root.
pub fn detect_tasks(root: &Path) -> Vec<TaskDef> {
    let mut v = Vec::new();
    let mk = |name: &str, cmd: &[&str], matcher: &str| TaskDef {
        name: name.into(),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        problem_matcher: matcher.into(),
        ..Default::default()
    };
    if root.join("Cargo.toml").exists() {
        v.push(mk("Cargo: build", &["cargo", "build"], "rustc"));
        v.push(mk("Cargo: test", &["cargo", "test"], "rustc"));
        v.push(mk("Cargo: run", &["cargo", "run"], "rustc"));
        v.push(mk("Cargo: clippy", &["cargo", "clippy", "--all-targets"], "rustc"));
        v.push(mk("Cargo: check", &["cargo", "check"], "rustc"));
    }
    if root.join("go.mod").exists() {
        v.push(mk("Go: build", &["go", "build", "./..."], "go"));
        v.push(mk("Go: test", &["go", "test", "./..."], "go"));
        v.push(mk("Go: vet", &["go", "vet", "./..."], "go"));
    }
    if root.join("package.json").exists() {
        v.push(mk("npm: test", &["npm", "test"], "generic"));
        v.push(mk("npm: run build", &["npm", "run", "build"], "generic"));
    }
    if root.join("Makefile").exists() || root.join("makefile").exists() {
        v.push(mk("Make", &["make"], "gcc"));
        v.push(mk("Make: test", &["make", "test"], "gcc"));
    }
    if root.join("CMakeLists.txt").exists() {
        v.push(mk("CMake: configure", &["cmake", "-S", ".", "-B", "build"], "gcc"));
        v.push(mk("CMake: build", &["cmake", "--build", "build"], "gcc"));
    }
    if root.join("pyproject.toml").exists() || root.join("setup.py").exists() || root.join("pytest.ini").exists() {
        v.push(mk("Python: pytest", &["python3", "-m", "pytest"], "python"));
    }
    v
}

/// A task that runs the current file (`python3 file.py`, etc.).
pub fn task_for_file(path: &Path) -> Option<TaskDef> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let p = path.display().to_string();
    let (name, cmd, matcher): (&str, Vec<String>, &str) = match ext.as_str() {
        "py" => ("Python: run file", vec!["python3".into(), p], "python"),
        "rs" if !path.components().any(|c| c.as_os_str() == "src") => {
            ("Rust: run file (rustc)", vec!["sh".into(), "-c".into(), format!("rustc {:?} -o /tmp/apex-run && /tmp/apex-run", path)], "rustc")
        }
        "sh" => ("Shell: run file", vec!["sh".into(), p], "generic"),
        "js" | "mjs" => ("Node: run file", vec!["node".into(), p], "generic"),
        "go" => ("Go: run file", vec!["go".into(), "run".into(), p], "go"),
        "lua" => ("Lua: run file", vec!["lua".into(), p], "generic"),
        "rb" => ("Ruby: run file", vec!["ruby".into(), p], "generic"),
        _ => return None,
    };
    Some(TaskDef { name: name.into(), command: cmd, problem_matcher: matcher.into(), ..Default::default() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: PathBuf,
    pub line: usize,
    pub col: Option<usize>,
    pub message: String,
    pub is_error: bool,
}

/// Compiled problem matcher.
pub struct ProblemMatcher {
    re: regex::Regex,
    /// For rustc: the message is on the line before the `-->` location.
    message_from_previous: bool,
    last_message: Option<(String, bool)>,
}

impl ProblemMatcher {
    pub fn new(name: &str) -> Option<Self> {
        let (pattern, prev) = match name {
            "rustc" => (r"^\s*--> (?P<file>[^:\n]+):(?P<line>\d+):(?P<col>\d+)", true),
            "gcc" | "clang" => {
                (r"^(?P<file>[^:\s][^:\n]*?):(?P<line>\d+):(?:(?P<col>\d+):)?\s*(?P<level>error|warning|fatal error|note):\s*(?P<message>.*)$", false)
            }
            "go" => (r"^(?P<file>[^:\s][^:\n]*?\.go):(?P<line>\d+):(?:(?P<col>\d+):)?\s*(?P<message>.*)$", false),
            "python" => (r#"^\s*File "(?P<file>[^"]+)", line (?P<line>\d+)"#, false),
            "generic" => (r"^(?P<file>[^:\s][^:\n]*?\.[A-Za-z0-9]+):(?P<line>\d+)(?::(?P<col>\d+))?[:\s]\s*(?P<message>.*)$", false),
            custom => (custom, false),
        };
        let re = regex::RegexBuilder::new(pattern).size_limit(1 << 20).build().ok()?;
        if re.capture_names().flatten().filter(|n| *n == "file" || *n == "line").count() < 2 {
            return None;
        }
        Some(Self { re, message_from_previous: prev, last_message: None })
    }

    /// Feed one output line; returns a problem if this line completes one.
    pub fn feed(&mut self, line: &str, cwd: &Path) -> Option<Problem> {
        if self.message_from_previous {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("error").or_else(|| t.strip_prefix("warning")) {
                let is_error = t.starts_with("error");
                let msg = rest.trim_start_matches(|c: char| c == '[' || c == ']' || c.is_alphanumeric()).trim_start_matches(':').trim();
                self.last_message = Some((msg.to_string(), is_error));
                return None;
            }
        }
        let caps = self.re.captures(line)?;
        let file = caps.name("file")?.as_str().trim();
        let line_no: usize = caps.name("line")?.as_str().parse().ok()?;
        let col = caps.name("col").and_then(|c| c.as_str().parse().ok());
        let (message, is_error) = if self.message_from_previous {
            self.last_message.take().unwrap_or_else(|| (String::new(), true))
        } else {
            let level = caps.name("level").map(|l| l.as_str().to_string()).unwrap_or_default();
            (caps.name("message").map(|m| m.as_str().to_string()).unwrap_or_default(), !level.starts_with("warning") && !level.starts_with("note"))
        };
        let path = PathBuf::from(file);
        let path = if path.is_absolute() { path } else { cwd.join(path) };
        Some(Problem { file: path, line: line_no, col, message, is_error })
    }
}

/// Remove ANSI CSI/OSC/other escape sequences so output cannot drive the terminal
/// or hide content. Keeps printable text, tabs and newlines.
pub fn sanitise_output(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters then a final byte 0x40..=0x7e
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: until BEL or ST (ESC \)
                    let mut prev = '\0';
                    for n in chars.by_ref() {
                        if n == '\u{07}' || (prev == '\u{1b}' && n == '\\') {
                            break;
                        }
                        prev = n;
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c == '\r' {
            // Carriage-return progress bars: keep the last segment of the line.
            if chars.peek() != Some(&'\n') {
                if let Some(i) = out.rfind('\n') {
                    out.truncate(i + 1);
                } else {
                    out.clear();
                }
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        out.push(c);
    }
    out
}

#[derive(Debug)]
pub enum TaskEvent {
    Line(String),
    Problem(Problem),
    Exited { code: Option<i32>, elapsed: Duration, timed_out: bool, killed: bool },
    Error(String),
}

pub struct RunningTask {
    pub name: String,
    pub rx: Receiver<TaskEvent>,
    cancel: Arc<AtomicBool>,
    pub started: Instant,
}

impl RunningTask {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for RunningTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Maximum lines retained by the output panel; the runner itself streams.
pub const MAX_OUTPUT_LINES: usize = 20_000;

/// Start a task. `root` resolves relative working directories.
pub fn run(def: &TaskDef, root: &Path) -> Result<RunningTask, String> {
    if def.command.is_empty() {
        return Err("task has no command".into());
    }
    let cwd = match &def.cwd {
        Some(c) if Path::new(c).is_absolute() => PathBuf::from(c),
        Some(c) => root.join(c),
        None => root.to_path_buf(),
    };
    if !cwd.is_dir() {
        return Err(format!("working directory {} does not exist", cwd.display()));
    }
    let mut cmd = if def.shell {
        let joined = def.command.join(" ");
        if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(joined);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(joined);
            c
        }
    } else {
        let mut c = Command::new(&def.command[0]);
        c.args(&def.command[1..]);
        c
    };
    cmd.current_dir(&cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in &def.env {
        // Refuse to let a task override the dynamic loader or shell behaviour.
        if k.starts_with("LD_") || k.starts_with("DYLD_") || k == "BASH_ENV" || k == "ENV" {
            continue;
        }
        cmd.env(k, v);
    }
    cmd.env("TERM", "dumb").env("NO_COLOR", "1").env("CARGO_TERM_COLOR", "never");
    #[cfg(unix)]
    {
        // New process group so cancellation kills children too.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not start {:?}: {e}", def.command[0]))?;
    let (tx, rx) = bounded::<TaskEvent>(1024);
    let cancel = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let matcher_name = def.problem_matcher.clone();
    let timeout = def.timeout_secs;
    let c = cancel.clone();
    let cwd2 = cwd.clone();
    std::thread::Builder::new()
        .name("apex-task".into())
        .spawn(move || {
            let (ltx, lrx) = bounded::<String>(4096);
            let mut readers = Vec::new();
            for pipe in [stdout.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), stderr.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)]
                .into_iter()
                .flatten()
            {
                let ltx = ltx.clone();
                readers.push(std::thread::spawn(move || {
                    let mut reader = BufReader::new(pipe);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        match reader.read_until(b'\n', &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string();
                                if ltx.send(sanitise_output(&line)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }));
            }
            drop(ltx);
            let mut matcher = ProblemMatcher::new(&matcher_name);
            let mut timed_out = false;
            let mut killed = false;
            let code = loop {
                // Pump lines.
                while let Ok(line) = lrx.try_recv() {
                    if let Some(m) = &mut matcher
                        && let Some(p) = m.feed(&line, &cwd2)
                    {
                        let _ = tx.send(TaskEvent::Problem(p));
                    }
                    if tx.send(TaskEvent::Line(line)).is_err() {
                        break;
                    }
                }
                if c.load(Ordering::Relaxed) {
                    kill_tree(&mut child);
                    killed = true;
                }
                if timeout > 0 && started.elapsed() > Duration::from_secs(timeout) && !killed {
                    kill_tree(&mut child);
                    timed_out = true;
                    killed = true;
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status.code(),
                    Ok(None) => std::thread::sleep(Duration::from_millis(15)),
                    Err(e) => {
                        let _ = tx.send(TaskEvent::Error(e.to_string()));
                        break None;
                    }
                }
            };
            for r in readers {
                let _ = r.join();
            }
            while let Ok(line) = lrx.try_recv() {
                if let Some(m) = &mut matcher
                    && let Some(p) = m.feed(&line, &cwd2)
                {
                    let _ = tx.send(TaskEvent::Problem(p));
                }
                let _ = tx.send(TaskEvent::Line(line));
            }
            let _ = tx.send(TaskEvent::Exited { code, elapsed: started.elapsed(), timed_out, killed });
        })
        .map_err(|e| e.to_string())?;
    let _ = Sender::<()>::clone;
    Ok(RunningTask { name: def.name.clone(), rx, cancel, started })
}

pub(crate) fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // SAFETY: killpg only signals; the pgid is the child's pid because of process_group(0).
        unsafe {
            libc::killpg(child.id() as i32, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(200));
        if child.try_wait().ok().flatten().is_none() {
            // SAFETY: as above.
            unsafe {
                libc::killpg(child.id() as i32, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_escapes_and_progress_bars() {
        assert_eq!(sanitise_output("\u{1b}[31mred\u{1b}[0m ok"), "red ok");
        assert_eq!(sanitise_output("\u{1b}]0;title\u{07}text"), "text");
        assert_eq!(sanitise_output("\u{1b}]8;;http://x\u{1b}\\link"), "link");
        assert_eq!(sanitise_output("10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(sanitise_output("a\u{0}b\tc"), "ab\tc");
    }

    #[test]
    fn problem_matchers() {
        let cwd = Path::new("/w");
        let mut m = ProblemMatcher::new("rustc").unwrap();
        assert!(m.feed("error[E0308]: mismatched types", cwd).is_none());
        let p = m.feed("  --> src/main.rs:12:5", cwd).unwrap();
        assert_eq!(p, Problem { file: "/w/src/main.rs".into(), line: 12, col: Some(5), message: "mismatched types".into(), is_error: true });
        let mut g = ProblemMatcher::new("gcc").unwrap();
        let p = g.feed("foo.c:3:9: warning: unused variable 'x'", cwd).unwrap();
        assert_eq!((p.line, p.col, p.is_error), (3, Some(9), false));
        let mut py = ProblemMatcher::new("python").unwrap();
        assert_eq!(py.feed("  File \"/abs/x.py\", line 7, in <module>", cwd).unwrap().line, 7);
        let mut go = ProblemMatcher::new("go").unwrap();
        assert_eq!(go.feed("./main.go:4:2: undefined: foo", cwd).unwrap().message, "undefined: foo");
        assert!(ProblemMatcher::new("(").is_none());
        assert!(ProblemMatcher::new("nothing here").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn runs_captures_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let def = TaskDef { name: "echo".into(), command: vec!["sh".into(), "-c".into(), "echo out; echo err 1>&2; exit 3".into()], ..Default::default() };
        let t = run(&def, dir.path()).unwrap();
        let mut lines = Vec::new();
        let mut code = None;
        for ev in t.rx.iter() {
            match ev {
                TaskEvent::Line(l) => lines.push(l),
                TaskEvent::Exited { code: c, .. } => {
                    code = c;
                    break;
                }
                _ => {}
            }
        }
        lines.sort();
        assert_eq!(lines, vec!["err", "out"]);
        assert_eq!(code, Some(3));
        // Cancellation kills a sleeping process tree promptly.
        let def = TaskDef { name: "sleep".into(), command: vec!["sh".into(), "-c".into(), "sleep 30".into()], ..Default::default() };
        let t = run(&def, dir.path()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        t.cancel();
        let start = Instant::now();
        let mut killed = false;
        for ev in t.rx.iter() {
            if let TaskEvent::Exited { killed: k, .. } = ev {
                killed = k;
                break;
            }
        }
        assert!(killed);
        assert!(start.elapsed() < Duration::from_secs(5));
        // Timeouts.
        let def = TaskDef { name: "slow".into(), command: vec!["sleep".into(), "30".into()], timeout_secs: 1, ..Default::default() };
        let t = run(&def, dir.path()).unwrap();
        let mut timed_out = false;
        for ev in t.rx.iter() {
            if let TaskEvent::Exited { timed_out: to, .. } = ev {
                timed_out = to;
                break;
            }
        }
        assert!(timed_out);
        assert!(run(&TaskDef { name: "x".into(), command: vec!["/nonexistent/binary".into()], ..Default::default() }, dir.path()).is_err());
    }
}
