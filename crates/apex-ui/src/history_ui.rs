//! Git > Edit Commit History: a GUI for rewriting commit metadata in the
//! workspace's repository (the first folder's work tree). Commits are listed
//! newest first; edits are drafts until Apply, which rewrites history with
//! `git filter-branch` after tagging a backup (see `git_history.rs`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use apex_project::git_history::{self as gh, CommitRecord, GitDate, RepoFacts};
use egui::{Color32, RichText, Sense, Ui, pos2, vec2};

use crate::app::ApexApp;
use crate::command::Command;

/// Which of a commit's two dates the calendar edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DateField {
    Author,
    Committer,
}

/// The modal shown over the editor window, with its form state.
#[derive(Default)]
enum Mode {
    /// No modal open.
    #[default]
    None,
    /// Replace author: `query` matches name or email; `exact` turns off
    /// case-insensitive substring matching.
    BulkAuthor { query: String, name: String, email: String, exact: bool },
    /// Rewrite dates: day range `start`..=`end` (YYYY-MM-DD) and daily time
    /// window `from`..`to` (HH:MM), all as typed.
    Dates { start: String, end: String, from: String, to: String },
    /// Date picker for the selected commit's `field`; `view_year` and
    /// `view_month` are the month on show, which may differ from `date`'s.
    Calendar { field: DateField, date: GitDate, view_year: i64, view_month: u32 },
    /// Final confirmation before Apply; `typed` holds the "FORCE" text
    /// required when force-pushing.
    Confirm { typed: String },
}

/// Result of the background load job: repository facts and commits, oldest first.
type Loaded = Result<(RepoFacts, Vec<CommitRecord>), String>;

/// State of the Edit Commit History window.
#[derive(Default)]
pub struct HistoryEditor {
    /// Whether the window is shown.
    pub open: bool,
    /// Work tree of the repository being edited.
    repo: PathBuf,
    /// Branch, cleanliness, origin and local UTC offset, from the last load.
    facts: RepoFacts,
    /// Oldest first, as loaded.
    originals: Vec<CommitRecord>,
    /// Edited records keyed by full hash; a commit without an entry is unedited.
    drafts: HashMap<String, CommitRecord>,
    /// Receiver for an in-flight load job; `Some` while loading.
    loading: Option<Receiver<Loaded>>,
    /// Reload once the apply job has finished.
    awaiting_apply: bool,
    /// Full hash of the selected commit.
    selected: Option<String>,
    /// Filter text matched against hash, author, email and message.
    search: String,
    /// Show only commits in `ai`.
    ai_only: bool,
    /// Hashes of commits that mention Claude, Codex or Anthropic (see `gh::ai_commits`).
    ai: HashSet<String>,
    /// Apply option: strip co-author and session trailers from every message.
    remove_co_authors: bool,
    /// Apply option: force-push all branches after rewriting.
    force_push: bool,
    /// Apply option: also force-push tags (only with `force_push`).
    push_tags: bool,
    /// The open modal, if any.
    mode: Mode,
    /// Error from the last load, shown under the repository line.
    error: Option<String>,
    /// Scroll the list to the selection on the next frame (after keyboard navigation).
    scroll_to_selected: bool,
}

impl HistoryEditor {
    /// The record for `hash` as currently edited: the draft if any, else the original.
    fn draft(&self, hash: &str) -> Option<&CommitRecord> {
        self.drafts.get(hash).or_else(|| self.originals.iter().find(|c| c.hash == hash))
    }

    /// Whether `c` has a draft that differs from it in an editable field.
    fn is_dirty(&self, c: &CommitRecord) -> bool {
        self.drafts.get(&c.hash).is_some_and(|d| gh::changed(c, d))
    }

