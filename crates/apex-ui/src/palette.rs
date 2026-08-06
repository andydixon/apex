//! The command palette and Goto Anything overlay: a fuzzy picker over
//! application-provided items.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::command::Command;

/// What the palette is picking.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteMode {
    Commands,
    /// Goto Anything: files, with `:line`, `@symbol`, `#text` operators.
    Anything,
    Symbols,
    ProjectSymbols,
    Themes,
    Languages,
    Encodings,
    LineEndings,
    RecentFiles,
    RecentProjects,
    ClipboardHistory,
    Recovery,
    BuildTasks,
    Macros,
    /// Free text input (go to line, rename, etc.).
    Prompt(String),
}

/// One selectable row.
#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: String,
    pub detail: String,
    pub shortcut: String,
    /// Opaque payload returned on selection.
    pub payload: Payload,
    /// Sort weight boost (recently used etc.).
    pub boost: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Command(Command, Option<String>),
    File(std::path::PathBuf, Option<usize>),
    Line(usize, Option<usize>),
    Symbol { line: usize, byte: usize, path: Option<std::path::PathBuf> },
    Text(String),
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct ScoredItem {
    pub index: usize,
    pub score: u32,
    pub indices: Vec<u32>,
}

pub struct Palette {
    pub open: bool,
    pub mode: PaletteMode,
    pub query: String,
    pub items: Vec<PaletteItem>,
    pub results: Vec<ScoredItem>,
    pub selected: usize,
    pub focus_requested: bool,
    pub placeholder: String,
    /// The view/scroll state to restore if the overlay is cancelled.
    pub restore: Option<(crate::view::ViewId, apex_edit::SelectionSet, egui::Vec2)>,
    last_query: Option<String>,
    matcher: Matcher,
    pub max_rows: usize,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            open: false,
            mode: PaletteMode::Commands,
            query: String::new(),
            items: Vec::new(),
            results: Vec::new(),
            selected: 0,
            focus_requested: false,
            placeholder: String::new(),
            restore: None,
            last_query: None,
            matcher: Matcher::new(Config::DEFAULT),
            max_rows: 12,
        }
    }
}

/// What the overlay produced this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteEvent {
    None,
    /// Selection moved (for live preview).
    Highlight(Payload),
    Accept(Payload, String),
    /// Accepted with no matching item (free text).
    AcceptText(String),
    Cancel,
}

impl Palette {
    pub fn show_with(&mut self, mode: PaletteMode, items: Vec<PaletteItem>, placeholder: &str, initial: &str) {
        self.open = true;
        self.mode = mode;
        self.items = items;
        self.query = initial.to_owned();
        self.placeholder = placeholder.to_owned();
        self.selected = 0;
        self.focus_requested = true;
        self.last_query = None;
        self.rescore();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.results.clear();
        self.restore = None;
    }

    /// The part of the query used for fuzzy matching (operators stripped).
    pub fn match_text(&self) -> &str {
        match self.mode {
            PaletteMode::Anything => {
                let q = self.query.as_str();
                // `file:line`, `:line`, `@symbol`, `#text` → file part only here.
                let end = q.find([':', '@', '#']).unwrap_or(q.len());
                &q[..end]
            }
            _ => &self.query,
        }
    }

    /// Operator suffix of a Goto Anything query, if any.
    pub fn operator(&self) -> Option<(char, &str)> {
        if self.mode != PaletteMode::Anything {
            return None;
        }
        let q = self.query.as_str();
        let i = q.find([':', '@', '#'])?;
        Some((q.as_bytes()[i] as char, &q[i + 1..]))
    }

