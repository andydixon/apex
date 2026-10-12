//! The Source Control panel: changed files of one repository grouped into
//! conflicts, staged and unstaged changes, and the commit box. Shown in the
//! lower part of the sidebar (`git_sidebar`); every action dispatches a Git
//! command, so the menu, palette and panel behave the same.

use std::path::{Path, PathBuf};

use apex_project::git::{FileStatus, StatusEntry};
use egui::{Rect, Sense, Ui, pos2, vec2};

use crate::app::ApexApp;
use crate::command::Command;
use crate::git_ops::rel_path;

#[derive(Default)]
pub struct SourceControl {
    /// Repository shown (work tree); follows the active file when unset.
    pub repo: Option<PathBuf>,
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    /// Messages of earlier commits, newest last (Up/Down in an empty box).
    pub history: Vec<String>,
    history_pos: Option<usize>,
    pub focus_message: bool,
    selected: Option<PathBuf>,
}

fn letter(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Modified | FileStatus::Staged => "M",
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Untracked => "U",
        FileStatus::Conflict => "!",
        FileStatus::Ignored => "I",
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Group {
    Conflicts,
    Staged,
    Changes,
}

impl ApexApp {
    /// The repository the panel and repository-wide commands work on.
    pub(crate) fn git_panel_repo(&mut self) -> Option<PathBuf> {
        if let Some(r) = &self.git.sc.repo
            && self.git.repos.contains_key(r)
        {
            return Some(r.clone());
        }
        let active = self.active_ids().and_then(|(_, d)| self.git_workdir_for_doc(d));
        if let Some(a) = active.filter(|a| self.git.repos.contains_key(a)) {
            return Some(a);
        }
        let mut keys: Vec<&PathBuf> = self.git.repos.keys().collect();
        keys.sort();
        keys.first().map(|p| (*p).clone())
    }

    pub(crate) fn git_panel_visible(&self) -> bool {
        self.settings.git_sidebar && self.git.has_repos()
    }

    pub(crate) fn show_source_control(&mut self, ui: &mut Ui) {
        let Some(repo) = self.git_panel_repo() else { return };
        let Some(entry) = self.git.repos.get(&repo) else { return };
        let status = entry.status.clone();
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        let muted = theme.ui.text_muted;
        ui.painter().rect_filled(ui.available_rect_before_wrap(), 0.0, theme.ui.sidebar);
        ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
        let mut cmds: Vec<(Command, Option<String>)> = Vec::new();
        let count = status.entries.len();

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("SOURCE CONTROL{}", if count > 0 { format!(" ({count})") } else { String::new() }))
                    .size(ui_font - 2.0)
                    .color(muted)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("…", |ui| {
                    for c in [Command::GitStageAll, Command::GitUnstageAll, Command::GitCommitAmend, Command::GitUndoCommit, Command::GitShowOutput] {
                        if ui.button(c.name()).clicked() {
                            cmds.push((c, None));
                            ui.close();
                        }
                    }
                });
            });
        });
        // Repository / branch line, with a chooser when several are open.
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let mut branch = status.branch.clone();
            if status.unborn {
                branch.push_str(" (no commits)");
            }
            if let Some(u) = &status.upstream {
                if status.ahead + status.behind > 0 {
                    branch.push_str(&format!("  ↑{} ↓{}", status.ahead, status.behind));
                }
                ui.label(egui::RichText::new(format!("⎇ {branch}")).size(ui_font - 1.0)).on_hover_text(format!("Tracking {u}"));
            } else {
                ui.label(egui::RichText::new(format!("⎇ {branch}")).size(ui_font - 1.0));
            }
            if self.git.repos.len() > 1 {
                let mut repos: Vec<PathBuf> = self.git.repos.keys().cloned().collect();
                repos.sort();
                let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                egui::ComboBox::from_id_salt("git-repo").selected_text(name(&repo)).show_ui(ui, |ui| {
                    for r in &repos {
                        if ui.selectable_label(*r == repo, name(r)).clicked() {
                            self.git.sc.repo = Some(r.clone());
                        }
                    }
                });
            }
        });

        // Commit box.
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let hint = if self.git.sc.amend { "Message (empty keeps the last one)" } else { "Message (Ctrl+Enter to commit)" };
            let edit = egui::TextEdit::multiline(&mut self.git.sc.message)
                .id_salt("git-commit-message")
                .hint_text(hint)
                .desired_rows(3)
                .desired_width(ui.available_width() - 6.0)
                .font(egui::FontId::proportional(ui_font));
            let r = ui.add(edit);
            if std::mem::take(&mut self.git.sc.focus_message) {
                r.request_focus();
            }
            if r.has_focus() {
                let (enter, up, down) = ui.input(|i| {
                    (i.modifiers.command && i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown))
                });
                if enter {
                    cmds.push((Command::GitCommit, None));
                }
                // History recall only while the box is empty or showing history.
                let sc = &mut self.git.sc;
                let browsing = sc.message.is_empty() || sc.history_pos.is_some_and(|i| sc.history.get(i) == Some(&sc.message));
                if browsing && !sc.history.is_empty() && (up || down) {
                    let n = sc.history.len();
                    let pos = match (sc.history_pos, up) {
                        (None, true) => Some(n - 1),
                        (None, false) => None,
                        (Some(i), true) => Some(i.saturating_sub(1)),
                        (Some(i), false) if i + 1 < n => Some(i + 1),
                        (Some(_), false) => None,
                    };
                    sc.history_pos = pos;
                    sc.message = pos.map(|i| sc.history[i].clone()).unwrap_or_default();
                }
            }
        });
        let summary_len = self.git.sc.message.lines().next().map(|l| l.chars().count()).unwrap_or(0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let staged = status.entries.iter().filter(|e| e.staged.is_some()).count();
            let label = if self.git.sc.amend { "Amend" } else { "Commit" };
            let b = ui.add_enabled(self.git.ops.running.is_none(), egui::Button::new(label));
            if b.on_hover_text(if staged == 0 { "Nothing staged: offers to commit all tracked changes" } else { "Commit the staged changes" }).clicked() {
                cmds.push((Command::GitCommit, None));
            }
            ui.checkbox(&mut self.git.sc.amend, "Amend");
            ui.checkbox(&mut self.git.sc.signoff, "Sign-off").on_hover_text("Add a Signed-off-by trailer (-s)");
            if summary_len > 0 {
                let colour = if summary_len > 72 {
                    theme.ui.error
                } else if summary_len > 50 {
                    theme.ui.warning
                } else {
                    muted
                };
                ui.label(egui::RichText::new(summary_len.to_string()).size(ui_font - 2.0).color(colour))
                    .on_hover_text("Summary line length (50 recommended, 72 at most)");
            }
        });
        ui.separator();

        let mut conflicts: Vec<&StatusEntry> = Vec::new();
        let mut staged: Vec<&StatusEntry> = Vec::new();
        let mut changes: Vec<&StatusEntry> = Vec::new();
        for e in &status.entries {
            if e.conflict {
                conflicts.push(e);
                continue;
            }
            if e.staged.is_some() {
                staged.push(e);
            }
            if e.unstaged.is_some() {
                changes.push(e);
            }
        }
        if status.entries.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("No changes").color(muted));
            });
        }
        let row_h = ui_font + 8.0;
        egui::ScrollArea::vertical().id_salt("git-sc-files").auto_shrink([false, false]).show(ui, |ui| {
            for (group, title, list) in
                [(Group::Conflicts, "MERGE CONFLICTS", &conflicts), (Group::Staged, "STAGED CHANGES", &staged), (Group::Changes, "CHANGES", &changes)]
            {
                if list.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!("{title}  {}", list.len())).size(ui_font - 3.0).color(muted).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (icon, tip, c) =
                            if group == Group::Staged { ("−", "Unstage all", Command::GitUnstageAll) } else { ("+", "Stage all", Command::GitStageAll) };
                        if ui.small_button(icon).on_hover_text(tip).clicked() {
                            cmds.push((c, None));
                        }
                    });
                });
                for e in list.iter() {
                    self.git_panel_row(ui, &repo, e, group, row_h, &mut cmds);
                }
                ui.add_space(4.0);
            }
        });
        self.queue.extend(cmds);
    }

    fn git_panel_row(&mut self, ui: &mut Ui, repo: &Path, e: &StatusEntry, group: Group, row_h: f32, cmds: &mut Vec<(Command, Option<String>)>) {
        let theme = &self.theme;
        let ui_font = self.settings.ui_font_size;
        let st = match group {
            Group::Conflicts => FileStatus::Conflict,
            Group::Staged => e.staged.unwrap_or(FileStatus::Modified),
            Group::Changes => e.unstaged.unwrap_or(FileStatus::Modified),
        };
        let colour = match st {
            FileStatus::Added | FileStatus::Untracked => theme.editor.git_added,
            FileStatus::Deleted | FileStatus::Conflict => theme.editor.git_deleted,
            _ => theme.editor.git_modified,
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let selected = self.git.sc.selected.as_deref() == Some(e.path.as_path());
        if selected || hovered {
            ui.painter().rect_filled(rect, 2.0, if selected { theme.ui.selection } else { theme.ui.hover });
        }
        let painter = ui.painter_at(rect);
        let font = egui::FontId::proportional(ui_font);
        let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rel = rel_path(repo, &e.path).unwrap_or_default();
        let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let x = rect.min.x + 12.0;
        let g = painter.layout_no_wrap(name.clone(), font.clone(), if st == FileStatus::Deleted { theme.ui.text_muted } else { theme.ui.text });
        let name_w = g.rect.width();
        painter.galley(pos2(x, rect.center().y - g.rect.height() / 2.0), g, theme.ui.text);
        if !dir.is_empty() {
            painter.text(
                pos2(x + name_w + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &dir,
                egui::FontId::proportional(ui_font - 2.0),
                theme.ui.text_muted,
            );
        }
        painter.text(pos2(rect.max.x - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, letter(st), egui::FontId::proportional(ui_font - 1.0), colour);
        let path_arg = Some(e.path.display().to_string());
        if hovered {
            // Inline actions, right to left, before the status letter.
            let mut actions: Vec<(&str, &str, Command)> = Vec::new();
            match group {
                Group::Staged => actions.push(("−", "Unstage", Command::GitUnstageFile)),
                Group::Changes | Group::Conflicts => {
                    actions.push(("+", if group == Group::Conflicts { "Mark resolved (stage)" } else { "Stage" }, Command::GitStageFile));
                    if group == Group::Changes {
                        actions.push(("↶", "Discard changes", Command::GitDiscardFile));
                    }
                }
            }
            let bw = row_h - 2.0;
            let mut bx = rect.max.x - 24.0 - bw;
            for (icon, tip, c) in actions {
                let r = Rect::from_min_size(pos2(bx, rect.min.y + 1.0), vec2(bw, bw));
                if ui.put(r, egui::Button::new(icon).small()).on_hover_text(tip).clicked() {
                    cmds.push((c, path_arg.clone()));
                }
                bx -= bw + 2.0;
            }
        }
        let open_changes = if group == Group::Staged { Command::GitOpenStagedChanges } else { Command::GitOpenChanges };
        if resp.double_clicked() {
            cmds.push((open_changes, path_arg.clone()));
        } else if resp.clicked() {
            self.git.sc.selected = Some(e.path.clone());
            if st != FileStatus::Deleted {
                self.open_preview(&e.path);
            }
        }
        resp.on_hover_text(&rel).context_menu(|ui| {
            let mut item = |ui: &mut Ui, label: &str, c: Command| {
                if ui.button(label).clicked() {
                    cmds.push((c, path_arg.clone()));
                    ui.close();
                }
            };
            if st != FileStatus::Deleted {
                item(ui, "Open File", Command::OpenRecent);
            }
            item(ui, "Open Changes", open_changes);
            ui.separator();
            match group {
                Group::Staged => item(ui, "Unstage", Command::GitUnstageFile),
                _ => {
                    item(ui, "Stage", Command::GitStageFile);
                    if group == Group::Changes {
                        item(ui, "Discard Changes…", Command::GitDiscardFile);
                    }
                }
            }
            ui.separator();
            if ui.button("Copy Path").clicked() {
                ui.ctx().copy_text(e.path.display().to_string());
                ui.close();
            }
        });
    }

    /// Commit, amend and undo; the message comes from the panel.
    pub(crate) fn git_commit_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::GitToggleSourceControl => {
                self.settings.git_sidebar = !self.settings.git_sidebar;
                if self.settings.git_sidebar {
                    self.settings.sidebar = true;
                }
                return true;
            }
            Command::GitOpenChanges | Command::GitOpenStagedChanges => {
                self.git_open_changes(ctx, arg.as_deref(), cmd == Command::GitOpenStagedChanges);
                return true;
            }
            Command::GitCommit | Command::GitCommitAmend | Command::GitUndoCommit => {}
            _ => return false,
        }
        let Some(workdir) = self.git_panel_repo().or_else(|| self.git_target(None).map(|t| t.0)) else { return true };
        if !self.trust.is_trusted(&workdir) {
            self.dialog = Some(crate::app::Dialog::Trust { root: workdir, then: Some((cmd, arg)) });
            return true;
        }
        let status = self.git.repos.get(&workdir).map(|e| e.status.clone()).unwrap_or_default();
        if cmd == Command::GitUndoCommit {
            if status.unborn {
                self.set_status("There is no commit to undo".into(), false);
                return true;
            }
            let msg = apex_project::git::head_message(&workdir).unwrap_or_default();
            let summary = msg.lines().next().unwrap_or("").to_string();
            let body = format!(
                "Undo “{summary}”? The commit is removed from {} and its changes stay staged; its message goes back into the commit box.",
                status.branch
            );
            if !self.git_confirm("Undo last commit?", body, "Undo Commit", cmd, &arg) {
                return true;
            }
            self.git.sc.message = msg.trim_end().to_string();
            self.git_run("Undo commit", workdir, |c| {
                c.git(&["reset", "--soft", "HEAD~1"], None).map(|_| "Undid the last commit; its changes are staged".into())
            });
            return true;
        }
        let amend = cmd == Command::GitCommitAmend || self.git.sc.amend;
        let message = self.git.sc.message.trim().to_string();
        if message.is_empty() && !amend {
            self.set_status("Enter a commit message".into(), false);
            self.settings.sidebar = true;
            self.settings.git_sidebar = true;
            self.git.sc.focus_message = true;
            return true;
        }
        if amend && status.unborn {
            self.set_status("There is no commit to amend".into(), false);
            return true;
        }
        let staged = status.entries.iter().any(|e| e.staged.is_some());
        let tracked_changes = status.entries.iter().any(|e| matches!(e.unstaged, Some(s) if s != FileStatus::Untracked));
        let mut all = false;
        if !staged && !amend {
            if !tracked_changes {
                self.set_status("Nothing to commit".into(), false);
                return true;
            }
            if !self.git_confirm("Nothing is staged", "Commit all changes to tracked files? Untracked files are not included.".into(), "Commit All", cmd, &arg)
            {
                return true;
            }
            all = true;
        }
        if self.dirty_docs_in(&workdir) > 0 {
            self.set_status("Note: some files in this repository have unsaved changes, which are not committed".into(), false);
        }
        let signoff = self.git.sc.signoff;
        if !message.is_empty() {
            self.git.sc.history.retain(|m| m != &message);
            self.git.sc.history.push(message.clone());
            if self.git.sc.history.len() > 30 {
                self.git.sc.history.remove(0);
            }
        }
        self.git.sc.history_pos = None;
        self.git.sc.message.clear();
        self.git.sc.amend = false;
        self.git_run(if amend { "Amend" } else { "Commit" }, workdir, move |c| {
            let mut args = vec!["commit"];
            if amend {
                args.push("--amend");
            }
            if all {
                args.push("-a");
            }
            if signoff {
                args.push("-s");
            }
            if message.is_empty() {
                args.push("--no-edit");
                c.git(&args, None)?;
            } else {
                args.extend(["-F", "-"]);
                c.git(&args, Some(message.as_bytes()))?;
            }
            let head = c.git_quiet(&["log", "-1", "--format=%h %s"], None).unwrap_or_default();
            Ok(format!("Committed {}", head.trim()))
        });
        true
    }

    fn dirty_docs_in(&self, workdir: &Path) -> usize {
        self.docs.values().filter(|d| d.is_dirty() && d.path.as_ref().is_some_and(|p| p.starts_with(workdir))).count()
    }

    /// Unified diff of a file's unstaged (index → working tree) or staged
    /// (HEAD → index) changes in a read-only tab.
    fn git_open_changes(&mut self, ctx: &egui::Context, arg: Option<&str>, staged: bool) {
        let Some((workdir, Some(path))) = self.git_target(arg) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let index = apex_project::git::file_at(&workdir, &path, false).ok().flatten();
        let (base, current, labels) = if staged {
            (apex_project::git::file_at(&workdir, &path, true).ok().flatten(), index, ("HEAD", "index"))
        } else {
            let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
            let disk = apex_project::fileio::load(&path, None, fallback).ok().map(|l| l.text);
            (index, disk, ("index", "working tree"))
        };
        let (base, current) = (base.unwrap_or_default(), current.unwrap_or_default());
        if base == current {
            self.set_status(format!("No {} changes in {name}", if staged { "staged" } else { "unstaged" }), false);
            return;
        }
        let diff = apex_project::git::unified_diff(&base, &current, &format!("{name} ({})", labels.0), &format!("{name} ({})", labels.1));
        self.open_scratch_doc(&format!("{name} ({} changes).diff", if staged { "staged" } else { "unstaged" }), &diff, Some("diff"), true, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_letters() {
        assert_eq!(letter(FileStatus::Untracked), "U");
        assert_eq!(letter(FileStatus::Staged), "M");
        assert_eq!(letter(FileStatus::Conflict), "!");
    }
}
