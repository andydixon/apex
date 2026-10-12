//! Running the user's `git` for everything that writes to a repository or
//! talks to a remote. Reads stay on libgit2 (`git.rs`); this module runs
//! commands on a worker thread, streams their output for the Git log, and
//! builds partial patches for staging individual hunks and lines.
//!
//! Commands run with `GIT_TERMINAL_PROMPT=0`, without a controlling terminal
//! (Unix: `setsid`) and without a console window (Windows), so nothing can
//! wait for input that the user cannot see.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

/// The `git` program: `APEX_GIT` if set, else `git` on `PATH`.
pub fn program() -> PathBuf {
    std::env::var_os("APEX_GIT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("git"))
}

/// Whether `git` can be run at all (checked once).
pub fn available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| base_command(&program(), None).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()))
}

fn base_command(program: &Path, workdir: Option<&Path>) -> Command {
    let mut c = Command::new(program);
    if let Some(w) = workdir {
        c.current_dir(w);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; it detaches the child from our
        // terminal (so ssh cannot prompt on it) and makes it a group leader
        // for cancellation.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Output of a background job, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    /// A line for the Git log (commands, their output).
    Log(String),
    /// The latest progress line (`Receiving objects: 45%`).
    Progress(String),
    /// The job ended: a short success message, or the error.
    Finished(Result<String, String>),
}

/// Handle to a running job; dropping it does not cancel.
pub struct GitJob {
    pub label: String,
    pub rx: Receiver<JobEvent>,
    pub started: Instant,
    cancel: Arc<AtomicBool>,
}

impl GitJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Passed to a job's body: runs commands in the repository.
pub struct JobCtx {
    pub workdir: PathBuf,
    tx: Sender<JobEvent>,
    cancel: Arc<AtomicBool>,
}

impl JobCtx {
    /// Run `git args…`, logging the command, stderr and stdout. Returns stdout.
    pub fn git(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
        self.run(args, stdin, true)
    }

    /// As [`git`](Self::git) but without logging stdout (diffs, plumbing).
    pub fn git_quiet(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
        self.run(args, stdin, false)
    }

    pub fn log(&self, line: impl Into<String>) {
        let _ = self.tx.send(JobEvent::Log(line.into()));
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>, log_stdout: bool) -> Result<String, String> {
        if self.cancelled() {
            return Err("cancelled".into());
        }
        self.log(format!("$ git {}", args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")));
        let mut cmd = base_command(&program(), Some(&self.workdir));
        cmd.args(["-c", "color.ui=false", "-c", "core.quotepath=false"]).args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().map_err(|e| format!("could not run git: {e}"))?;
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // Small payloads (messages, patches); written before reading.
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            });
        }
        let out_reader = child.stdout.take().map(|mut s| {
            std::thread::spawn(move || {
                let mut v = Vec::new();
                let _ = s.read_to_end(&mut v);
                v
            })
        });
        let err_reader = child.stderr.take().map(|s| {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                // Progress lines end in \r; everything else is a log line.
                let mut r = BufReader::new(s);
                let mut buf = Vec::new();
                let mut lines = Vec::new();
                loop {
                    buf.clear();
                    let n = read_until_cr_or_lf(&mut r, &mut buf);
                    if n == 0 {
                        break;
                    }
                    let ended_cr = buf.last() == Some(&b'\r');
                    let line = crate::tasks::sanitise_output(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']));
                    if line.is_empty() {
                        continue;
                    }
                    if ended_cr {
                        let _ = tx.send(JobEvent::Progress(line));
                    } else {
                        let _ = tx.send(JobEvent::Log(line.clone()));
                        lines.push(line);
                    }
                }
                lines
            })
        });
        let code = loop {
            if self.cancelled() {
                crate::tasks::kill_tree(&mut child);
                break None;
            }
            match child.try_wait() {
                Ok(Some(s)) => break s.code(),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let stdout = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        if log_stdout {
            for l in stdout.lines() {
                self.log(crate::tasks::sanitise_output(l));
            }
        }
        match code {
            Some(0) => Ok(stdout),
            None if self.cancelled() => Err("cancelled".into()),
            c => {
                let msg = stderr.iter().rev().find(|l| l.starts_with("fatal:") || l.starts_with("error:")).or(stderr.last()).cloned();
                Err(msg.unwrap_or_else(|| format!("git exited with code {}", c.map(|c| c.to_string()).unwrap_or_else(|| "none".into()))))
            }
        }
    }
}

fn read_until_cr_or_lf(r: &mut impl BufRead, buf: &mut Vec<u8>) -> usize {
    let mut n = 0;
    loop {
        let (done, used) = match r.fill_buf() {
            Ok([]) | Err(_) => return n,
            Ok(avail) => match avail.iter().position(|&b| b == b'\n' || b == b'\r') {
                Some(i) => {
                    buf.extend_from_slice(&avail[..=i]);
                    (true, i + 1)
                }
                None => {
                    buf.extend_from_slice(avail);
                    (false, avail.len())
                }
            },
        };
        r.consume(used);
        n += used;
        if done {
            return n;
        }
    }
}

fn shell_quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// Run `body` on a worker thread in `workdir`.
pub fn start_job(label: &str, workdir: PathBuf, body: impl FnOnce(&JobCtx) -> Result<String, String> + Send + 'static) -> GitJob {
    let (tx, rx) = unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = JobCtx { workdir, tx: tx.clone(), cancel: cancel.clone() };
    let spawned = std::thread::Builder::new().name("apex-git-job".into()).spawn(move || {
        let r = if available() { body(&ctx) } else { Err("the git command-line tool was not found (install Git or set APEX_GIT)".into()) };
        let _ = ctx.tx.send(JobEvent::Finished(r));
    });
    if let Err(e) = spawned {
        let _ = tx.send(JobEvent::Finished(Err(format!("could not start a thread: {e}"))));
    }
    GitJob { label: label.to_string(), rx, started: Instant::now(), cancel }
}

// ----- partial patches ---------------------------------------------------------

/// One file's section of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// `diff --git`, `index`, mode, `---`, `+++` lines.
    pub header: Vec<String>,
    pub hunks: Vec<PatchHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// Text after the second `@@` (function context).
    pub section: String,
    /// Body lines including their prefix (' ', '+', '-', '\\').
    pub lines: Vec<String>,
}

