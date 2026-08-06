//! The Find in Files panel: inputs, streaming results and replace preview.

use std::collections::HashSet;
use std::path::PathBuf;

use apex_project::search::{FileMatches, SearchEvent, SearchHandle, SearchRequest, SearchStats};
use egui::Ui;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq)]
pub enum FifAction {
    Open {
        path: PathBuf,
        line: usize,
        col: usize,
        neighbour: bool,
    },
    /// Apply the replacement to every file not excluded.
    ApplyReplace {
        files: Vec<PathBuf>,
        replacement: String,
    },
}

#[derive(Default)]
pub struct FindInFiles {
    pub open: bool,
    pub query: String,
    pub replacement: String,
    pub include: String,
    pub exclude: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub search_hidden: bool,
    pub search_ignored: bool,
    pub show_replace: bool,
    pub results: Vec<FileMatches>,
    pub excluded: HashSet<PathBuf>,
    pub stats: Option<SearchStats>,
    pub error: Option<String>,
    pub running: Option<SearchHandle>,
    pub focus_query: bool,
    pub collapsed: HashSet<PathBuf>,
    pub where_: Vec<PathBuf>,
    pub confirm_replace: bool,
    pub history: Vec<String>,
}

impl FindInFiles {
    pub fn start(&mut self, roots: Vec<PathBuf>, exclude_defaults: &[String], follow_symlinks: bool) {
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
            max_file_bytes: 20 << 20,
            max_results: 20_000,
            context_lines: 0,
        };
        match apex_project::search::start(req) {
            Ok(h) => self.running = Some(h),
            Err(e) => self.error = Some(e),
        }
        self.history.retain(|h| *h != self.query);
        self.history.insert(0, self.query.clone());
        self.history.truncate(20);
    }

    /// Drain search events. Returns true if the panel needs a repaint soon.
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

    pub fn cancel(&mut self) {
        if let Some(h) = self.running.take() {
            h.cancel();
        }
    }

    pub fn total_matches(&self) -> usize {
        self.results.iter().filter(|f| !self.excluded.contains(&f.path)).map(|f| f.lines.iter().map(|l| l.ranges.len()).sum::<usize>()).sum()
    }

    pub fn ui(&mut self, ui: &mut Ui, theme: &Theme, ui_font: f32, roots: &[PathBuf]) -> Vec<FifAction> {
        let mut actions = Vec::new();
        let mono = egui::FontId::monospace(ui_font);
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new("apex-fif-query"))
                    .hint_text("Find in files")
                    .desired_width(260.0)
                    .lock_focus(true),
            );
            if self.focus_query {
                r.request_focus();
                self.focus_query = false;
            }
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
        let mut rows: Vec<Row> = Vec::new();
        for (fi, f) in self.results.iter().enumerate() {
            rows.push(Row::File(fi));
            if !self.collapsed.contains(&f.path) {
                for (li, _) in f.lines.iter().enumerate() {
                    rows.push(Row::Line(fi, li));
                }
            }
        }
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
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, theme.ui.hover);
                        }
                        let num = format!("{:>5}: ", l.line + 1);
                        let mut job = egui::text::LayoutJob::default();
                        job.append(&num, 0.0, egui::TextFormat { font_id: mono.clone(), color: theme.ui.text_muted, ..Default::default() });
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

enum Row {
    File(usize),
    Line(usize, usize),
}