    pub fn rescore(&mut self) {
        let text = self.match_text().to_owned();
        if self.last_query.as_deref() == Some(text.as_str()) && !self.results.is_empty() {
            return;
        }
        self.last_query = Some(text.clone());
        let mut out: Vec<ScoredItem> = Vec::with_capacity(self.items.len().min(2000));
        if text.trim().is_empty() {
            let mut idx: Vec<usize> = (0..self.items.len()).collect();
            idx.sort_by_key(|&i| std::cmp::Reverse(self.items[i].boost));
            out.extend(idx.into_iter().take(500).map(|i| ScoredItem { index: i, score: self.items[i].boost, indices: Vec::new() }));
        } else {
            let pattern = Pattern::parse(&text, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut indices = Vec::new();
            for (i, item) in self.items.iter().enumerate() {
                let hay = Utf32Str::new(&item.label, &mut buf);
                indices.clear();
                if let Some(score) = pattern.indices(hay, &mut self.matcher, &mut indices) {
                    indices.sort_unstable();
                    indices.dedup();
                    out.push(ScoredItem { index: i, score: score + item.boost, indices: indices.clone() });
                } else if !item.detail.is_empty() {
                    // Also match against the detail (path directory etc.) at a penalty.
                    let hay = Utf32Str::new(&item.detail, &mut buf);
                    if let Some(score) = pattern.score(hay, &mut self.matcher) {
                        out.push(ScoredItem { index: i, score: score / 2 + item.boost, indices: Vec::new() });
                    }
                }
            }
            out.sort_by(|a, b| b.score.cmp(&a.score).then(self.items[a.index].label.len().cmp(&self.items[b.index].label.len())));
            out.truncate(500);
        }
        self.results = out;
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    pub fn selected_payload(&self) -> Option<&Payload> {
        self.results.get(self.selected).map(|r| &self.items[r.index].payload)
    }

    /// Draw the overlay. Keyboard handling is done here because the text
    /// field owns focus.
    pub fn ui(&mut self, ctx: &egui::Context, theme: &crate::theme::Theme, ui_font: f32) -> PaletteEvent {
        if !self.open {
            return PaletteEvent::None;
        }
        let mut event = PaletteEvent::None;
        let prev_selected = self.selected;
        // Keys first, so the text edit does not consume them.
        let (up, down, enter, escape, page_up, page_down, tab) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::P)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::K),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::N)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::J),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
            )
        });
        let width = (ctx.content_rect().width() * 0.6).clamp(360.0, 760.0);
        let area = egui::Area::new(egui::Id::new("apex-palette"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .interactable(true);
        area.show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme.ui.surface_raised)
                .stroke(egui::Stroke::new(1.0, theme.ui.border))
                .corner_radius(8.0)
                .inner_margin(8.0)
                .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: egui::Color32::from_black_alpha(if theme.dark { 160 } else { 60 }) })
                .show(ui, |ui| {
                    ui.set_width(width);
                    let font = egui::FontId::proportional(ui_font + 1.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .id(egui::Id::new("apex-palette-input"))
                            .font(font.clone())
                            .hint_text(self.placeholder.clone())
                            .desired_width(f32::INFINITY)
                            .lock_focus(true)
                            .frame(egui::Frame::NONE),
                    );
                    if self.focus_requested {
                        resp.request_focus();
                        self.focus_requested = false;
                    }
                    if resp.changed() {
                        self.selected = 0;
                    }
                    self.rescore();
                    ui.add_space(4.0);
                    ui.separator();
                    let row_h = ui_font + 12.0;
                    let n = self.results.len().min(self.max_rows.max(1) * 4);
                    if up {
                        self.selected = if self.selected == 0 { self.results.len().saturating_sub(1) } else { self.selected - 1 };
                    }
                    if down || tab {
                        self.selected = if self.results.is_empty() { 0 } else { (self.selected + 1) % self.results.len() };
                    }
                    if page_up {
                        self.selected = self.selected.saturating_sub(self.max_rows);
                    }
                    if page_down {
                        self.selected = (self.selected + self.max_rows).min(self.results.len().saturating_sub(1));
                    }
                    let mut clicked: Option<usize> = None;
                    let scroll_h = row_h * self.max_rows as f32;
                    egui::ScrollArea::vertical().max_height(scroll_h).auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
                        for row in range {
                            let r = &self.results[row];
                            let item = &self.items[r.index];
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                            let selected = row == self.selected;
                            if selected {
                                ui.painter().rect_filled(rect, 4.0, theme.ui.selection);
                                if prev_selected != self.selected {
                                    ui.scroll_to_rect(rect, None);
                                }
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 4.0, theme.ui.hover);
                            }
                            // Label with matched characters emphasised.
                            let mut job = egui::text::LayoutJob::default();
                            let normal = egui::TextFormat { font_id: font.clone(), color: theme.ui.text, ..Default::default() };
                            let hit = egui::TextFormat {
                                font_id: font.clone(),
                                color: theme.ui.accent,
                                underline: egui::Stroke::new(1.0, theme.ui.accent),
                                ..Default::default()
                            };
                            let mut hit_iter = r.indices.iter().peekable();
                            let mut run = String::new();
                            let mut run_hit = false;
                            for (ci, c) in item.label.chars().enumerate() {
                                let is_hit = hit_iter.peek().is_some_and(|&&h| h as usize == ci);
                                if is_hit {
                                    hit_iter.next();
                                }
                                if is_hit != run_hit && !run.is_empty() {
                                    job.append(&run, 0.0, if run_hit { hit.clone() } else { normal.clone() });
                                    run.clear();
                                }
                                run_hit = is_hit;
                                run.push(c);
                            }
                            if !run.is_empty() {
                                job.append(&run, 0.0, if run_hit { hit.clone() } else { normal.clone() });
                            }
                            let galley = ui.fonts_mut(|f| f.layout_job(job));
                            let text_pos = egui::pos2(rect.min.x + 10.0, rect.center().y - galley.rect.height() / 2.0);
                            ui.painter().galley(text_pos, galley.clone(), theme.ui.text);
                            let mut x = text_pos.x + galley.rect.width() + 12.0;
                            if !item.detail.is_empty() {
                                let avail = rect.max.x - x - 90.0;
                                if avail > 40.0 {
                                    let detail = egui::WidgetText::from(egui::RichText::new(&item.detail).color(theme.ui.text_muted).size(ui_font - 1.0));
                                    let g = detail.into_galley(ui, Some(egui::TextWrapMode::Truncate), avail, egui::TextStyle::Body);
                                    ui.painter().galley(egui::pos2(x, rect.center().y - g.rect.height() / 2.0), g.clone(), theme.ui.text_muted);
                                    x += g.rect.width();
                                }
                            }
                            let _ = x;
                            if !item.shortcut.is_empty() {
                                ui.painter().text(
                                    egui::pos2(rect.max.x - 10.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    &item.shortcut,
                                    egui::FontId::proportional(ui_font - 1.0),
                                    theme.ui.text_muted,
                                );
                            }
                            if resp.clicked() {
                                clicked = Some(row);
                            }
                        }
                    });
                    if self.results.is_empty() && !self.query.is_empty() {
                        ui.label(egui::RichText::new("No matches").color(theme.ui.text_muted));
                    }
                    if let Some(row) = clicked {
                        self.selected = row;
                        let r = &self.results[row];
                        event = PaletteEvent::Accept(self.items[r.index].payload.clone(), self.query.clone());
                    }
                });
        });
        if escape {
            event = PaletteEvent::Cancel;
        } else if enter && event == PaletteEvent::None {
            event = match self.results.get(self.selected) {
                Some(r) => PaletteEvent::Accept(self.items[r.index].payload.clone(), self.query.clone()),
                None => PaletteEvent::AcceptText(self.query.clone()),
            };
        } else if event == PaletteEvent::None
            && (prev_selected != self.selected || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_) | egui::Event::Key { .. }))))
            && let Some(p) = self.selected_payload()
        {
            event = PaletteEvent::Highlight(p.clone());
        }
        // Click outside closes.
        if event == PaletteEvent::None
            && ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| ctx.layer_id_at(p).is_some_and(|l| l.id == egui::Id::new("apex-palette")))
        {
            event = PaletteEvent::Cancel;
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, detail: &str, boost: u32) -> PaletteItem {
        PaletteItem { label: label.into(), detail: detail.into(), shortcut: String::new(), payload: Payload::Text(label.into()), boost }
    }

    #[test]
    fn fuzzy_scoring_and_operators() {
        let mut p = Palette::default();
        p.show_with(PaletteMode::Anything, vec![item("src/main.rs", "", 0), item("src/app/model.rs", "", 0), item("README.md", "", 50)], "", "");
        // Empty query: boosted first.
        assert_eq!(p.items[p.results[0].index].label, "README.md");
        p.query = "mainrs:12".into();
        p.rescore();
        assert_eq!(p.match_text(), "mainrs");
        assert_eq!(p.operator(), Some((':', "12")));
        assert_eq!(p.items[p.results[0].index].label, "src/main.rs");
        assert!(!p.results[0].indices.is_empty());
        p.query = "zzzz".into();
        p.rescore();
        assert!(p.results.is_empty());
    }
}
