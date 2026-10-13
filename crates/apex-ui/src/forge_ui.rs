//! GitHub and other code hosts: links to the remote (no network from Apex,
//! the browser opens them) and, through the GitHub CLI (`gh`), pull
//! requests: create, status and checks of the current branch's PR, list,
//! check out and diff. `gh` owns authentication; Apex stores no tokens.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use apex_project::forge::{self, CheckState, PrSummary, WebRepo};
use apex_project::git_cli;

use crate::app::{ApexApp, Dialog, open_url};
use crate::command::Command;
use crate::git_ops::{first_remote, rel_path};
use crate::palette::{PaletteItem, PaletteMode, Payload};

/// Results sent from background `gh` jobs back to the UI thread.
enum Incoming {
    /// The PR for (workdir, branch), or None when the branch has none.
    Status(PathBuf, String, Option<Box<PrSummary>>),
    /// Open pull requests, shown in the palette.
    List(Vec<PrSummary>),
    /// Check runs of the current branch's PR, shown in the palette.
    Checks(Vec<forge::CheckRun>),
    /// PR number and its unified diff text, opened as a scratch document.
    Diff(u64, String),
}

/// Pull request state and the channel that background `gh` jobs report on.
#[derive(Default)]
pub struct ForgeUi {
    /// The current branch's pull request: (workdir, branch, PR).
    pub(crate) pr: Option<(PathBuf, String, PrSummary)>,
    /// Receiving end, drained each frame by `forge_poll`; created lazily.
    rx: Option<Receiver<Incoming>>,
    /// Sending end, cloned into each job closure.
    tx: Option<std::sync::mpsc::Sender<Incoming>>,
}

impl ForgeUi {
    /// A sender for job results, creating the channel on first use.
    fn sender(&mut self) -> std::sync::mpsc::Sender<Incoming> {
        if self.tx.is_none() {
            let (tx, rx) = channel();
            self.tx = Some(tx);
            self.rx = Some(rx);
        }
        self.tx.clone().expect("set above")
    }
}

/// The web address of a repository's main remote.
fn web_repo_for(workdir: &Path) -> Option<WebRepo> {
    let remote = first_remote(workdir)?;
    // The URL as configured (before `insteadOf` rewriting): the host the user named.
    let cfg = git2::Repository::open(workdir).ok()?.config().ok()?;
    let url = cfg.get_string(&format!("remote.{remote}.url")).ok()?;
    forge::web_repo(&url)
}

/// The full hex id of HEAD's commit, or `None` if the repository cannot be
/// opened or HEAD is unborn.
fn head_id(workdir: &Path) -> Option<String> {
    let repo = git2::Repository::open(workdir).ok()?;
    repo.head().ok()?.target().map(|o| o.to_string())
}

/// `origin/HEAD`'s branch name, else `main`/`master` if present.
/// Uses the first remote (preferring `origin`); falls back to `main`.
fn default_branch(workdir: &Path) -> String {
    let Ok(repo) = git2::Repository::open(workdir) else { return "main".into() };
    let remote = first_remote(workdir).unwrap_or_else(|| "origin".into());
    if let Ok(r) = repo.find_reference(&format!("refs/remotes/{remote}/HEAD"))
        && let Some(t) = r.symbolic_target()
        && let Some(b) = t.rsplit('/').next()
    {
        return b.to_string();
    }
    for b in ["main", "master"] {
        if repo.find_reference(&format!("refs/remotes/{remote}/{b}")).is_ok() || repo.find_branch(b, git2::BranchType::Local).is_ok() {
            return b.into();
        }
    }
    "main".into()
}

/// (subject, body) of commits on HEAD not on `base`, oldest first.
/// Capped at 200 commits; empty if the repository or HEAD cannot be read.
fn commits_since(workdir: &Path, base: &str) -> Vec<(String, String)> {
    let Ok(repo) = git2::Repository::open(workdir) else { return Vec::new() };
    let remote = first_remote(workdir).unwrap_or_else(|| "origin".into());
    let Ok(mut walk) = repo.revwalk() else { return Vec::new() };
    if walk.push_head().is_err() {
        return Vec::new();
    }
    // Prefer the remote-tracking base, as that is what the PR merges into.
    // If neither ref exists nothing is hidden and all of HEAD's history counts.
    for r in [format!("refs/remotes/{remote}/{base}"), format!("refs/heads/{base}")] {
        if let Ok(oid) = repo.refname_to_id(&r) {
            let _ = walk.hide(oid);
            break;
        }
    }
    let _ = walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE);
    walk.take(200)
        .flatten()
        .filter_map(|oid| repo.find_commit(oid).ok())
        .map(|c| (c.summary().unwrap_or("").to_string(), c.body().unwrap_or("").to_string()))
        .collect()
}

