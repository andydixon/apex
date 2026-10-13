//! The Find in Files panel: inputs, streaming results and replace preview.
//!
//! `FindInFiles` is the panel's state, owned by `ApexApp` as `app.fif`. The
//! search itself runs on a worker thread in `apex_project::search`; `start`
//! launches it and `poll` (called every frame from `poll_background`) drains
//! its events into `results`. `ui` draws the panel and returns `FifAction`s for
//! the app to act on, because the panel has no access to documents or panes.
//!
//! Gotcha: pressing Find does not start the search here. `ui` emits a
//! `FifAction::Open` with an empty path as a sentinel, and
//! `ApexApp::show_bottom_panel` turns that into `start` with the current
//! workspace roots and settings. Replace-all is likewise confirmed and applied
//! by the app (`apply_replace_in_files`), not by this module.
//!
//! Match ranges from the search engine are byte offsets within the line text;
//! the app converts them to char columns when opening a result.

use std::collections::HashSet;
use std::path::PathBuf;

use apex_project::search::{FileMatches, SearchEvent, SearchHandle, SearchRequest, SearchStats};
use egui::Ui;

use crate::theme::Theme;

/// Something the panel wants the application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum FifAction {
    /// Open `path` at a match. `line` is zero-based, `col` is a byte offset in
    /// the line. `neighbour` (Ctrl/Cmd-click) opens in the next pane, splitting
    /// if there is only one. An empty `path` is the "start search" sentinel.
    Open { path: PathBuf, line: usize, col: usize, neighbour: bool },
    /// Apply the replacement to every file not excluded.
    /// Currently never emitted; the app reads `confirm_replace` instead.
    ApplyReplace { files: Vec<PathBuf>, replacement: String },
}

/// State of the Find in Files panel. All fields are read and written by the
/// app as well as by `ui`, so they are public.
#[derive(Default)]
pub struct FindInFiles {
    /// Set to true by the app whenever the Find tab is opened; not read by the
    /// panel itself (the bottom panel's visibility lives in `ApexApp::panel_open`).
    pub open: bool,
    /// Search text (literal or regex, depending on `regex`).
    pub query: String,
    /// Replacement text; `$1` style captures when `regex` is on.
    pub replacement: String,
    /// Comma-separated include globs typed by the user.
    pub include: String,
    /// Comma-separated extra exclude globs typed by the user.
    pub exclude: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Include dot-files.
    pub search_hidden: bool,
    /// Ignore `.gitignore` and the settings' exclude patterns.
    pub search_ignored: bool,
    /// Whether the replacement row is visible.
    pub show_replace: bool,
    /// Files with matches, sorted by path once the search finishes.
    pub results: Vec<FileMatches>,
    /// Files the user has struck out of the replacement (toggle per file).
    pub excluded: HashSet<PathBuf>,
    /// Summary from the finished search; `None` while running or before any search.
    pub stats: Option<SearchStats>,
    /// Error from the last `start` or from the search thread.
    pub error: Option<String>,
    /// Handle of the search in progress. Dropping it cancels the worker
    /// (`SearchHandle` implements `Drop`), so `running = None` is enough to stop it.
    pub running: Option<SearchHandle>,
    /// One-shot request to focus the query box on the next frame.
    pub focus_query: bool,
    /// Files whose line list is folded in the results view.
    pub collapsed: HashSet<PathBuf>,
    /// Folders to search instead of the workspace roots (sidebar "Find in Folder").
    pub where_: Vec<PathBuf>,
    /// Set by the Replace button; the app clears it and shows the confirmation dialog.
    pub confirm_replace: bool,
    /// Recent queries, most recent first, saved with the session.
    pub history: Vec<String>,
}

