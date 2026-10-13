//! Branches, stash, merge conflicts and operations in progress (merge,
//! rebase, cherry-pick, revert), plus line history. Lists use the palette,
//! names use the prompt, changes run `git` as background jobs.
//!
//! How it fits: `git_branch_command` is one of the command handlers the
//! dispatcher in `app.rs` tries in turn. Listing (branches, stashes) reads
//! the repository through libgit2 (`apex_project::git_repo`) and fills the
//! command palette with items whose payload is a follow-up command (for
//! example `GitSwitchTo` with the branch name packed in the argument).
//! Anything that changes the repository goes through `git_run`
//! (`git_ops.rs`) after the trust check. Conflict resolution is purely a
//! buffer edit: `git_conflicts` parses the `<<<<<<<`/`=======`/`>>>>>>>`
//! markers of a document git reports as conflicted and caches the line
//! tints per document revision for the editor to paint.
//!
//! Gotcha: palette and prompt arguments are plain strings, so several
//! values are packed with the `SEP` control character.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use apex_edit::{DocId, Selection, SelectionSet};
use apex_project::git_repo::{self, Conflict, Operation, Resolution};
use egui::Color32;

use crate::app::{ApexApp, Dialog};
use crate::command::Command;
use crate::palette::{PaletteItem, PaletteMode, Payload};
use crate::workspace_ui::PromptKind;

/// Separator between packed command arguments.
pub(crate) const SEP: char = '\u{1f}';

/// Line tints and the conflict blocks of a document.
///
/// The tints are `(line, colour)` pairs sorted by line, shared with the
/// editor through `Arc` so a frame never copies them.
pub(crate) type ConflictInfo = (Arc<Vec<(usize, Color32)>>, Arc<Vec<Conflict>>);

/// Conflict tints per document, cached by revision.
#[derive(Default)]
pub struct ConflictCache {
    /// Document → (revision the info was computed for, info).
    docs: HashMap<DocId, (u64, ConflictInfo)>,
}

/// A palette row that runs `cmd` with `arg` when chosen.
fn item(label: impl Into<String>, detail: impl Into<String>, cmd: Command, arg: impl Into<String>) -> PaletteItem {
    PaletteItem { label: label.into(), detail: detail.into(), shortcut: String::new(), payload: Payload::Command(cmd, Some(arg.into())), boost: 0 }
}

/// A subset of git's ref-name rules (`git check-ref-format --branch`),
/// enough to reject names that would make `git switch -c` fail.
fn valid_branch_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with(['-', '/', '.'])
        && !n.ends_with(['/', '.'])
        && !n.contains("..")
        && !n.contains("@{")
        && !n.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\\\".contains(c))
        && !n.ends_with(".lock")
}