impl ApexApp {
    /// Drain results from background `gh` jobs and act on them: update the
    /// branch's PR, show PR or check lists in the palette, or open a diff.
    /// Called once per frame.
    pub(crate) fn forge_poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.git.forge.rx else { return };
        // Collect first: handling a message needs `&mut self`, which the
        // borrow of `rx` would otherwise block.
        let mut got = Vec::new();
        while let Ok(m) = rx.try_recv() {
            got.push(m);
        }
        for m in got {
            match m {
                Incoming::Status(w, b, pr) => {
                    self.git.forge.pr = pr.map(|p| (w, b, *p));
                    if self.git.forge.pr.is_none() {
                        self.set_status("This branch has no pull request".into(), false);
                    }
                }
                Incoming::List(list) => {
                    let items: Vec<PaletteItem> = list
                        .iter()
                        .map(|p| PaletteItem {
                            label: format!("#{}  {}", p.number, p.title),
                            detail: format!(
                                "{}{}  {}",
                                p.head_ref_name,
                                if p.is_draft { " (draft)" } else { "" },
                                p.author.as_ref().map(|a| a.login.as_str()).unwrap_or("")
                            ),
                            shortcut: String::new(),
                            // Number and URL joined by U+001F (unit separator), split again in `GhPrActions`.
                            payload: Payload::Command(Command::GhPrActions, Some(format!("{}\u{1f}{}", p.number, p.url))),
                            boost: 0,
                        })
                        .collect();
                    if items.is_empty() {
                        self.set_status("No open pull requests".into(), false);
                    } else {
                        self.palette.show_with(PaletteMode::Prompt("pull_requests".into()), items, "Pull requests", "");
                    }
                }
                Incoming::Checks(runs) => {
                    let items: Vec<PaletteItem> = runs
                        .iter()
                        .map(|r| PaletteItem {
                            label: r.label().to_string(),
                            detail: match r.outcome() {
                                CheckState::Passed => "passed",
                                CheckState::Failed => "failed",
                                CheckState::Pending => "running",
                                CheckState::Skipped => "skipped",
                            }
                            .into(),
                            shortcut: String::new(),
                            payload: Payload::Command(Command::GhOpenUrl, Some(r.link().to_string())),
                            boost: 0,
                        })
                        .collect();
                    if items.is_empty() {
                        self.set_status("No checks on this pull request".into(), false);
                    } else {
                        self.palette.show_with(PaletteMode::Prompt("checks".into()), items, "Checks (Enter opens the details)", "");
                    }
                }
                Incoming::Diff(n, text) => self.open_scratch_doc(&format!("pull request #{n}.diff"), &text, Some("diff"), true, ctx),
            }
        }
    }

    /// Status-bar label of the current branch's PR.
    /// `None` once the repository has moved to a different branch, so a stale PR is not shown.
    pub(crate) fn forge_status_label(&mut self) -> Option<String> {
        let (w, b, pr) = self.git.forge.pr.as_ref()?;
        let st = self.git.repos.get(w)?;
        (st.status.branch == *b).then(|| pr.status_label())
    }

    /// Whether a `gh` command may run in `workdir`. Reports a missing `gh` in
    /// the status bar; for an untrusted folder, opens the Trust dialog, which
    /// re-runs `cmd` with `arg` once trust is granted. Gated because `gh` runs
    /// `git`, which honours the repository's own config.
    fn gh_ready(&mut self, workdir: &Path, cmd: Command, arg: &Option<String>) -> bool {
        if !git_cli::gh_available() {
            self.set_status("This needs the GitHub CLI: install gh and run `gh auth login` (or set APEX_GH)".into(), true);
            return false;
        }
        if !self.trust.is_trusted(workdir) {
            self.dialog = Some(Dialog::Trust { root: workdir.to_path_buf(), then: Some((cmd, arg.clone())) });
            return false;
        }
        true
    }

    /// Ask gh for the current branch's PR (after push, create, or on request).
    /// Queues a `gh pr view` job; the result arrives as `Incoming::Status`.
    pub(crate) fn forge_refresh_pr(&mut self, workdir: PathBuf) {
        // Silent no-op (no Trust dialog) as this also runs automatically after pushes.
        if !git_cli::gh_available() || !self.trust.is_trusted(&workdir) || web_repo_for(&workdir).is_none() {
            return;
        }
        let branch = self.git.repos.get(&workdir).map(|e| e.status.branch.clone()).unwrap_or_default();
        let tx = self.git.forge.sender();
        self.git_run("Pull request status", workdir.clone(), move |c| match c.gh(&["pr", "view", "--json", forge::PR_FIELDS], None) {
            Ok(out) => {
                let pr = forge::parse_pr(&out)?;
                let label = pr.status_label();
                let _ = tx.send(Incoming::Status(workdir, branch, Some(Box::new(pr))));
                Ok(label)
            }
            // gh reports "no PR" as an error; treat it as an empty result, not a failure.
            Err(e) if e.contains("no pull requests found") => {
                let _ = tx.send(Incoming::Status(workdir, branch, None));
                Ok(String::new())
            }
            Err(e) => Err(e),
        });
    }

    /// Run a code-host command (remote links and `gh` pull request commands).
    /// `arg` is a file path, commit id, PR number or URL depending on `cmd`.
    /// Returns `false` if `cmd` is not a forge command, so the caller can try others.
    /// `gh` and `git push` run as queued background jobs; links open in the browser.
    pub(crate) fn forge_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::GitOpenOnRemote
            | Command::GitCopyPermalink
            | Command::GitOpenCommitOnRemote
            | Command::GitOpenBranchOnRemote
            | Command::GitCompareOnRemote
            | Command::GhCreatePr
            | Command::GhPrStatus
            | Command::GhListPrs
            | Command::GhPrActions
            | Command::GhCheckoutPr
            | Command::GhPrDiff
            | Command::GhChecks
            | Command::GhOpenPr
            | Command::GhOpenUrl
            | Command::GhSubmitPr => {}
            _ => return false,
        }
        if cmd == Command::GhOpenUrl {
            if let Some(u) = arg
                && !open_url(&u)
            {
                self.set_status(format!("Could not open {u}"), true);
            }
            return true;
        }
        // Only path-like arguments name the target file; PR numbers and commit ids
        // fall back to the active document. ':' admits Windows drive paths.
        let Some((workdir, file)) = self.git_target(arg.as_deref().filter(|a| a.starts_with('/') || a.contains(':'))) else { return true };
        let status = self.git.repos.get(&workdir).map(|e| e.status.clone()).unwrap_or_default();
        let web = web_repo_for(&workdir);
        let need_web = |app: &mut ApexApp| -> Option<WebRepo> {
            if web.is_none() {
                app.set_status("This repository has no remote on a recognised code host".into(), false);
            }
            web.clone()
        };
        match cmd {
            Command::GitOpenOnRemote | Command::GitCopyPermalink => {
                let Some(web) = need_web(self) else { return true };
                let Some(path) = file else {
                    self.set_status("Open a file first".into(), false);
                    return true;
                };
                let Some(rel) = rel_path(&workdir, &path) else { return true };
                let Some(rev) = head_id(&workdir) else {
                    self.set_status("This repository has no commits yet".into(), false);
                    return true;
                };
                let lines = self.active_ids().map(|(v, d)| {
                    let rope = self.docs[&d].rope();
                    let s = self.views[&v].sels.primary().range();
                    let a = rope.char_to_line(s.start.min(rope.len_chars()));
                    let mut b = rope.char_to_line(s.end.min(rope.len_chars()));
                    // A selection ending at the start of a line does not include that line.
                    if b > a && rope.line_to_char(b) == s.end {
                        b -= 1;
                    }
                    (a + 1, b + 1)
                });
                let url = web.file_url(&rev, &rel, lines);
                let warn = if status.ahead > 0 || status.upstream.is_none() {
                    " (HEAD is not pushed yet, so the link may not work until you push)"
                } else if self.git_entry_for(&workdir, &path).is_some() {
                    " (the file has uncommitted changes; the link shows the committed version)"
                } else {
                    ""
                };
                if cmd == Command::GitCopyPermalink {
                    ctx.copy_text(url);
                    self.set_status(format!("Permalink copied{warn}"), false);
                } else {
                    open_url(&url);
                    if !warn.is_empty() {
                        self.set_status(format!("Opened{warn}"), false);
                    }
                }
            }
            Command::GitOpenCommitOnRemote => {
                let Some(web) = need_web(self) else { return true };
                // Accept only something shaped like a commit id, so arbitrary text never reaches the URL.
                let id = arg.filter(|a| a.len() >= 7 && a.chars().all(|c| c.is_ascii_hexdigit())).or_else(|| head_id(&workdir));
                if let Some(id) = id {
                    open_url(&web.commit_url(&id));
                }
            }
            Command::GitOpenBranchOnRemote | Command::GitCompareOnRemote => {
                let Some(web) = need_web(self) else { return true };
                if status.detached || status.branch.is_empty() {
                    self.set_status("Check out a branch first".into(), false);
                    return true;
                }
                open_url(&if cmd == Command::GitCompareOnRemote { web.compare_url(&status.branch) } else { web.branch_url(&status.branch) });
            }
            Command::GhCreatePr => {
                if need_web(self).is_none() || !self.gh_ready(&workdir, cmd, &arg) {
                    return true;
                }
                if status.detached || status.unborn {
                    self.set_status("Check out a branch with commits first".into(), false);
                    return true;
                }
                let base = default_branch(&workdir);
                if status.branch == base {
                    self.set_status(format!("You are on {base}; create a branch for the pull request first"), false);
                    return true;
                }
                let commits = commits_since(&workdir, &base);
                if commits.is_empty() {
                    self.set_status(format!("{} has no commits that are not on {base}", status.branch), false);
                    return true;
                }
                let (title, body) = forge::pr_template(&status.branch, &commits);
                self.dialog = Some(Dialog::CreatePr { workdir, branch: status.branch, base, title, body, draft: false });
            }
            Command::GhPrStatus => {
                if self.gh_ready(&workdir, cmd, &arg) {
                    self.forge_refresh_pr(workdir);
                }
            }
            Command::GhOpenPr => match self.git.forge.pr.as_ref().filter(|(w, b, _)| *w == workdir && *b == status.branch) {
                Some((_, _, pr)) => {
                    open_url(&pr.url.clone());
                }
                None => self.set_status("No pull request known for this branch; use Refresh Pull Request Status".into(), false),
            },
            Command::GhListPrs => {
                if !self.gh_ready(&workdir, cmd, &arg) {
                    return true;
                }
                let tx = self.git.forge.sender();
                self.git_run("List pull requests", workdir, move |c| {
                    let out = c.gh(&["pr", "list", "--limit", "100", "--json", forge::PR_LIST_FIELDS], None)?;
                    let _ = tx.send(Incoming::List(forge::parse_pr_list(&out)?));
                    Ok(String::new())
                });
            }
            Command::GhPrActions => {
                let Some((n, url)) = arg.as_deref().and_then(|a| a.split_once('\u{1f}')) else { return true };
                let item = |label: &str, c: Command, a: &str| PaletteItem {
                    label: label.into(),
                    detail: String::new(),
                    shortcut: String::new(),
                    payload: Payload::Command(c, Some(a.to_string())),
                    boost: 0,
                };
                let items = vec![
                    item("Check out", Command::GhCheckoutPr, n),
                    item("Show diff", Command::GhPrDiff, n),
                    item("Open in browser", Command::GhOpenUrl, url),
                ];
                self.palette.show_with(PaletteMode::Prompt("pr_actions".into()), items, &format!("Pull request #{n}"), "");
            }
            Command::GhCheckoutPr => {
                // Digits only, so the argument cannot be read by gh as an option or branch name.
                let Some(n) = arg.filter(|a| a.chars().all(|c| c.is_ascii_digit()) && !a.is_empty()) else { return true };
                if !self.gh_ready(&workdir, cmd, &Some(n.clone())) {
                    return true;
                }
                self.git_run("Check out pull request", workdir, move |c| c.gh(&["pr", "checkout", &n], None).map(|_| format!("Checked out pull request #{n}")));
            }
            Command::GhPrDiff => {
                let Some(n) = arg.filter(|a| a.chars().all(|c| c.is_ascii_digit()) && !a.is_empty()) else { return true };
                if !self.gh_ready(&workdir, cmd, &Some(n.clone())) {
                    return true;
                }
                let tx = self.git.forge.sender();
                self.git_run("Pull request diff", workdir, move |c| {
                    let out = c.gh(&["pr", "diff", &n, "--color", "never"], None)?;
                    let _ = tx.send(Incoming::Diff(n.parse().unwrap_or(0), out));
                    Ok(String::new())
                });
            }
            Command::GhChecks => {
                if !self.gh_ready(&workdir, cmd, &arg) {
                    return true;
                }
                let tx = self.git.forge.sender();
                self.git_run("Pull request checks", workdir, move |c| {
                    let out = c.gh(&["pr", "view", "--json", "statusCheckRollup"], None)?;
                    let pr: PrStatusOnly = serde_json::from_str(&out).map_err(|e| format!("unexpected output from gh: {e}"))?;
                    let _ = tx.send(Incoming::Checks(pr.status_check_rollup));
                    Ok(String::new())
                });
            }
            Command::GhSubmitPr => {
                // Sent by the Create Pull Request dialog with its fields.
                let Some(Dialog::CreatePr { workdir, branch, base, title, body, draft }) = self.git.forge_pending_submit.take() else { return true };
                if title.trim().is_empty() {
                    self.set_status("The pull request needs a title".into(), true);
                    return true;
                }
                let remote = first_remote(&workdir).unwrap_or_else(|| "origin".into());
                let publish = status.upstream.is_none();
                let push = publish || status.ahead > 0;
                let tx = self.git.forge.sender();
                let wd = workdir.clone();
                self.git_run("Create pull request", workdir, move |c| {
                    // gh needs the head branch on the remote before it can open a PR.
                    if publish {
                        c.git(&["push", "--set-upstream", "--progress", &remote, &branch], None)?;
                    } else if push {
                        c.git(&["push", "--progress"], None)?;
                    }
                    // The body goes over stdin (`--body-file -`) so it is not limited or mangled as an argument.
                    let mut args = vec!["pr", "create", "--title", title.trim(), "--body-file", "-", "--base", &base, "--head", &branch];
                    if draft {
                        args.push("--draft");
                    }
                    let out = c.gh(&args, Some(body.as_bytes()))?;
                    // gh prints the new PR's URL as its last https line.
                    let url = out.lines().rev().find(|l| l.starts_with("https://")).unwrap_or("").trim().to_string();
                    if let Ok(v) = c.gh(&["pr", "view", "--json", forge::PR_FIELDS], None)
                        && let Ok(pr) = forge::parse_pr(&v)
                    {
                        let _ = tx.send(Incoming::Status(wd, branch.clone(), Some(Box::new(pr))));
                    }
                    Ok(if url.is_empty() { "Pull request created".into() } else { format!("Pull request created: {url}") })
                });
            }
            _ => {}
        }
        true
    }

    /// The Create Pull Request form.
    /// Takes the dialog by value and puts it back in `self.dialog` unless
    /// cancelled. On submit, stores it in `forge_pending_submit` and runs
    /// `Command::GhSubmitPr`. Escape cancels.
    pub(crate) fn show_create_pr_dialog(&mut self, ctx: &egui::Context, d: Dialog) {
        let Dialog::CreatePr { workdir, branch, mut base, mut title, mut body, mut draft } = d else { return };
        let mut submit = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("create-pr")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Create pull request");
            ui.label(format!("{branch} into"));
            ui.add(egui::TextEdit::singleline(&mut base).desired_width(200.0).hint_text("base branch"));
            ui.label("Title");
            ui.add(egui::TextEdit::singleline(&mut title).desired_width(f32::INFINITY));
            ui.label("Description");
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut body).desired_width(f32::INFINITY).desired_rows(8));
            });
            ui.checkbox(&mut draft, "Draft");
            ui.label(egui::RichText::new("Unpushed commits are pushed first. Created with the GitHub CLI (gh).").color(self.theme.ui.text_muted));
            ui.horizontal(|ui| {
                submit = ui.add_enabled(!title.trim().is_empty(), egui::Button::new("Create")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        let d = Dialog::CreatePr { workdir, branch, base, title, body, draft };
        if submit {
            self.git.forge_pending_submit = Some(d);
            self.dialog = None;
            self.execute(ctx, Command::GhSubmitPr, None);
        } else if !cancel {
            self.dialog = Some(d);
        } else {
            self.dialog = None;
        }
    }
}

/// The part of `gh pr view --json statusCheckRollup` output that Apex reads.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrStatusOnly {
    #[serde(default)]
    status_check_rollup: Vec<forge::CheckRun>,
}