impl FindInFiles {
    /// Reset the result state and launch a search over `roots` (or `where_` if
    /// set) using the current inputs. `exclude_defaults` are the settings'
    /// patterns, dropped when `search_ignored` is on. A failed launch or an empty
    /// query leaves `running` as `None`. Also records the query in `history`.
    pub fn start(&mut self, roots: Vec<PathBuf>, exclude_defaults: &[String], follow_symlinks: bool) {
        // Dropping the old handle cancels any search still running.
        self.running = None;
        self.results.clear();
        self.excluded.clear();
        self.collapsed.clear();
        self.stats = None;
        self.error = None;
        if self.query.is_empty() {
            return;
        }
        let roots = if self.where_.is_empty() { roots } else { self.where_.clone() };
        if roots.is_empty() {
            self.error = Some("Open a folder first (File ▸ Open Folder…)".into());
            return;
        }
        let mut exclude: Vec<String> = if self.search_ignored { Vec::new() } else { exclude_defaults.to_vec() };
        exclude.extend(self.exclude.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        let include: Vec<String> = self.include.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let req = SearchRequest {
            roots,
            pattern: self.query.clone(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            include,
            exclude,
            hidden: self.search_hidden,
            respect_gitignore: !self.search_ignored,
            follow_symlinks,
            // 20 MiB per file and 20k matches keep memory and the row list bounded.
            max_file_bytes: 20 << 20,
            max_results: 20_000,
            context_lines: 0,
        };
        match apex_project::search::start(req) {
            Ok(h) => self.running = Some(h),
            Err(e) => self.error = Some(e),
        }
        // Move-to-front history, capped so the session file stays small.
        self.history.retain(|h| *h != self.query);
        self.history.insert(0, self.query.clone());
        self.history.truncate(20);
    }

    /// Drain search events. Returns true if the panel needs a repaint soon.
    ///
    /// Takes at most 256 events per call so a huge result set cannot stall a
    /// frame. On `Done` the results are sorted by path (the worker reports files
    /// in walk order, which is not stable across runs).
    pub fn poll(&mut self) -> bool {
        let Some(h) = &self.running else { return false };
        let mut got = false;
        for _ in 0..256 {
            match h.rx.try_recv() {
                Ok(SearchEvent::File(f)) => {
                    self.results.push(f);
                    got = true;
                }
                Ok(SearchEvent::Done(s)) => {
                    self.stats = Some(s);
                    self.running = None;
                    self.results.sort_by(|a, b| a.path.cmp(&b.path));
                    return true;
                }
                Ok(SearchEvent::Error(e)) => {
                    self.error = Some(e);
                    self.running = None;
                    return true;
                }
                Err(_) => break,
            }
        }
        got || self.running.is_some()
    }

    /// Stop the running search, if any. Results received so far are kept.
    pub fn cancel(&mut self) {
        if let Some(h) = self.running.take() {
            h.cancel();
        }
    }

    /// Number of match ranges across all files not excluded from Replace.
    pub fn total_matches(&self) -> usize {
        self.results.iter().filter(|f| !self.excluded.contains(&f.path)).map(|f| f.lines.iter().map(|l| l.ranges.len()).sum::<usize>()).sum()
    }

    /// Draw the panel: input rows, status line and the virtualised result list.
    /// `roots` are only used to shorten displayed paths and for the empty-state
    /// hint. Returns the actions the app should perform this frame.
    pub fn ui(&mut self, ui: &mut Ui, theme: &Theme, ui_font: f32, roots: &[PathBuf]) -> Vec<FifAction> {
        let mut actions = Vec::new();
        let mono = egui::FontId::monospace(ui_font);
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new("apex-fif-query"))
                    .hint_text("Find in files")
                    .desired_width(260.0)
                    // Keep Tab from moving focus out of the box.
                    .lock_focus(true),
            );
            if self.focus_query {
                r.request_focus();
                self.focus_query = false;
            }
            // egui's TextEdit drops focus on Enter, so "lost focus + Enter pressed"
            // is the idiom for "the user submitted the field".
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let toggle = |ui: &mut Ui, on: &mut bool, label: &str, tip: &str| {
                if ui.selectable_label(*on, label).on_hover_text(tip).clicked() {
                    *on = !*on;
                }
            };
            toggle(ui, &mut self.regex, ".*", "Regular expression");
            toggle(ui, &mut self.case_sensitive, "Aa", "Case sensitive");
            toggle(ui, &mut self.whole_word, "Word", "Whole word");
            toggle(ui, &mut self.search_hidden, "Hidden", "Include hidden files");
            toggle(ui, &mut self.search_ignored, "Ignored", "Include files excluded by .gitignore and settings");
            if ui.button("Find").clicked() || enter {
                actions.push(FifAction::Open { path: PathBuf::new(), line: 0, col: 0, neighbour: false }); // sentinel: start search
            }
            if self.running.is_some() && ui.button("Cancel").clicked() {
                self.cancel();
            }
            if ui.selectable_label(self.show_replace, "Replace").clicked() {
                self.show_replace = !self.show_replace;
            }
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Include").color(theme.ui.text_muted));
            ui.add(egui::TextEdit::singleline(&mut self.include).hint_text("*.rs, src/**").desired_width(160.0));
            ui.label(egui::RichText::new("Exclude").color(theme.ui.text_muted));
            ui.add(egui::TextEdit::singleline(&mut self.exclude).hint_text("**/vendor").desired_width(160.0));
            if !self.where_.is_empty() {
                ui.label(
                    egui::RichText::new(format!("in {}", self.where_.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")))
                        .color(theme.ui.text_muted),
                );
                if ui.small_button("×").on_hover_text("Search all folders").clicked() {
                    self.where_.clear();
                }
            }
        });
        if self.show_replace {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.replacement).hint_text("Replace with").desired_width(260.0));
                let n = self.total_matches();
                let files = self.results.iter().filter(|f| !self.excluded.contains(&f.path)).count();
                // Replacing while the result list is still growing would act on a
                // partial set, so wait for the search to finish.
                let enabled = n > 0 && self.running.is_none();
                if ui.add_enabled(enabled, egui::Button::new(format!("Replace {n} in {files} files…"))).clicked() {
                    self.confirm_replace = true;
                }
                if self.regex {
                    ui.label(egui::RichText::new("$1 … inserts capture groups").color(theme.ui.text_muted));
                }
            });
        }
        // Status line.
        ui.horizontal(|ui| {
            if let Some(e) = &self.error {
                ui.label(egui::RichText::new(e).color(theme.ui.error));
            } else if self.running.is_some() {
                let scanned = self.running.as_ref().map(|h| h.scanned.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0);
                ui.spinner();
                ui.label(egui::RichText::new(format!("Searching… {scanned} files, {} matches", self.total_matches())).color(theme.ui.text_muted));
            } else if let Some(s) = &self.stats {
                let mut t =
                    format!("{} matches in {} files ({} files scanned in {:.2}s)", s.matches, s.files_matched, s.files_scanned, s.elapsed.as_secs_f32());
                if s.skipped_binary + s.skipped_large + s.unreadable > 0 {
                    t.push_str(&format!("; skipped {} binary, {} large, {} unreadable", s.skipped_binary, s.skipped_large, s.unreadable));
                }
                if s.truncated {
                    t.push_str("; results truncated");
                }
                ui.label(egui::RichText::new(t).color(theme.ui.text_muted));
            } else if roots.is_empty() {
                ui.label(egui::RichText::new("Open a folder to search in it").color(theme.ui.text_muted));
            }
        });
        ui.separator();
        let row_h = ui_font + 6.0;
        // Flatten files and their (unfolded) lines into one row list so a single
        // virtualised ScrollArea can lay out only the visible rows.
        let mut rows: Vec<Row> = Vec::new();
        for (fi, f) in self.results.iter().enumerate() {
            rows.push(Row::File(fi));
            if !self.collapsed.contains(&f.path) {
                for (li, _) in f.lines.iter().enumerate() {
                    rows.push(Row::Line(fi, li));
                }
            }
        }
        // Toggles are deferred to after the closure: it borrows `self.results`
        // immutably while drawing, so the sets cannot be mutated inside it.
        let mut toggle_collapse: Option<PathBuf> = None;
        let mut toggle_exclude: Option<PathBuf> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
            for r in range {
                match rows[r] {
                    Row::File(fi) => {
                        let f = &self.results[fi];
                        let excluded = self.excluded.contains(&f.path);
                        ui.horizontal(|ui| {
                            let collapsed = self.collapsed.contains(&f.path);
                            if ui.small_button(if collapsed { "+" } else { "-" }).clicked() {
                                toggle_collapse = Some(f.path.clone());
                            }
                            let n: usize = f.lines.iter().map(|l| l.ranges.len()).sum();
                            let rel = roots
                                .iter()
                                .find_map(|r| f.path.strip_prefix(r).ok())
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| f.path.display().to_string());
                            // Excluded files are shown struck through and muted.
                            let label = egui::RichText::new(format!("{rel}  ({n})"))
                                .color(if excluded { theme.ui.text_muted } else { theme.ui.accent })
                                .strikethrough();
                            let label = if excluded { label } else { egui::RichText::new(format!("{rel}  ({n})")).color(theme.ui.accent) };
                            if ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked() {
                                actions.push(FifAction::Open {
                                    path: f.path.clone(),
                                    line: f.lines.first().map(|l| l.line).unwrap_or(0),
                                    col: 0,
                                    neighbour: false,
                                });
                            }
                            if self.show_replace
                                && ui.small_button(if excluded { "include" } else { "exclude" }).on_hover_text("Exclude this file from Replace").clicked()
                            {
                                toggle_exclude = Some(f.path.clone());
                            }
                        });
                    }
                    Row::Line(fi, li) => {
                        let f = &self.results[fi];
                        let l = &f.lines[li];
                        // Painted by hand rather than with widgets: one allocation per
                        // row keeps thousands of result rows cheap.
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, theme.ui.hover);
                        }
                        let num = format!("{:>5}: ", l.line + 1);
                        let mut job = egui::text::LayoutJob::default();
                        job.append(&num, 0.0, egui::TextFormat { font_id: mono.clone(), color: theme.ui.text_muted, ..Default::default() });
                        // Leading indentation is dropped from the display, so the match
                        // byte ranges have to be shifted left by the same amount.
                        let text = l.text.trim_start();
                        let trimmed = l.text.len() - text.len();
                        let mut cursor = 0usize;
                        for &(s, e) in &l.ranges {
                            let (s, e) = (s.saturating_sub(trimmed).min(text.len()), e.saturating_sub(trimmed).min(text.len()));
                            if s > cursor {
                                job.append(&text[cursor..s], 0.0, egui::TextFormat { font_id: mono.clone(), color: theme.ui.text, ..Default::default() });
                            }
                            if e > s {
                                job.append(
                                    &text[s..e],
                                    0.0,
                                    egui::TextFormat {
                                        font_id: mono.clone(),
                                        color: theme.ui.text,
                                        background: theme.editor.search_match,
                                        ..Default::default()
                                    },
                                );
                                cursor = e;
                            }
                        }
                        if cursor < text.len() {
                            job.append(&text[cursor..], 0.0, egui::TextFormat { font_id: mono.clone(), color: theme.ui.text, ..Default::default() });
                        }
                        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
                        let galley = ui.fonts_mut(|fo| fo.layout_job(job));
                        ui.painter().galley(egui::pos2(rect.min.x + 24.0, rect.center().y - galley.rect.height() / 2.0), galley, theme.ui.text);
                        if resp.clicked() {
                            let col = l.ranges.first().map(|r| r.0).unwrap_or(0);
                            let neighbour = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                            actions.push(FifAction::Open { path: f.path.clone(), line: l.line, col, neighbour });
                        }
                    }
                }
            }
        });
        // `remove` returning false means the path was not in the set: insert it.
        if let Some(p) = toggle_collapse
            && !self.collapsed.remove(&p)
        {
            self.collapsed.insert(p);
        }
        if let Some(p) = toggle_exclude
            && !self.excluded.remove(&p)
        {
            self.excluded.insert(p);
        }
        actions
    }
}

/// One row of the flattened result list: a file header or a matching line
/// (indices into `results` and `results[fi].lines`).
enum Row {
    File(usize),
    Line(usize, usize),
}