    /// Commits shown, newest first, after the search and AI filters.
    fn visible(&self) -> Vec<&CommitRecord> {
        let q = self.search.trim().to_lowercase();
        self.originals
            .iter()
            .rev()
            .filter(|c| !self.ai_only || self.ai.contains(&c.hash))
            .filter(|c| {
                if q.is_empty() {
                    return true;
                }
                let d = self.draft(&c.hash).unwrap_or(c);
                [c.hash.as_str(), d.author_name.as_str(), d.author_email.as_str(), d.message.as_str()].iter().any(|s| s.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// All drafts (originals where untouched), oldest first.
    fn all_drafts(&self) -> Vec<CommitRecord> {
        self.originals.iter().map(|c| self.drafts.get(&c.hash).cloned().unwrap_or_else(|| c.clone())).collect()
    }

    /// Store every record in `all` as a draft (used after a bulk edit).
    fn store_all(&mut self, all: Vec<CommitRecord>) {
        for d in all {
            self.drafts.insert(d.hash.clone(), d);
        }
    }
}

/// A git date string shortened to `YYYY-MM-DD HH:MM` for the list; unparseable input is returned unchanged.
fn short_date(s: &str) -> String {
    match GitDate::parse(s) {
        Some(d) => format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute),
        None => s.to_string(),
    }
}

impl ApexApp {
    /// Handle `Command::GitHistoryEditor`: open the window on the workspace's
    /// repository and start loading it. Returns `false` for any other command.
    /// An untrusted repository opens the Trust dialog instead, which re-runs the command.
    pub(crate) fn history_command(&mut self, cmd: Command, arg: Option<String>) -> bool {
        if cmd != Command::GitHistoryEditor {
            return false;
        }
        // The workspace folder's repository; else the active file's.
        let repo = self.roots.iter().find_map(|r| apex_project::git::workdir_of(r)).or_else(|| self.git_target(None).map(|t| t.0));
        let Some(repo) = repo else {
            self.set_status("Open a folder inside a Git repository first".into(), false);
            return true;
        };
        if !self.trust.is_trusted(&repo) {
            self.dialog = Some(crate::app::Dialog::Trust { root: repo, then: Some((cmd, arg)) });
            return true;
        }
        let h = &mut self.git.history;
        // Switching repository resets all state; reopening the same one keeps the options.
        if h.repo != repo {
            *h = Default::default();
            h.repo = repo;
        }
        h.open = true;
        self.history_reload();
        true
    }

    /// Queue a background job that loads repository facts and commits, and
    /// clear the error. The result is picked up by `show_history_editor`.
    fn history_reload(&mut self) {
        let (tx, rx) = channel();
        self.git.history.loading = Some(rx);
        self.git.history.error = None;
        let repo = self.git.history.repo.clone();
        self.git_run("Load history", repo, move |c| {
            let r = gh::load(c);
            let n = r.as_ref().map(|(_, v)| v.len()).unwrap_or(0);
            let failed = r.as_ref().err().cloned();
            let _ = tx.send(r);
            match failed {
                Some(e) => Err(e),
                None => Ok(format!("Loaded {n} commits")),
            }
        });
    }

    /// Draw the Edit Commit History window and its modals, if open. Also
    /// consumes a finished load (replacing commits and clearing drafts) and
    /// reloads once an apply job has finished.
    pub(crate) fn show_history_editor(&mut self, ctx: &egui::Context) {
        if !self.git.history.open {
            return;
        }
        // Results of loading and applying.
        if let Some(rx) = &self.git.history.loading
            && let Ok(r) = rx.try_recv()
        {
            let h = &mut self.git.history;
            h.loading = None;
            match r {
                Ok((facts, commits)) => {
                    h.ai = gh::ai_commits(&commits);
                    h.facts = facts;
                    h.originals = commits;
                    h.drafts.clear();
                    // Keep the selection across reloads if the commit still exists; else select the newest.
                    if h.selected.as_ref().is_none_or(|s| !h.originals.iter().any(|c| &c.hash == s)) {
                        h.selected = h.originals.last().map(|c| c.hash.clone());
                    }
                }
                Err(e) => h.error = Some(e),
            }
        }
        // Git jobs run one at a time, so nothing running means the apply job has ended
        // (successfully or not); reload to show the rewritten hashes.
        if self.git.history.awaiting_apply && self.git.ops.running.is_none() {
            self.git.history.awaiting_apply = false;
            self.history_reload();
        }
        let mut open = true;
        let theme = self.theme.clone();
        egui::Window::new("Edit Commit History")
            .id(egui::Id::new("apex-history-editor"))
            .open(&mut open)
            .default_size([1100.0, 680.0])
            .max_size(ctx.content_rect().size() - vec2(16.0, 16.0))
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| self.history_contents(ui, &theme));
        if !open {
            self.git.history.open = false;
        }
        self.history_modals(ctx);
    }

    /// Window body: repository line, toolbar, then the commit list beside the edit form.
    fn history_contents(&mut self, ui: &mut Ui, theme: &crate::theme::Theme) {
        let muted = theme.ui.text_muted;
        let h = &mut self.git.history;
        // Repository line.
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(h.repo.display().to_string()).strong());
            ui.label(RichText::new(h.facts.branch.clone().map(|b| format!("on {b}")).unwrap_or_else(|| "detached HEAD".into())).color(muted));
            if h.facts.clean {
                ui.label(RichText::new("clean").color(theme.editor.git_added));
            } else {
                ui.label(RichText::new("uncommitted changes: commit or stash before applying").color(theme.ui.warning));
            }
            if let Some(o) = &h.facts.origin {
                ui.label(RichText::new(format!("origin {o}")).color(muted));
            }
            if h.loading.is_some() {
                ui.spinner();
            }
        });
        if let Some(e) = &h.error {
            ui.label(RichText::new(e).color(theme.ui.error));
        }
        // Toolbar.
        let dirty: usize = h.originals.iter().filter(|c| h.is_dirty(c)).count();
        let planned = gh::planned(&h.originals, &h.drafts, h.remove_co_authors).len();
        let mut reload = false;
        let mut apply = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut h.search).hint_text("Search hash, author, email, message").desired_width(240.0));
            ui.checkbox(&mut h.ai_only, format!("Claude/Codex only ({})", h.ai.len()))
                .on_hover_text("Commits whose author, committer or message mentions Claude, Codex or Anthropic, and merges that brought them in");
            ui.separator();
            if ui.button("Replace author…").clicked() {
                h.mode = Mode::BulkAuthor { query: String::new(), name: String::new(), email: String::new(), exact: false };
            }
            if ui.button("Rewrite dates…").clicked() {
                let today = gh::now(h.facts.local_offset);
                let d = format!("{:04}-{:02}-{:02}", today.year, today.month, today.day);
                h.mode = Mode::Dates { start: d.clone(), end: d, from: "09:00".into(), to: "17:00".into() };
            }
            ui.separator();
            ui.checkbox(&mut h.remove_co_authors, "Remove co-author lines")
                .on_hover_text("Drop Co-authored-by: trailers and Claude-Session: lines from every message; Claude/Codex attribution blocks are removed whole");
            ui.checkbox(&mut h.force_push, "Force push");
            ui.add_enabled(h.force_push, egui::Checkbox::new(&mut h.push_tags, "Push tags"));
            ui.separator();
            if ui.button("Reload").on_hover_text("Discards unapplied edits").clicked() {
                reload = true;
            }
            let label = if dirty > 0 { format!("Apply {planned} change{}…", if planned == 1 { "" } else { "s" }) } else { format!("Apply ({planned})…") };
            // With force push on, Apply is allowed with no edits, to push an earlier rewrite.
            if ui.add_enabled((planned > 0 || h.force_push) && h.loading.is_none(), egui::Button::new(label)).clicked() {
                apply = true;
            }
        });
        if reload {
            self.history_reload();
            return;
        }
        let h = &mut self.git.history;
        if apply {
            h.mode = Mode::Confirm { typed: String::new() };
        }
        ui.separator();
        // Keyboard navigation of the list when no text field has focus.
        let visible: Vec<String> = h.visible().iter().map(|c| c.hash.clone()).collect();
        if !ui.ctx().egui_wants_keyboard_input() && !visible.is_empty() && matches!(h.mode, Mode::None) {
            let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
            if up || down {
                let cur = h.selected.as_ref().and_then(|s| visible.iter().position(|v| v == s)).unwrap_or(0);
                let next = if up { cur.saturating_sub(1) } else { (cur + 1).min(visible.len() - 1) };
                h.selected = Some(visible[next].clone());
                h.scroll_to_selected = true;
            }
        }
        let avail = ui.available_size();
        let top_down = egui::Layout::top_down(egui::Align::Min);
        ui.horizontal_top(|ui| {
            let list_w = (avail.x * 0.45).max(280.0);
            ui.allocate_ui_with_layout(vec2(list_w, avail.y), top_down, |ui| {
                ui.set_min_size(vec2(list_w, avail.y));
                self.history_list(ui, theme, &visible)
            });
            ui.separator();
            ui.allocate_ui_with_layout(vec2(ui.available_width(), avail.y), top_down, |ui| self.history_form(ui, theme));
        });
    }

    /// The commit list (`visible` hashes, newest first), drawn virtualised with fixed-height rows.
    /// Clicking a row selects it; edited commits get a dot, AI-related ones a warning colour.
    fn history_list(&mut self, ui: &mut Ui, theme: &crate::theme::Theme, visible: &[String]) {
        let h = &mut self.git.history;
        let row_h = 38.0;
        let font = egui::FontId::proportional(self.settings.ui_font_size);
        let small = egui::FontId::proportional(self.settings.ui_font_size - 2.0);
        let scroll_to = std::mem::take(&mut h.scroll_to_selected).then(|| h.selected.as_ref().and_then(|s| visible.iter().position(|v| v == s))).flatten();
        let mut area = egui::ScrollArea::vertical().id_salt("history-list").auto_shrink([false, false]);
        if let Some(i) = scroll_to {
            // Last frame's scroll offset (stored below) tells whether the row is already in view;
            // only scroll, centring it, when it is not.
            let top = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("history-list-offset"))).unwrap_or(0.0);
            let y = i as f32 * row_h;
            let viewport = ui.available_height();
            if y < top || y + row_h > top + viewport {
                area = area.vertical_scroll_offset((y - viewport / 2.0).max(0.0));
            }
        }
        let out = area.show_rows(ui, row_h, visible.len(), |ui, range| {
            for i in range {
                let hash = &visible[i];
                let Some(orig) = h.originals.iter().find(|c| &c.hash == hash) else { continue };
                let dirty = h.is_dirty(orig);
                let d = h.draft(hash).cloned().unwrap_or_else(|| orig.clone());
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let selected = h.selected.as_deref() == Some(hash.as_str());
                if selected || resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, if selected { theme.ui.selection } else { theme.ui.hover });
                }
                let p = ui.painter_at(rect);
                let x = rect.min.x + 14.0;
                if dirty {
                    p.circle_filled(pos2(rect.min.x + 6.0, rect.min.y + 11.0), 3.5, theme.ui.accent);
                }
                let top = format!("{}  {}  {}", d.short_hash, short_date(&d.author_date), d.author_name);
                p.text(pos2(x, rect.min.y + 3.0), egui::Align2::LEFT_TOP, top, small.clone(), theme.ui.text_muted);
                if !orig.refs.is_empty() {
                    p.text(pos2(rect.max.x - 6.0, rect.min.y + 3.0), egui::Align2::RIGHT_TOP, orig.refs.join(", "), small.clone(), theme.ui.accent);
                }
                let colour = if h.ai.contains(hash) { theme.ui.warning } else { theme.ui.text };
                p.text(pos2(x, rect.min.y + 19.0), egui::Align2::LEFT_TOP, d.subject(), font.clone(), colour);
                if resp.clicked() {
                    h.selected = Some(hash.clone());
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("history-list-offset"), out.state.offset.y));
        if visible.is_empty() && h.loading.is_none() {
            ui.label(RichText::new("No commits match").color(theme.ui.text_muted));
        }
    }

    /// The edit form for the selected commit: author, committer, dates and message.
    /// Edits are stored as a draft only when something changed this frame.
    fn history_form(&mut self, ui: &mut Ui, theme: &crate::theme::Theme) {
        let h = &mut self.git.history;
        let Some(hash) = h.selected.clone() else {
            ui.label(RichText::new("Select a commit").color(theme.ui.text_muted));
            return;
        };
        let Some(orig) = h.originals.iter().find(|c| c.hash == hash).cloned() else { return };
        let mut d = h.drafts.get(&hash).cloned().unwrap_or_else(|| orig.clone());
        let before = d.clone();
        let mut calendar: Option<DateField> = None;
        let local_offset = h.facts.local_offset;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&orig.hash).monospace().size(self.settings.ui_font_size - 1.0));
            if orig.parents.len() > 1 {
                ui.label(RichText::new("merge").color(theme.ui.text_muted));
            }
        });
        let bad = |s: &str| GitDate::parse(s).is_none();
        egui::Grid::new("history-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for (field, name, email, date) in [
                (DateField::Author, &mut d.author_name, &mut d.author_email, &mut d.author_date),
                (DateField::Committer, &mut d.committer_name, &mut d.committer_email, &mut d.committer_date),
            ] {
                let label = if field == DateField::Author { "Author" } else { "Committer" };
                ui.label(label);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(150.0));
                    ui.add(egui::TextEdit::singleline(email).hint_text("Email").desired_width(200.0));
                });
                ui.end_row();
                ui.label(format!("{label} date"));
                ui.horizontal(|ui| {
                    let invalid = bad(date);
                    let mut edit = egui::TextEdit::singleline(date).desired_width(230.0).font(egui::TextStyle::Monospace);
                    if invalid {
                        edit = edit.text_color(theme.ui.error);
                    }
                    ui.add(edit).on_hover_text("YYYY-MM-DD HH:MM[:SS][±HH:MM]; without an offset the local time zone applies");
                    if ui.button("Calendar…").clicked() {
                        calendar = Some(field);
                    }
                    if ui.button("Now").clicked() {
                        *date = gh::now(local_offset).format();
                    }
                });
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Copy author to committer").clicked() {
                d.committer_name = d.author_name.clone();
                d.committer_email = d.author_email.clone();
                d.committer_date = d.author_date.clone();
            }
            let dirty = gh::changed(&orig, &d);
            if ui.add_enabled(dirty, egui::Button::new("Reset commit")).clicked() {
                d = orig.clone();
            }
            if dirty {
                ui.label(RichText::new("edited").color(theme.ui.accent));
            }
        });
        ui.label("Message");
        let avail = ui.available_height().max(120.0);
        egui::ScrollArea::vertical().id_salt("history-message").max_height(avail).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut d.message).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).desired_rows(12));
        });
        if d != before {
            h.drafts.insert(hash.clone(), d.clone());
        }
        if let Some(field) = calendar {
            // Start the picker at the field's current value, or now if it does not parse.
            let current = if field == DateField::Author { &d.author_date } else { &d.committer_date };
            let date = GitDate::parse(current).unwrap_or_else(|| gh::now(local_offset));
            h.mode = Mode::Calendar { field, date, view_year: date.year, view_month: date.month };
        }
    }

    /// Show the modal for the current `Mode` and act on its result: stage bulk
    /// author or date edits, set a calendar date, or queue the rewrite job
    /// (`gh::apply`, which runs `git filter-branch` and optionally force-pushes)
    /// and switch the bottom panel to the Git tab.
    fn history_modals(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let h = &mut self.git.history;
        let mut close = false;
        let mut status: Option<(String, bool)> = None;
        let mut start_apply = false;
        // Take the mode out so the modal closures can borrow `h` too; it is put back unless closed.
        let mut mode = std::mem::take(&mut h.mode);
        match &mut mode {
            Mode::None => return,
            Mode::BulkAuthor { query, name, email, exact } => {
                let r = egui::Modal::new(egui::Id::new("history-bulk")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading("Replace author");
                    ui.label("Every commit whose author name or email matches gets the new author.");
                    egui::Grid::new("bulk-grid").num_columns(2).show(ui, |ui| {
                        ui.label("Match");
                        ui.text_edit_singleline(query);
                        ui.end_row();
                        ui.label("New name");
                        ui.text_edit_singleline(name);
                        ui.end_row();
                        ui.label("New email");
                        ui.text_edit_singleline(email);
                        ui.end_row();
                    });
                    ui.checkbox(exact, "Exact match (otherwise any part, ignoring case)");
                    ui.horizontal(|ui| (ui.button("Replace").clicked(), ui.button("Cancel").clicked())).inner
                });
                let (ok, cancel) = r.inner;
                if ok {
                    let (q, n, e, x) = (query.clone(), name.clone(), email.clone(), *exact);
                    let mut all = h.all_drafts();
                    match gh::replace_author(&mut all, &q, &n, &e, x) {
                        Ok(count) => {
                            h.store_all(all);
                            status = Some((format!("Replaced the author of {count} commit{}", if count == 1 { "" } else { "s" }), false));
                            close = true;
                        }
                        Err(e) => status = Some((e, true)),
                    }
                }
                close |= cancel || r.should_close();
            }
            Mode::Dates { start, end, from, to } => {
                let n = h.originals.len();
                let r = egui::Modal::new(egui::Id::new("history-dates")).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.heading("Rewrite all dates");
                    ui.label(format!(
                        "Every one of the {n} commits gets a random author and committer time on a day between the two dates, inside the daily window, in local time. Oldest commits get the earliest times, so the order is kept."
                    ));
                    egui::Grid::new("dates-grid").num_columns(2).show(ui, |ui| {
                        ui.label("From day");
                        ui.add(egui::TextEdit::singleline(start).hint_text("YYYY-MM-DD"));
                        ui.end_row();
                        ui.label("To day");
                        ui.add(egui::TextEdit::singleline(end).hint_text("YYYY-MM-DD"));
                        ui.end_row();
                        ui.label("Daily window");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(from).desired_width(60.0).hint_text("08:00"));
                            ui.label("to");
                            ui.add(egui::TextEdit::singleline(to).desired_width(60.0).hint_text("12:00"));
                        });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| (ui.button("Rewrite dates").clicked(), ui.button("Cancel").clicked())).inner
                });
                let (ok, cancel) = r.inner;
                if ok {
                    let (s, e, f, t) = (start.clone(), end.clone(), from.clone(), to.clone());
                    let mut all = h.all_drafts();
                    match gh::spread_dates(&mut all, &s, &e, &f, &t, |n| fastrand::u64(0..n)) {
                        Ok(count) => {
                            h.store_all(all);
                            status = Some((format!("New dates staged for {count} commits"), false));
                            close = true;
                        }
                        Err(e) => status = Some((e, true)),
                    }
                }
                close |= cancel || r.should_close();
            }
            Mode::Calendar { field, date, view_year, view_month } => {
                let r = egui::Modal::new(egui::Id::new("history-calendar")).show(ctx, |ui| calendar_ui(ui, date, view_year, view_month, &theme));
                match r.inner {
                    Some(true) => {
                        let value = date.format();
                        if let Some(hash) = h.selected.clone()
                            && let Some(orig) = h.originals.iter().find(|c| c.hash == hash).cloned()
                        {
                            let d = h.drafts.entry(hash).or_insert(orig);
                            match field {
                                DateField::Author => d.author_date = value,
                                DateField::Committer => d.committer_date = value,
                            }
                        }
                        close = true;
                    }
                    Some(false) => close = true,
                    None => close |= r.should_close(),
                }
            }
            Mode::Confirm { typed } => {
                let changes = gh::planned(&h.originals, &h.drafts, h.remove_co_authors);
                let removed: usize = if h.remove_co_authors { h.all_drafts().iter().map(|d| gh::remove_co_authors(&d.message).1).sum() } else { 0 };
                let force = h.force_push;
                let r = egui::Modal::new(egui::Id::new("history-confirm")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.heading("Rewrite history?");
                    ui.label(format!(
                        "{} commit{} will be rewritten; every descendant gets a new hash. A backup tag (apex-backup-…) is created first. Old objects are then pruned, so the tag is the way back.",
                        changes.len(),
                        if changes.len() == 1 { "" } else { "s" }
                    ));
                    if removed > 0 {
                        ui.label(format!("{removed} co-author/session line{} will be removed.", if removed == 1 { "" } else { "s" }));
                    }
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for c in changes.iter().rev().take(200) {
                            ui.label(RichText::new(format!("{}  {}", c.short_hash, c.subject())).monospace());
                        }
                    });
                    if !h.facts.clean {
                        ui.label(RichText::new("The repository has uncommitted changes; the rewrite will refuse to run.").color(theme.ui.warning));
                    }
                    if force {
                        ui.label(RichText::new(format!("Then force-push all branches{} to origin (--force-with-lease). Others who pulled must re-clone or reset.", if h.push_tags { " and tags" } else { "" })).color(theme.ui.warning));
                        ui.horizontal(|ui| {
                            ui.label("Type FORCE to confirm:");
                            ui.text_edit_singleline(typed);
                        });
                    }
                    // Force-pushing rewritten history affects others, so it needs a typed confirmation.
                    let can = !force || typed.trim() == "FORCE";
                    // Enter confirms too (it is also how FORCE is submitted).
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let (ok, cancel) = ui.horizontal(|ui| (ui.add_enabled(can, egui::Button::new("Rewrite")).clicked(), ui.button("Cancel").clicked())).inner;
                    (ok || (enter && can), cancel)
                });
                let (ok, cancel) = r.inner;
                if ok {
                    start_apply = true;
                    close = true;
                }
                close |= cancel || r.should_close();
            }
        }
        if !close {
            h.mode = mode;
        }
        if start_apply {
            let changes = gh::planned(&h.originals, &h.drafts, h.remove_co_authors);
            let originals = h.originals.clone();
            let opts = gh::ApplyOptions { remove_co_authors: h.remove_co_authors, force_push: h.force_push, push_tags: h.push_tags };
            let repo = h.repo.clone();
            h.awaiting_apply = true;
            self.git_run("Rewrite history", repo, move |c| gh::apply(c, &originals, &changes, &opts));
            use crate::tasks_ui::PanelTab;
            self.panel_open = true;
            self.panel_tab = PanelTab::Git;
        }
        if let Some((s, err)) = status {
            self.set_status(s, err);
        }
    }
}

