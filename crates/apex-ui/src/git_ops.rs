//! Git commands that write to the repository or reach a remote: a queue of
//! background `git` jobs (one at a time), the Git output log, staging, and
//! confirmation of destructive operations. All of them need a trusted
//! workspace, because `git` runs the repository's hooks and configured
//! programs.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use apex_project::git::FileStatus;
use apex_project::git_cli::{self, GitJob, JobCtx, JobEvent};
use egui::Ui;

use crate::app::{ApexApp, Dialog};
use crate::command::Command;

type JobBody = Box<dyn FnOnce(&JobCtx) -> Result<String, String> + Send>;

const MAX_LOG: usize = 5000;

#[derive(Default)]
pub struct GitOps {
    pub(crate) running: Option<GitJob>,
    queued: VecDeque<(String, PathBuf, JobBody)>,
    pub(crate) log: VecDeque<String>,
    pub(crate) progress: Option<String>,
    scroll_log: bool,
    /// Set while re-running a command the user has just confirmed.
    pub(crate) confirmed: bool,
    last_auto_fetch: Option<std::time::Instant>,
}

/// `origin` if present, else the first configured remote.
fn first_remote(workdir: &Path) -> Option<String> {
    let repo = git2::Repository::open(workdir).ok()?;
    let remotes = repo.remotes().ok()?;
    let names: Vec<String> = remotes.iter().flatten().map(str::to_string).collect();
    names.iter().find(|n| *n == "origin").or(names.first()).cloned()
}

/// Advice for errors that mean git could not authenticate.
fn auth_hint(e: &str) -> Option<&'static str> {
    let l = e.to_lowercase();
    if l.contains("permission denied (publickey") || l.contains("host key verification failed") {
        Some("check that your SSH key is loaded in ssh-agent and the host is known")
    } else if l.contains("authentication failed")
        || l.contains("could not read username")
        || l.contains("could not read password")
        || l.contains("terminal prompts disabled")
    {
        Some("set up a credential helper, or use a personal access token as the password")
    } else {
        None
    }
}

/// `path` relative to `workdir` with `/` separators, as git expects.
pub(crate) fn rel_path(workdir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(workdir).ok()?;
    let s: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(if s.is_empty() { ".".into() } else { s.join("/") })
}

impl ApexApp {
    /// Queue a job; it starts when the previous one has finished.
    pub(crate) fn git_run(&mut self, label: &str, workdir: PathBuf, body: impl FnOnce(&JobCtx) -> Result<String, String> + Send + 'static) {
        self.git.ops.queued.push_back((label.to_string(), workdir, Box::new(body)));
        self.git_start_next();
    }

    fn git_start_next(&mut self) {
        if self.git.ops.running.is_some() {
            return;
        }
        let Some((label, workdir, body)) = self.git.ops.queued.pop_front() else { return };
        self.git.ops.progress = None;
        self.git.ops.running = Some(git_cli::start_job(&label, workdir, body));
    }

    /// Drain job output; returns true while anything is happening.
    pub(crate) fn git_ops_poll(&mut self) -> bool {
        let Some(job) = &self.git.ops.running else { return false };
        let mut finished = None;
        while let Ok(e) = job.rx.try_recv() {
            match e {
                JobEvent::Log(l) => {
                    if self.git.ops.log.len() >= MAX_LOG {
                        self.git.ops.log.pop_front();
                    }
                    self.git.ops.log.push_back(l);
                    self.git.ops.scroll_log = true;
                }
                JobEvent::Progress(p) => self.git.ops.progress = Some(p),
                JobEvent::Prompt(prompt, reply) => {
                    // A dialog already open (rare) cancels the prompt rather than losing it.
                    if self.dialog.is_some() {
                        let _ = reply.send(None);
                    } else {
                        self.dialog = Some(Dialog::Askpass { prompt, text: String::new(), reply });
                    }
                }
                JobEvent::Finished(r) => {
                    finished = Some(r);
                    break;
                }
            }
        }
        if let Some(r) = finished {
            let label = self.git.ops.running.take().map(|j| j.label).unwrap_or_default();
            self.git.ops.progress = None;
            match r {
                Ok(msg) if msg.is_empty() => {}
                Ok(msg) => self.set_status(msg, false),
                Err(e) => {
                    self.git.ops.log.push_back(format!("[{label} failed: {e}]"));
                    let hint = auth_hint(&e).map(|h| format!(" ({h})")).unwrap_or_default();
                    self.set_status(format!("{label} failed: {e}{hint}"), true);
                }
            }
            self.git_mark_dirty();
            self.git_start_next();
        }
        true
    }