impl ApexApp {
    /// Handle the branch, stash, operation and conflict commands.
    ///
    /// Returns `false` for commands it does not own. Conflict editing and
    /// navigation work on the buffer only; everything else resolves the
    /// repository (the panel's choice for repository-wide commands, the
    /// active file's otherwise), requires trust, then either fills the
    /// palette, opens a prompt, asks for confirmation, or queues a job.
    pub(crate) fn git_branch_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        use Command::*;
        if !matches!(
            cmd,
            GitSwitchBranch
                | GitSwitchTo
                | GitCreateBranch
                | GitRenameBranch
                | GitDeleteBranch
                | GitDeleteBranchNamed
                | GitMergeBranch
                | GitMergeBranchNamed
                | GitCheckoutCommit
                | GitCherryPick
                | GitRevertCommit
                | GitStash
                | GitStashes
                | GitStashActions
                | GitStashApply
                | GitStashPop
                | GitStashDrop
                | GitStashShow
                | GitContinue
                | GitAbort
                | GitAcceptOurs
                | GitAcceptTheirs
                | GitAcceptBoth
                | GitNextConflict
                | GitPrevConflict
                | GitMarkResolved
                | GitLineHistory
        ) {
            return false;
        }
        // Conflict editing works on the buffer and needs no git.
        match cmd {
            GitAcceptOurs | GitAcceptTheirs | GitAcceptBoth => {
                let how = match cmd {
                    GitAcceptOurs => Resolution::Ours,
                    GitAcceptTheirs => Resolution::Theirs,
                    _ => Resolution::Both,
                };
                self.git_resolve_at_caret(how);
                return true;
            }
            GitNextConflict | GitPrevConflict => {
                self.git_jump_conflict(cmd == GitNextConflict);
                return true;
            }
            _ => {}
        }
        let Some((workdir, file)) = self.git_target(None) else { return true };
        // With no active file the panel's chosen repository wins over the first folder.
        let workdir = self.git_panel_repo().filter(|_| file.is_none()).unwrap_or(workdir);
        if !self.trust.is_trusted(&workdir) {
            self.dialog = Some(Dialog::Trust { root: workdir, then: Some((cmd, arg)) });
            return true;
        }
        let status = self.git.repos.get(&workdir).map(|e| e.status.clone()).unwrap_or_default();
        match cmd {
            GitSwitchBranch => {
                let list = git_repo::branches(&workdir).unwrap_or_default();
                let locals: Vec<String> = list.iter().filter(|b| !b.remote).map(|b| b.name.clone()).collect();
                let mut items = vec![item("+ Create new branch…", "from the current commit", GitCreateBranch, "")];
                for b in &list {
                    if b.head {
                        continue;
                    }
                    // A remote branch that already has a local one is reached through it.
                    if b.remote && locals.iter().any(|l| b.name.split_once('/').is_some_and(|(_, rest)| rest == l)) {
                        continue;
                    }
                    let detail = match (&b.upstream, b.remote) {
                        (_, true) => format!("remote • {}", b.summary),
                        (Some(u), _) => format!("tracks {u} • {}", b.summary),
                        (None, _) => b.summary.clone(),
                    };
                    // Packed argument: name, then whether it is a remote branch.
                    items.push(item(&b.name, detail, GitSwitchTo, format!("{}{SEP}{}", b.name, b.remote)));
                }
                self.palette.show_with(PaletteMode::Prompt("branches".into()), items, &format!("Switch branch (on {})", status.branch), "");
            }
            GitSwitchTo => {
                let Some((name, remote)) = arg.as_deref().and_then(|a| a.split_once(SEP)) else { return true };
                let (name, remote) = (name.to_string(), remote == "true");
                self.git_run("Switch branch", workdir, move |c| {
                    if remote {
                        // origin/feature -> a local feature tracking it.
                        c.git(&["switch", "--track", &name], None).map(|_| format!("Switched to a new branch tracking {name}"))
                    } else {
                        c.git(&["switch", &name], None).map(|_| format!("Switched to {name}"))
                    }
                });
            }
            // Two-step: no argument opens the prompt, which re-runs the command with the name
            // (optionally followed by SEP and a start point, from the log window).
            GitCreateBranch => match arg.filter(|a| !a.is_empty()) {
                None => {
                    self.prompt("New branch name", "", PromptKind::GitArg(GitCreateBranch, None));
                }
                Some(a) => {
                    let (name, at) = match a.split_once(SEP) {
                        Some((n, at)) => (n.trim().to_string(), Some(at.to_string())),
                        None => (a.trim().to_string(), None),
                    };
                    if !valid_branch_name(&name) {
                        self.set_status(format!("“{name}” is not a valid branch name"), true);
                        return true;
                    }
                    self.git_run("Create branch", workdir, move |c| {
                        let mut args = vec!["switch", "-c", name.as_str()];
                        if let Some(at) = &at {
                            args.push(at);
                        }
                        c.git(&args, None).map(|_| format!("Created and switched to {name}"))
                    });
                }
            },
            GitRenameBranch => match arg.filter(|a| !a.is_empty()) {
                None => {
                    if status.detached || status.branch.is_empty() {
                        self.set_status("Not on a branch".into(), false);
                        return true;
                    }
                    self.prompt(&format!("Rename {} to", status.branch), &status.branch.clone(), PromptKind::GitArg(GitRenameBranch, None));
                }
                Some(name) => {
                    let name = name.trim().to_string();
                    if !valid_branch_name(&name) {
                        self.set_status(format!("“{name}” is not a valid branch name"), true);
                        return true;
                    }
                    self.git_run("Rename branch", workdir, move |c| c.git(&["branch", "-m", &name], None).map(|_| format!("Renamed the branch to {name}")));
                }
            },
            GitDeleteBranch | GitMergeBranch => {
                let list = git_repo::branches(&workdir).unwrap_or_default();
                let target = if cmd == GitDeleteBranch { GitDeleteBranchNamed } else { GitMergeBranchNamed };
                // Only local branches can be deleted here; any branch can be merged.
                let items: Vec<PaletteItem> = list
                    .iter()
                    .filter(|b| !b.head && (cmd == GitMergeBranch || !b.remote))
                    .map(|b| item(&b.name, b.summary.clone(), target, b.name.clone()))
                    .collect();
                if items.is_empty() {
                    self.set_status("No other branches".into(), false);
                    return true;
                }
                let title = if cmd == GitDeleteBranch { "Delete branch".to_string() } else { format!("Merge into {}", status.branch) };
                self.palette.show_with(PaletteMode::Prompt("branches".into()), items, &title, "");
            }
            GitDeleteBranchNamed => {
                let Some(name) = arg.clone() else { return true };
                let body = format!("Delete the local branch {name}? Git refuses if it has commits that are not merged anywhere.");
                if !self.git_confirm("Delete branch?", body, "Delete", cmd, &arg) {
                    return true;
                }
                self.git_run("Delete branch", workdir, move |c| {
                    // `-d` (not `-D`): never throw away unmerged work from the UI; tell the user how instead.
                    c.git(&["branch", "-d", &name], None).map(|_| format!("Deleted {name}")).map_err(|e| {
                        if e.contains("not fully merged") {
                            format!("{name} has unmerged commits; delete it with `git branch -D {name}` if you are sure")
                        } else {
                            e
                        }
                    })
                });
            }
            GitMergeBranchNamed => {
                let Some(name) = arg else { return true };
                self.git_run("Merge", workdir, move |c| {
                    // A conflicted merge exits non-zero; turn git's wall of text into one instruction.
                    c.git(&["merge", "--no-edit", &name], None).map(|_| format!("Merged {name}")).map_err(|e| {
                        if e.contains("CONFLICT") || e.contains("Automatic merge failed") {
                            "conflicts: resolve them, stage the files, then Continue".to_string()
                        } else {
                            e
                        }
                    })
                });
            }
            GitCheckoutCommit => {
                let Some(id) = arg.clone() else { return true };
                let body = format!("Check out {} without a branch (detached HEAD)? New commits there need a branch to be kept.", &id[..id.len().min(10)]);
                if !self.git_confirm("Check out commit?", body, "Check Out", cmd, &arg) {
                    return true;
                }
                self.git_run("Check out", workdir, move |c| {
                    c.git(&["switch", "--detach", &id], None).map(|_| format!("Checked out {}", &id[..id.len().min(7)]))
                });
            }
            GitCherryPick => {
                let Some(id) = arg else { return true };
                self.git_run("Cherry-pick", workdir, move |c| c.git(&["cherry-pick", &id], None).map(|_| format!("Cherry-picked {}", &id[..id.len().min(7)])));
            }
            GitRevertCommit => {
                let Some(id) = arg.clone() else { return true };
                let body = format!("Create a new commit that undoes {}?", &id[..id.len().min(10)]);
                if !self.git_confirm("Revert commit?", body, "Revert", cmd, &arg) {
                    return true;
                }
                self.git_run("Revert", workdir, move |c| c.git(&["revert", "--no-edit", &id], None).map(|_| format!("Reverted {}", &id[..id.len().min(7)])));
            }
            GitStash => match arg {
                None => self.prompt("Stash message (optional; untracked files are included)", "", PromptKind::GitArg(GitStash, None)),
                Some(msg) => self.git_run("Stash", workdir, move |c| {
                    let mut args = vec!["stash", "push", "--include-untracked"];
                    if !msg.trim().is_empty() {
                        args.extend(["-m", msg.trim()]);
                    }
                    // `stash push` succeeds with nothing to do; detect that from its output.
                    c.git(&args, None)
                        .map(|out| if out.contains("No local changes") { "No local changes to stash".into() } else { "Stashed your changes".into() })
                }),
            },
            GitStashes => {
                let list = git_repo::stashes(&workdir).unwrap_or_default();
                if list.is_empty() {
                    self.set_status("No stashes".into(), false);
                    return true;
                }
                let items: Vec<PaletteItem> =
                    list.iter().map(|s| item(format!("stash@{{{}}}", s.index), s.message.clone(), GitStashActions, s.index.to_string())).collect();
                self.palette.show_with(PaletteMode::Prompt("stashes".into()), items, "Stashes", "");
            }
            // Second palette level: what to do with the chosen stash.
            GitStashActions => {
                let Some(i) = arg else { return true };
                let items = vec![
                    item("Apply", "keep the stash", GitStashApply, i.clone()),
                    item("Pop", "apply and remove it", GitStashPop, i.clone()),
                    item("Show", "as a diff", GitStashShow, i.clone()),
                    item("Drop…", "delete it", GitStashDrop, i.clone()),
                ];
                self.palette.show_with(PaletteMode::Prompt("stash_actions".into()), items, &format!("stash@{{{i}}}"), "");
            }
            GitStashApply | GitStashPop | GitStashDrop | GitStashShow => {
                let i: usize = arg.as_deref().and_then(|a| a.parse().ok()).unwrap_or(0);
                let r = format!("stash@{{{i}}}");
                if cmd == GitStashDrop && !self.git_confirm("Drop stash?", format!("Delete {r}? Its changes are lost."), "Drop", cmd, &arg) {
                    return true;
                }
                if cmd == GitStashShow {
                    // The diff text comes back on the text channel and opens as a read-only tab.
                    let tx = self.git_text_sender();
                    self.git_run("Show stash", workdir, move |c| {
                        let out = c.git_quiet(&["stash", "show", "-p", "--include-untracked", &r], None)?;
                        let _ = tx.send((format!("{r}.diff"), out));
                        Ok(String::new())
                    });
                    return true;
                }
                let verb = match cmd {
                    GitStashApply => "apply",
                    GitStashPop => "pop",
                    _ => "drop",
                };
                self.git_run("Stash", workdir, move |c| c.git(&["stash", verb, &r], None).map(|_| format!("Stash {verb}: {r}")));
            }
            GitContinue | GitAbort => {
                let Some(op) = status.operation else {
                    self.set_status("No merge, rebase, cherry-pick or revert is in progress".into(), false);
                    return true;
                };
                if cmd == GitAbort
                    && !self.git_confirm(
                        &format!("Abort the {}?", op.verb()),
                        format!("Return to the state before the {} started? Conflict resolutions made so far are discarded.", op.verb()),
                        "Abort",
                        cmd,
                        &arg,
                    )
                {
                    return true;
                }
                if cmd == GitContinue && status.entries.iter().any(|e| e.conflict) {
                    self.set_status("Resolve and stage every conflicted file first (Mark Resolved)".into(), false);
                    return true;
                }
                let verb = op.verb();
                let flag = if cmd == GitContinue { "--continue" } else { "--abort" };
                // `core.editor=true` keeps the prepared message (git runs it via sh).
                self.git_run(if cmd == GitContinue { "Continue" } else { "Abort" }, workdir, move |c| {
                    c.git(&["-c", "core.editor=true", verb, flag], None)
                        .map(|_| format!("{} {verb}", if flag == "--continue" { "Continued" } else { "Aborted" }))
                });
            }
            GitMarkResolved => {
                let Some(path) = file else {
                    self.set_status("Open the conflicted file first".into(), false);
                    return true;
                };
                if let Some((_, d)) = self.active_ids()
                    && let Some(doc) = self.docs.get(&d)
                {
                    // Staging reads the file on disk, so unsaved resolutions would be lost.
                    if doc.is_dirty() {
                        self.set_status("Save the file first".into(), false);
                        return true;
                    }
                    let left = git_repo::conflicts(doc.text().lines()).len();
                    if left > 0
                        && !self.git_confirm(
                            "Conflict markers remain",
                            format!("{left} conflict block(s) are still in the file. Stage it anyway?"),
                            "Stage",
                            cmd,
                            &arg,
                        )
                    {
                        return true;
                    }
                }
                let rel = crate::git_ops::rel_path(&workdir, &path).unwrap_or_default();
                self.git_run("Mark resolved", workdir, move |c| c.git(&["add", "--", &rel], None).map(|_| format!("Marked {rel} as resolved")));
            }
            GitLineHistory => {
                let Some(path) = file else { return true };
                let Some((v, d)) = self.active_ids() else { return true };
                let rope = self.docs[&d].rope();
                let s = self.views[&v].sels.primary().range();
                // `git log -L` takes one-based, inclusive line numbers; an empty selection is one line.
                let a = rope.char_to_line(s.start.min(rope.len_chars())) + 1;
                let b = rope.char_to_line(s.end.min(rope.len_chars())).max(a - 1) + 1;
                let rel = crate::git_ops::rel_path(&workdir, &path).unwrap_or_default();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let tx = self.git_text_sender();
                self.git_run("Line history", workdir, move |c| {
                    let spec = format!("{a},{b}:{rel}");
                    let out = c.git_quiet(&["log", "--no-color", "-L", &spec], None)?;
                    if out.trim().is_empty() {
                        return Err("no committed history for these lines".into());
                    }
                    let _ = tx.send((format!("{name} lines {a}-{b} history.diff"), out));
                    Ok(String::new())
                });
            }
            _ => {}
        }
        let _ = ctx;
        true
    }

    /// Channel for jobs that produce a text to show in a read-only tab.
    ///
    /// Created lazily on first use; `git_text_poll` reads the receiving end.
    pub(crate) fn git_text_sender(&mut self) -> std::sync::mpsc::Sender<(String, String)> {
        if self.git.text_tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.git.text_tx = Some(tx);
            self.git.text_rx = Some(rx);
        }
        self.git.text_tx.clone().expect("set above")
    }

    /// Open every `(title, text)` pair sent by a job as a read-only diff tab.
    /// Called from the frame loop.
    pub(crate) fn git_text_poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.git.text_rx else { return };
        // Collect first: opening a document borrows `self` mutably.
        let got: Vec<(String, String)> = rx.try_iter().collect();
        for (title, text) in got {
            self.open_scratch_doc(&title, &text, Some("diff"), true, ctx);
        }
    }

    /// Conflict blocks of a document that git reports as conflicted.
    ///
    /// Returns `None` for documents that are neither conflicted nor cached.
    /// Parses the markers from the buffer text (so edits are reflected at
    /// once) and caches the result by document revision. The cache entry is
    /// kept while git still reports the file as conflicted, even if the
    /// markers are gone, so the gutter clears as soon as the user resolves
    /// them rather than on the next status scan.
    pub(crate) fn git_conflicts(&mut self, d: DocId) -> Option<ConflictInfo> {
        let path = self.docs.get(&d)?.path.clone()?;
        let workdir = self.git_workdir_for_doc(d)?;
        let conflicted = self.git.repos.get(&workdir)?.status.entries.iter().any(|e| e.path == path && e.conflict);
        let doc = self.docs.get(&d)?;
        if !conflicted && !self.git.conflicts.docs.contains_key(&d) {
            return None;
        }
        let rev = doc.revision();
        if let Some((r, info)) = self.git.conflicts.docs.get(&d)
            && *r == rev
        {
            return Some(info.clone());
        }
        let cs = git_repo::conflicts(doc.text().lines());
        // Marker lines get a neutral tint, "ours" the added colour, "theirs" the
        // accent, and the optional diff3 base block a lighter neutral tint.
        let (ours, theirs, marker) = (self.theme.editor.git_added, self.theme.ui.accent, self.theme.ui.text_muted);
        let a = |c: Color32, al: u8| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), al);
        let mut tints = Vec::new();
        for c in &cs {
            tints.push((c.start, a(marker, 50)));
            tints.extend(c.ours().map(|l| (l, a(ours, 40))));
            if let Some(b) = c.base {
                tints.extend((b..c.mid).map(|l| (l, a(marker, 30))));
            }
            tints.push((c.mid, a(marker, 50)));
            tints.extend(c.theirs().map(|l| (l, a(theirs, 40))));
            tints.push((c.end, a(marker, 50)));
        }
        // The editor binary-searches the tints by line.
        tints.sort_by_key(|t| t.0);
        let info: ConflictInfo = (Arc::new(tints), Arc::new(cs));
        if conflicted || !info.1.is_empty() {
            self.git.conflicts.docs.insert(d, (rev, info.clone()));
        } else {
            self.git.conflicts.docs.remove(&d);
        }
        Some(info)
    }

    /// The conflict block containing the primary caret's line, with its view.
    fn git_conflict_at_caret(&mut self) -> Option<(crate::view::ViewId, Conflict)> {
        let (v, d) = self.active_ids()?;
        let line = self.views.get(&v)?.primary_line(self.docs.get(&d)?);
        let (_, cs) = self.git_conflicts(d)?;
        cs.iter().find(|c| (c.start..=c.end).contains(&line)).map(|c| (v, *c))
    }

    /// Replace the conflict block at the caret with one side (or both) as
    /// an undoable edit; the caret ends up at the start of the block.
    fn git_resolve_at_caret(&mut self, how: Resolution) {
        let Some((v, c)) = self.git_conflict_at_caret() else {
            self.set_status("The caret is not in a conflict block".into(), false);
            return;
        };
        self.run_edit(v, move |doc, _| {
            let rope = doc.rope();
            let n = rope.len_lines();
            let start = rope.line_to_char(c.start.min(n));
            // Include the terminator of the closing marker line, unless it is the last line.
            let end = if c.end + 1 < n { rope.line_to_char(c.end + 1) } else { rope.len_chars() };
            let block = rope.slice(start..end).to_string();
            let lines: Vec<&str> = block.split_inclusive('\n').collect();
            // `resolve` works on the extracted block, so rebase the marker lines to 0.
            let rel = Conflict { start: 0, base: c.base.map(|b| b - c.start), mid: c.mid - c.start, end: c.end - c.start };
            let text = git_repo::resolve(&lines, &rel, how);
            let tx = apex_edit::Transaction::single(apex_edit::Change::replace(start, end, text));
            Some(apex_edit::ops::EditResult { tx, sels: SelectionSet::single(Selection::point(start)), kind: apex_edit::EditKind::Other })
        });
    }

    /// Move the caret to the next (or previous) conflict block, wrapping
    /// around the file; records a jump for "go back".
    fn git_jump_conflict(&mut self, forward: bool) {
        let Some((v, d)) = self.active_ids() else { return };
        let Some((_, cs)) = self.git_conflicts(d) else {
            self.set_status("No conflicts in this file".into(), false);
            return;
        };
        if cs.is_empty() {
            self.set_status("No conflicts in this file".into(), false);
            return;
        }
        let line = self.views[&v].primary_line(&self.docs[&d]);
        let target = if forward { cs.iter().find(|c| c.start > line).or(cs.first()) } else { cs.iter().rev().find(|c| c.start < line).or(cs.last()) };
        if let Some(c) = target {
            self.note_jump();
            self.goto_line_col(v, c.start + 1, 1);
        }
    }

    /// "Merging" etc. for the status bar and panel.
    ///
    /// Returns the panel's repository, the operation git reports as in
    /// progress, and how many files are still conflicted.
    pub(crate) fn git_operation(&mut self) -> Option<(PathBuf, Operation, usize)> {
        let w = self.git_panel_repo()?;
        let st = &self.git.repos.get(&w)?.status;
        let op = st.operation?;
        Some((w, op, st.entries.iter().filter(|e| e.conflict).count()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the accepted and rejected shapes of a branch name.
    #[test]
    fn branch_names() {
        for ok in ["main", "feature/x", "fix-1.2", "a_b"] {
            assert!(valid_branch_name(ok), "{ok}");
        }
        for bad in ["", "-x", "a b", "a..b", "x.lock", "a:b", "x/", "a~1", "@{u}x"] {
            assert!(!valid_branch_name(bad), "{bad}");
        }
    }
}