/// Month grid and time fields. Some(true) = OK, Some(false) = cancel.
/// Edits `date` in place; `year` and `month` are the month on show.
/// `None` while the user has not pressed either button.
fn calendar_ui(ui: &mut Ui, date: &mut GitDate, year: &mut i64, month: &mut u32, theme: &crate::theme::Theme) -> Option<bool> {
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    ui.set_width(320.0);
    ui.horizontal(|ui| {
        if ui.small_button("<<").on_hover_text("Previous year").clicked() {
            *year -= 1;
        }
        if ui.small_button("<").on_hover_text("Previous month").clicked() {
            if *month == 1 {
                *month = 12;
                *year -= 1;
            } else {
                *month -= 1;
            }
        }
        ui.label(RichText::new(format!("{} {}", MONTHS[(*month as usize).clamp(1, 12) - 1], year)).strong());
        if ui.small_button(">").on_hover_text("Next month").clicked() {
            if *month == 12 {
                *month = 1;
                *year += 1;
            } else {
                *month += 1;
            }
        }
        if ui.small_button(">>").on_hover_text("Next year").clicked() {
            *year += 1;
        }
    });
    // Monday-first grid; 1970-01-01 was a Thursday.
    let first = gh::days_from_civil(*year, *month, 1);
    let lead = (first + 3).rem_euclid(7) as u32;
    let days = gh::days_in_month(*year, *month);
    egui::Grid::new("calendar").spacing([4.0, 4.0]).show(ui, |ui| {
        for d in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
            ui.label(RichText::new(d).color(theme.ui.text_muted));
        }
        ui.end_row();
        let mut col = 0;
        for _ in 0..lead {
            ui.label("");
            col += 1;
        }
        for day in 1..=days {
            let selected = date.year == *year && date.month == *month && date.day == day;
            let b = egui::Button::new(format!("{day:>2}")).min_size(vec2(32.0, 24.0)).fill(if selected { theme.ui.accent } else { Color32::TRANSPARENT });
            if ui.add(b).clicked() {
                date.year = *year;
                date.month = *month;
                date.day = day;
            }
            col += 1;
            if col == 7 {
                ui.end_row();
                col = 0;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Time");
        ui.add(egui::DragValue::new(&mut date.hour).range(0..=23).custom_formatter(|v, _| format!("{v:02}")));
        ui.label(":");
        ui.add(egui::DragValue::new(&mut date.minute).range(0..=59).custom_formatter(|v, _| format!("{v:02}")));
        ui.label(":");
        ui.add(egui::DragValue::new(&mut date.second).range(0..=59).custom_formatter(|v, _| format!("{v:02}")));
    });
    ui.horizontal(|ui| {
        let mut local = date.offset.is_none();
        if ui.checkbox(&mut local, "Local time zone").on_hover_text("Otherwise the offset below is kept").changed() {
            date.offset = if local { None } else { Some(0) };
        }
        if let Some(o) = &mut date.offset {
            ui.label("UTC offset (minutes)");
            ui.add(egui::DragValue::new(o).range(-14 * 60..=14 * 60).speed(15));
        }
    });
    ui.label(RichText::new(date.format()).monospace());
    ui.horizontal(|ui| {
        if ui.button("OK").clicked() {
            return Some(true);
        }
        if ui.button("Cancel").clicked() {
            return Some(false);
        }
        None
    })
    .inner
}