fn parse_range(s: &str) -> Option<(usize, usize)> {
    let mut it = s.splitn(2, ',');
    let start = it.next()?.parse().ok()?;
    let len = match it.next() {
        Some(l) => l.parse().ok()?,
        None => 1,
    };
    Some((start, len))
}

/// Parse `git diff` output. Binary or malformed sections yield no hunks.
pub fn parse_patch(text: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    for line in text.split('\n') {
        if line.starts_with("diff --git ") {
            files.push(FilePatch { header: vec![line.to_string()], hunks: Vec::new() });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if let Some(rest) = line.strip_prefix("@@ -") {
            let Some((ranges, section)) = rest.split_once(" @@") else { continue };
            let Some((old, new)) = ranges.split_once(" +") else { continue };
            let (Some((os, ol)), Some((ns, nl))) = (parse_range(old), parse_range(new)) else { continue };
            f.hunks.push(PatchHunk { old_start: os, old_len: ol, new_start: ns, new_len: nl, section: section.to_string(), lines: Vec::new() });
        } else if let Some(h) = f.hunks.last_mut() {
            if line.starts_with([' ', '+', '-', '\\']) {
                h.lines.push(line.to_string());
            }
        } else if !line.is_empty() {
            f.header.push(line.to_string());
        }
    }
    files
}

/// A body line's position: zero-based old/new line numbers. `-` lines have
/// `new` = the new-side line they precede; `+` lines likewise for `old`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePos {
    pub old: usize,
    pub new: usize,
}

impl PatchHunk {
    /// Positions of each body line (same indices as `lines`).
    pub fn positions(&self) -> Vec<LinePos> {
        let mut old = if self.old_len == 0 { self.old_start } else { self.old_start.saturating_sub(1) };
        let mut new = if self.new_len == 0 { self.new_start } else { self.new_start.saturating_sub(1) };
        self.lines
            .iter()
            .map(|l| {
                let p = LinePos { old, new };
                match l.as_bytes().first() {
                    Some(b' ') => {
                        old += 1;
                        new += 1;
                    }
                    Some(b'-') => old += 1,
                    Some(b'+') => new += 1,
                    _ => {}
                }
                p
            })
            .collect()
    }
}