    /// Repository and (optional) file a command applies to: the path in
    /// `arg`, else the active document, else the first folder.
    pub(crate) fn git_target(&mut self, arg: Option<&str>) -> Option<(PathBuf, Option<PathBuf>)> {
        let file = match arg {
            Some(a) if !a.is_empty() => Some(PathBuf::from(a)),
            _ => self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()),
        };
        if let Some(f) = &file
            && let Some(w) = apex_project::git::workdir_of(f)
        {
            return Some((w, file));
        }
        let w = self.roots.iter().find_map(|r| apex_project::git::workdir_of(r));
        if w.is_none() {
            self.set_status("Not in a Git repository".into(), false);
        }
        w.map(|w| (w, None))
    }

    /// Ask to trust `workdir` before running `git` there; re-runs the command.
    fn git_trusted(&mut self, workdir: &Path, cmd: Command, arg: &Option<String>) -> bool {
        if self.trust.is_trusted(workdir) {
            return true;
        }
        self.dialog = Some(Dialog::Trust { root: workdir.to_path_buf(), then: Some((cmd, arg.clone())) });
        false
    }

    /// Ask before a destructive command; true once the user has agreed.
    pub(crate) fn git_confirm(&mut self, title: &str, body: String, button: &str, cmd: Command, arg: &Option<String>) -> bool {
        if std::mem::take(&mut self.git.ops.confirmed) {
            return true;
        }
        self.dialog = Some(Dialog::Confirm { title: title.into(), body, button: button.into(), then: (cmd, arg.clone()) });
        false
    }

    pub(crate) fn git_entry_for(&self, workdir: &Path, file: &Path) -> Option<apex_project::git::StatusEntry> {
        self.git.repos.get(workdir)?.status.entries.iter().find(|e| e.path == file).cloned()
    }

    pub(crate) fn git_ops_command(&mut self, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::GitShowOutput => {
                use crate::tasks_ui::PanelTab;
                self.panel_open = !(self.panel_open && self.panel_tab == PanelTab::Git);
                self.panel_tab = PanelTab::Git;
                return true;
            }
            Command::GitCancel => {
                match &self.git.ops.running {
                    Some(j) => j.cancel(),
                    None => self.set_status("No Git command is running".into(), false),
                }
                self.git.ops.queued.clear();
                return true;
            }
            Command::GitFetch | Command::GitPull | Command::GitPush | Command::GitForcePush | Command::GitSync => {
                self.git_remote_command(cmd, arg);
                return true;
            }
            Command::GitStageFile
            | Command::GitUnstageFile
            | Command::GitStageHunk
            | Command::GitStageLines
            | Command::GitStageAll
            | Command::GitUnstageAll
            | Command::GitDiscardFile => {}
            _ => return false,
        }
        let Some((workdir, file)) = self.git_target(arg.as_deref()) else { return true };
        let file_cmd = !matches!(cmd, Command::GitStageAll | Command::GitUnstageAll);
        let rel = file.as_deref().and_then(|f| rel_path(&workdir, f));
        if file_cmd && rel.is_none() {
            self.set_status("Open a file in the repository first".into(), false);
            return true;
        }
        if !self.git_trusted(&workdir, cmd, &arg) {
            return true;
        }
        let rel = rel.unwrap_or_default();
        let name = file.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match cmd {
            Command::GitStageFile => {
                self.git_run("Stage", workdir, move |c| c.git(&["add", "-A", "--", &rel], None).map(|_| format!("Staged {name}")));
            }
            Command::GitUnstageFile => {
                self.git_run("Unstage", workdir, move |c| {
                    // `reset` needs a HEAD; in a repository without commits, drop from the index.
                    c.git(&["reset", "-q", "--", &rel], None)
                        .or_else(|_| c.git(&["rm", "-r", "-q", "--cached", "--", &rel], None))
                        .map(|_| format!("Unstaged {name}"))
                });
            }
            Command::GitStageAll => self.git_run("Stage all", workdir, |c| c.git(&["add", "-A"], None).map(|_| "Staged all changes".into())),
            Command::GitUnstageAll => {
                self.git_run("Unstage all", workdir, |c| {
                    c.git(&["reset", "-q"], None).or_else(|_| c.git(&["rm", "-r", "-q", "--cached", "."], None)).map(|_| "Unstaged all changes".into())
                });
            }
            Command::GitDiscardFile => {
                let Some(path) = file else { return true };
                let entry = self.git_entry_for(&workdir, &path);
                let untracked = entry.as_ref().is_some_and(|e| e.unstaged == Some(FileStatus::Untracked) && e.staged.is_none());
                if entry.as_ref().is_none_or(|e| e.unstaged.is_none()) {
                    self.set_status(format!("{name} has no unstaged changes"), false);
                    return true;
                }
                if untracked {
                    // Same confirmation and deletion as the explorer.
                    self.dialog = Some(Dialog::ConfirmDelete(path));
                    return true;
                }
                let body = format!("Discard the unstaged changes to {}? The file is restored from the index; this cannot be undone.", self.display_path(&path));
                if !self.git_confirm("Discard changes?", body, "Discard", cmd, &arg) {
                    return true;
                }
                self.git_run("Discard", workdir, move |c| c.git(&["checkout", "-q", "--", &rel], None).map(|_| format!("Discarded changes to {name}")));
            }
            Command::GitStageHunk | Command::GitStageLines => {
                let Some((v, d)) = self.active_ids() else { return true };
                if self.docs.get(&d).is_some_and(|doc| doc.is_dirty()) {
                    self.set_status("Save the file before staging part of it".into(), false);
                    return true;
                }
                if self.docs.get(&d).and_then(|doc| doc.path.as_ref()) != file.as_ref() {
                    self.set_status("Staging changes works on the active file".into(), false);
                    return true;
                }
                let ranges: Vec<std::ops::Range<usize>> = if cmd == Command::GitStageHunk {
                    let saved = self.git.compare_head;
                    self.git.compare_head = false;
                    let h = self.git_hunk_at_caret(v, d);
                    self.git.compare_head = saved;
                    match h {
                        Some(h) => vec![h.new_lines],
                        None => {
                            self.set_status("No unstaged change at the caret".into(), false);
                            return true;
                        }
                    }
                } else {
                    let doc = &self.docs[&d];
                    let rope = doc.rope();
                    self.views[&v]
                        .sels
                        .iter()
                        .map(|s| {
                            let r = s.range();
                            let a = rope.char_to_line(r.start.min(rope.len_chars()));
                            let mut b = rope.char_to_line(r.end.min(rope.len_chars()));
                            // A selection ending at the start of a line does not include it.
                            if b > a && rope.line_to_char(b) == r.end {
                                b -= 1;
                            }
                            a..b + 1
                        })
                        .collect()
                };
                let untracked =
                    self.git_entry_for(&workdir, file.as_deref().unwrap_or(Path::new(""))).is_some_and(|e| e.unstaged == Some(FileStatus::Untracked));
                let what = if cmd == Command::GitStageHunk { "change" } else { "selected lines" };
                self.git_run("Stage", workdir, move |c| {
                    if untracked {
                        c.git(&["add", "-N", "--", &rel], None)?;
                    }
                    let diff = c.git_quiet(&["diff", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/", "-U3", "--", &rel], None)?;
                    let files = git_cli::parse_patch(&diff);
                    let f = files.first().ok_or_else(|| format!("{name} has no unstaged changes"))?;
                    let patch = git_cli::select_new_lines(f, false, &ranges).ok_or_else(|| format!("no unstaged changes in the {what}"))?;
                    c.git_quiet(&["apply", "--cached", "--whitespace=nowarn", "-"], Some(patch.as_bytes()))?;
                    Ok(format!("Staged {what} in {name}"))
                });
            }
            _ => {}
        }
        true
    }

    /// Fetch, pull, push, force push and sync for the panel's repository.
    fn git_remote_command(&mut self, cmd: Command, arg: Option<String>) {
        let Some(workdir) = self.git_panel_repo().or_else(|| self.git_target(None).map(|t| t.0)) else { return };
        if !self.git_trusted(&workdir, cmd, &arg) {
            return;
        }
        let status = self.git.repos.get(&workdir).map(|e| e.status.clone()).unwrap_or_default();
        let branch = status.branch.clone();
        let pull_args: Vec<&'static str> = match self.settings.git_pull_mode.as_str() {
            "rebase" => vec!["pull", "--rebase", "--progress"],
            "merge" => vec!["pull", "--no-rebase", "--no-edit", "--progress"],
            _ => vec!["pull", "--ff-only", "--progress"],
        };
        match cmd {
            Command::GitFetch => self.git_run("Fetch", workdir, |c| c.git(&["fetch", "--all", "--prune", "--progress"], None).map(|_| "Fetched".into())),
            Command::GitPull => self.git_run("Pull", workdir, move |c| c.git(&pull_args, None).map(|_| "Pulled".into())),
            Command::GitPush | Command::GitSync => {
                if status.detached || status.unborn {
                    self.set_status("Check out a branch with commits before pushing".into(), false);
                    return;
                }
                let sync = cmd == Command::GitSync;
                if status.upstream.is_none() {
                    // First push of this branch: publish it, after asking.
                    let remote = first_remote(&workdir);
                    let Some(remote) = remote else {
                        self.set_status("This repository has no remote to push to".into(), false);
                        return;
                    };
                    let body = format!("{branch} has no upstream branch. Push it to {remote}/{branch} and track it?");
                    if !self.git_confirm("Publish branch?", body, "Publish", cmd, &arg) {
                        return;
                    }
                    self.git_run("Publish", workdir, move |c| {
                        c.git(&["push", "--set-upstream", "--progress", &remote, &branch], None).map(|_| format!("Published {branch} to {remote}"))
                    });
                    return;
                }
                self.git_run(if sync { "Sync" } else { "Push" }, workdir, move |c| {
                    if sync {
                        c.git(&pull_args, None)?;
                    }
                    c.git(&["push", "--progress"], None).map(|_| if sync { "Synced".into() } else { "Pushed".into() })
                });
            }
            Command::GitForcePush => {
                if status.upstream.is_none() {
                    self.set_status("This branch has no upstream; use Push to publish it".into(), false);
                    return;
                }
                let up = status.upstream.clone().unwrap_or_default();
                let body = format!(
                    "Replace {up} with your local {branch}? Commits on {up} that you do not have are lost. --force-with-lease refuses if {up} changed since you last fetched."
                );
                if !self.git_confirm("Force push?", body, "Force Push", cmd, &arg) {
                    return;
                }
                self.git_run("Force push", workdir, |c| c.git(&["push", "--force-with-lease", "--progress"], None).map(|_| "Force-pushed".into()));
            }
            _ => {}
        }
    }

    /// Background fetch every `git_auto_fetch_minutes` (trusted repositories only).
    pub(crate) fn git_auto_fetch(&mut self) {
        let minutes = self.settings.git_auto_fetch_minutes;
        if minutes == 0 || self.git.ops.running.is_some() || !self.git.ops.queued.is_empty() {
            return;
        }
        if self.git.ops.last_auto_fetch.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(minutes * 60)) {
            return;
        }
        self.git.ops.last_auto_fetch = Some(std::time::Instant::now());
        let Some(workdir) = self.git_panel_repo() else { return };
        if !self.trust.is_trusted(&workdir) || first_remote(&workdir).is_none() {
            return;
        }
        self.git_run("Auto-fetch", workdir, |c| c.git(&["fetch", "--all", "--prune", "--quiet"], None).map(|_| String::new()));
    }

    /// Bottom panel "GIT" tab: the command log.
    pub(crate) fn show_git_output_panel(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        ui.horizontal(|ui| {
            if let Some(j) = &self.git.ops.running {
                ui.spinner();
                ui.label(egui::RichText::new(&j.label).strong());
                if let Some(p) = &self.git.ops.progress {
                    ui.label(egui::RichText::new(p).color(theme.ui.text_muted));
                }
                if ui.button("Cancel").clicked() {
                    self.queue.push_back((Command::GitCancel, None));
                }
            } else {
                ui.label(egui::RichText::new("Commands run by Apex in your repositories").color(theme.ui.text_muted));
            }
            if ui.button("Clear").clicked() {
                self.git.ops.log.clear();
            }
        });
        ui.separator();
        let mono = egui::FontId::monospace(ui_font);
        let row_h = ui_font + 5.0;
        let stick = std::mem::take(&mut self.git.ops.scroll_log);
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("apex-git-log").stick_to_bottom(stick).show_rows(
            ui,
            row_h,
            self.git.ops.log.len(),
            |ui, range| {
                for i in range {
                    let line = &self.git.ops.log[i];
                    let colour = if line.starts_with("$ ") {
                        theme.ui.accent
                    } else if line.starts_with("fatal:") || line.starts_with("error:") || line.ends_with("failed") || line.contains(" failed: ") {
                        theme.ui.error
                    } else if line.starts_with("warning:") || line.starts_with("hint:") {
                        theme.ui.warning
                    } else {
                        theme.ui.text
                    };
                    ui.label(egui::RichText::new(line).font(mono.clone()).color(colour));
                }
            },
        );
    }

    /// Status-bar text while a Git command runs.
    pub(crate) fn git_ops_status(&self) -> Option<String> {
        let j = self.git.ops.running.as_ref()?;
        Some(match &self.git.ops.progress {
            Some(p) => format!("{}: {p}", j.label),
            None => format!("{}…", j.label),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_hints() {
        assert!(auth_hint("git@github.com: Permission denied (publickey).").is_some());
        assert!(auth_hint("fatal: could not read Username for 'https://github.com': terminal prompts disabled").is_some());
        assert!(auth_hint("fatal: not a git repository").is_none());
    }

    #[test]
    fn relative_paths_use_slashes() {
        let w = Path::new("/repo");
        assert_eq!(rel_path(w, Path::new("/repo/src/main.rs")).as_deref(), Some("src/main.rs"));
        assert_eq!(rel_path(w, Path::new("/repo")).as_deref(), Some("."));
        assert_eq!(rel_path(w, Path::new("/elsewhere/x")), None);
    }
}