/// Build a patch containing only the selected `+`/`-` lines of `file`.
///
/// `reverse` = the patch will be applied with `--reverse` (unstaging,
/// discarding): then an unselected `+` line stays as context and an
/// unselected `-` line is dropped; forwards it is the other way round.
/// Returns `None` when nothing is selected.
pub fn select_lines(file: &FilePatch, reverse: bool, mut keep: impl FnMut(usize, usize, LinePos, &str) -> bool) -> Option<String> {
    let mut out = file.header.join("\n");
    out.push('\n');
    let mut any = false;
    let mut delta: isize = 0;
    for (hi, h) in file.hunks.iter().enumerate() {
        let pos = h.positions();
        let mut body: Vec<String> = Vec::new();
        let mut changed = false;
        let mut dropped_prev = false;
        for (li, line) in h.lines.iter().enumerate() {
            let tag = line.as_bytes().first().copied().unwrap_or(b' ');
            let rest = &line[1.min(line.len())..];
            match tag {
                b'+' | b'-' => {
                    let selected = keep(hi, li, pos[li], line);
                    let to_context = (tag == b'-' && !reverse) || (tag == b'+' && reverse);
                    if selected {
                        body.push(line.clone());
                        changed = true;
                        dropped_prev = false;
                    } else if to_context {
                        body.push(format!(" {rest}"));
                        dropped_prev = false;
                    } else {
                        dropped_prev = true;
                    }
                }
                b'\\' => {
                    if !dropped_prev {
                        body.push(line.clone());
                    }
                }
                _ => {
                    body.push(line.clone());
                    dropped_prev = false;
                }
            }
        }
        if !changed {
            continue;
        }
        any = true;
        let count = |c: u8| body.iter().filter(|l| l.as_bytes().first() == Some(&c)).count();
        let (ctx, minus, plus) = (count(b' '), count(b'-'), count(b'+'));
        let (old_len, new_len) = (ctx + minus, ctx + plus);
        // The side the patch is applied to keeps its position; the other is
        // shifted by the net effect of the hunks emitted before it.
        let (anchor_start, anchor_len) = if reverse { (h.new_start, h.new_len) } else { (h.old_start, h.old_len) };
        let before = if anchor_len == 0 { anchor_start as isize } else { anchor_start as isize - 1 };
        let (a_len, b_len) = if reverse { (new_len, old_len) } else { (old_len, new_len) };
        let b_before = before + delta;
        let b_start = if b_len == 0 { b_before } else { b_before + 1 }.max(0) as usize;
        let a_start = if a_len == 0 { before } else { before + 1 }.max(0) as usize;
        delta += b_len as isize - a_len as isize;
        let (os, ns) = if reverse { (b_start, a_start) } else { (a_start, b_start) };
        out.push_str(&format!("@@ -{os},{old_len} +{ns},{new_len} @@{}\n", h.section));
        for l in body {
            out.push_str(&l);
            out.push('\n');
        }
    }
    any.then_some(out)
}

/// Select the changes touching zero-based new-side `lines` (for staging
/// from the editor). A `-` line counts when either neighbouring new-side
/// line is selected, so a deletion marker on the line above the gap selects
/// it. Empty ranges select their start line.
pub fn select_new_lines(file: &FilePatch, reverse: bool, lines: &[std::ops::Range<usize>]) -> Option<String> {
    let hit = |l: usize| lines.iter().any(|r| (r.start..r.end.max(r.start + 1)).contains(&l));
    select_lines(file, reverse, |_, _, p, l| match l.as_bytes().first() {
        Some(b'+') => hit(p.new),
        Some(b'-') => hit(p.new) || (p.new > 0 && hit(p.new - 1)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").current_dir(dir).args(args).output().expect("git runs");
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn repo_with(content: &str) -> Option<tempfile::TempDir> {
        if !available() {
            eprintln!("git not installed; skipping");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "Test"]);
        git(p, &["config", "user.email", "test@example.com"]);
        git(p, &["config", "commit.gpgsign", "false"]);
        std::fs::write(p.join("f.txt"), content).unwrap();
        git(p, &["add", "f.txt"]);
        git(p, &["commit", "-q", "-m", "init"]);
        Some(dir)
    }

    fn index_text(dir: &Path) -> String {
        git(dir, &["show", ":f.txt"])
    }

    fn apply(dir: &Path, patch: &str, args: &[&str]) {
        let ctx = JobCtx { workdir: dir.to_path_buf(), tx: unbounded().0, cancel: Arc::new(AtomicBool::new(false)) };
        let mut a = vec!["apply"];
        a.extend_from_slice(args);
        a.push("-");
        ctx.git(&a, Some(patch.as_bytes())).unwrap_or_else(|e| panic!("apply failed: {e}\n{patch}"));
    }

    #[test]
    fn parses_hunks_and_positions() {
        let d = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@ fn x\n a\n-b\n+B\n c\n@@ -10,0 +11,2 @@\n+y\n+z\n";
        let f = &parse_patch(d)[0];
        assert_eq!(f.header.len(), 4);
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].section, " fn x");
        let p = f.hunks[0].positions();
        assert_eq!(p[1], LinePos { old: 1, new: 1 });
        assert_eq!(p[2], LinePos { old: 2, new: 1 });
        assert_eq!(f.hunks[1].positions()[0], LinePos { old: 10, new: 10 });
        assert!(select_lines(f, false, |_, _, _, _| false).is_none());
    }

    #[test]
    fn stages_and_unstages_selected_lines_with_real_git() {
        let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let Some(dir) = repo_with(&base) else { return };
        let p = dir.path();
        // Three separate changes: modify 3, delete 15, add after 28.
        let mut lines: Vec<String> = base.lines().map(str::to_string).collect();
        lines[2] = "LINE 3".into();
        lines.remove(14);
        lines.insert(27, "new after 28".into());
        let worktree = lines.join("\n") + "\n";
        std::fs::write(p.join("f.txt"), &worktree).unwrap();

        // Stage only the second change (the deletion of line 15; new-side
        // gap at zero-based line 14).
        let diff = git(p, &["diff", "--", "f.txt"]);
        let f = &parse_patch(&diff)[0];
        let patch = select_new_lines(f, false, std::slice::from_ref(&(14..15))).expect("selection");
        apply(p, &patch, &["--cached"]);
        let expected: String = (1..=30).filter(|&i| i != 15).map(|i| format!("line {i}\n")).collect();
        assert_eq!(index_text(p), expected);

        // Stage the remaining changes, then unstage only the first.
        let diff = git(p, &["diff", "--", "f.txt"]);
        let f = &parse_patch(&diff)[0];
        let patch = select_lines(f, false, |_, _, _, _| true).unwrap();
        apply(p, &patch, &["--cached"]);
        assert_eq!(index_text(p), worktree);
        let cached = git(p, &["diff", "--cached", "--", "f.txt"]);
        let f = &parse_patch(&cached)[0];
        let patch = select_new_lines(f, true, std::slice::from_ref(&(2..3))).unwrap();
        apply(p, &patch, &["--cached", "--reverse"]);
        let mut want: Vec<String> = worktree.lines().map(str::to_string).collect();
        want[2] = "line 3".into();
        assert_eq!(index_text(p), want.join("\n") + "\n");
    }

    #[test]
    fn partial_selection_inside_one_hunk() {
        let Some(dir) = repo_with("a\nb\nc\n") else { return };
        let p = dir.path();
        std::fs::write(p.join("f.txt"), "a\nB\nC\nc\n").unwrap();
        // Hunk: -b +B +C. Stage only "+C" (new line 2): b stays as context.
        let diff = git(p, &["diff", "--", "f.txt"]);
        let f = &parse_patch(&diff)[0];
        let patch = select_new_lines(f, false, std::slice::from_ref(&(2..3))).unwrap();
        apply(p, &patch, &["--cached"]);
        assert_eq!(index_text(p), "a\nb\nC\nc\n");
    }

    #[test]
    fn job_runs_and_reports_errors() {
        let Some(dir) = repo_with("x\n") else { return };
        let job = start_job("status", dir.path().to_path_buf(), |c| {
            let out = c.git(&["status", "--porcelain"], None)?;
            c.git(&["definitely-not-a-command"], None).map(|_| out)
        });
        let mut events = Vec::new();
        while let Ok(e) = job.rx.recv_timeout(Duration::from_secs(20)) {
            let end = matches!(e, JobEvent::Finished(_));
            events.push(e);
            if end {
                break;
            }
        }
        assert!(matches!(events.first(), Some(JobEvent::Log(l)) if l == "$ git status --porcelain"));
        assert!(matches!(events.last(), Some(JobEvent::Finished(Err(e))) if e.contains("not a git command")));
    }
}
