//! The command palette and Goto Anything overlay: a fuzzy picker over
//! application-provided items.
//!
//! The app (`workspace_ui.rs`, `lsp_ui.rs`, `git_ui.rs`, ...) fills a
//! [`Palette`] with [`PaletteItem`]s via [`Palette::show_with`], choosing a
//! [`PaletteMode`] that tells the app how to interpret the chosen
//! [`Payload`]. Each frame [`Palette::ui`] draws the overlay, re-scores the
//! items against the query with `nucleo_matcher`, and returns a
//! [`PaletteEvent`] the app acts on (open file, run command, preview the
//! highlighted item, or restore the view it saved in `restore`).
//!
//! Scoring is incremental only in the sense that an unchanged query is not
//! re-scored; every change rescans all items, capped at 500 results.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::command::Command;

/// What the palette is picking.
///
/// The app matches on this when it receives a [`PaletteEvent`] to know how
/// to interpret the payload; the palette itself only treats `Anything`
/// specially (operator parsing).
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteMode {
    /// Every [`Command`], labelled "Category: Name".
    Commands,
    /// Goto Anything: files, with `:line`, `@symbol`, `#text` operators.
    Anything,
    /// Symbols of the current document.
    Symbols,
    /// Symbols across the workspace.
    ProjectSymbols,
    Themes,
    Languages,
    Encodings,
    LineEndings,
    RecentFiles,
    RecentProjects,
    ClipboardHistory,
    /// Crash-recovery snapshots.
    Recovery,
    BuildTasks,
    Macros,
    /// Free text input (go to line, rename, etc.). The string names the
    /// prompt kind so the app knows what the answer is for.
    Prompt(String),
}

/// One selectable row.
#[derive(Debug, Clone)]
pub struct PaletteItem {
    /// Primary text, fuzzy-matched and shown with matched characters emphasised.
    pub label: String,
    /// Secondary text (directory, description); matched at half score if the label misses.
    pub detail: String,
    /// Right-aligned shortcut text, empty when none.
    pub shortcut: String,
    /// Opaque payload returned on selection.
    pub payload: Payload,
    /// Sort weight boost (recently used etc.).
    pub boost: u32,
}

/// What an item stands for; produced by the app and handed back unchanged
/// in [`PaletteEvent`].
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    /// Run a command with an optional argument.
    Command(Command, Option<String>),
    /// Open a file, optionally at a line.
    File(std::path::PathBuf, Option<usize>),
    /// Jump to a line and optional column in the current view.
    Line(usize, Option<usize>),
    /// Jump to a symbol: line, byte offset of its name and, for other files, the path.
    Symbol { line: usize, byte: usize, path: Option<std::path::PathBuf> },
    /// A plain value such as a theme or encoding name.
    Text(String),
    /// Index into a list the app holds (tasks, snippets, code actions).
    Index(usize),
}

/// A match for one item after scoring.
#[derive(Debug, Clone)]
pub struct ScoredItem {
    /// Index into `Palette::items`.
    pub index: usize,
    /// Matcher score plus `boost` and exact-match bonus; higher sorts first.
    pub score: u32,
    /// Character indices of the label that matched, sorted and deduplicated;
    /// empty for detail-only and empty-query matches.
    pub indices: Vec<u32>,
}

/// State of the overlay. Lives in `ApexApp` for the whole session; `open`
/// gates drawing.
pub struct Palette {
    pub open: bool,
    pub mode: PaletteMode,
    /// The text in the input box, including any operator suffix.
    pub query: String,
    /// Everything that can be chosen, as supplied by the app.
    pub items: Vec<PaletteItem>,
    /// Scored matches for the current query, best first.
    pub results: Vec<ScoredItem>,
    /// Index into `results` of the highlighted row.
    pub selected: usize,
    /// Set by `show_with` so the next `ui` call focuses the input.
    pub focus_requested: bool,
    /// Hint text shown in the empty input.
    pub placeholder: String,
    /// The view/scroll state to restore if the overlay is cancelled.
    pub restore: Option<(crate::view::ViewId, apex_edit::SelectionSet, egui::Vec2)>,
    /// Match text of the last `rescore`, to skip redundant work.
    last_query: Option<String>,
    /// Reused across frames; `nucleo` matchers hold scratch buffers.
    matcher: Matcher,
    /// Visible rows before the list scrolls; also the Page Up/Down step.
    pub max_rows: usize,
}

impl Default for Palette {
    /// Closed, in command mode, with a fresh matcher and 12 visible rows.
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
    /// An item was chosen (Enter or click), with the query at that moment.
    Accept(Payload, String),
    /// Accepted with no matching item (free text).
    AcceptText(String),
    /// Escape or a click outside the overlay.
    Cancel,
}

impl Palette {
    /// Open the overlay with a new item list, resetting the selection and
    /// scoring immediately so the first frame already shows results.
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

    /// Hide the overlay and drop its items and restore state.
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

    /// Recompute `results` for the current query. A no-op when the match
    /// text is unchanged and there are results already. Empty queries list
    /// items by `boost` only; otherwise labels are fuzzy-matched, details
    /// at half score, and an exact label match outranks everything.
    pub fn rescore(&mut self) {
        let text = self.match_text().to_owned();
        if self.last_query.as_deref() == Some(text.as_str()) && !self.results.is_empty() {
            return;
        }
        self.last_query = Some(text.clone());
        let mut out: Vec<ScoredItem> = Vec::with_capacity(self.items.len().min(2000));
        if text.trim().is_empty() {
            let mut idx: Vec<usize> = (0..self.items.len()).collect();
            // Stable sort keeps the app's order within equal boosts.
            idx.sort_by_key(|&i| std::cmp::Reverse(self.items[i].boost));
            out.extend(idx.into_iter().take(500).map(|i| ScoredItem { index: i, score: self.items[i].boost, indices: Vec::new() }));
        } else {
            let pattern = Pattern::parse(&text, CaseMatching::Smart, Normalization::Smart);
            // Typing a whole label picks that item, whatever the recency boosts.
            let bare = |t: &str| t.trim().trim_end_matches(['…', '.']).trim().to_lowercase();
            let exact = bare(&text);
            let mut buf = Vec::new();
            let mut indices = Vec::new();
            for (i, item) in self.items.iter().enumerate() {
                let hay = Utf32Str::new(&item.label, &mut buf);
                indices.clear();
                if let Some(score) = pattern.indices(hay, &mut self.matcher, &mut indices) {
                    // nucleo may report an index more than once and out of order.
                    indices.sort_unstable();
                    indices.dedup();
                    // Commands are listed as "Category: Name"; either form counts.
                    let named = item.label.split_once(": ").map(|(_, n)| n).unwrap_or(&item.label);
                    // 1_000_000 is well above any fuzzy score plus boost.
                    let bonus = if bare(&item.label) == exact || bare(named) == exact { 1_000_000 } else { 0 };
                    out.push(ScoredItem { index: i, score: score + item.boost + bonus, indices: indices.clone() });
                } else if !item.detail.is_empty() {
                    // Also match against the detail (path directory etc.) at a penalty.
                    let hay = Utf32Str::new(&item.detail, &mut buf);
                    if let Some(score) = pattern.score(hay, &mut self.matcher) {
                        out.push(ScoredItem { index: i, score: score / 2 + item.boost, indices: Vec::new() });
                    }
                }
            }
            // Equal scores: shorter labels first, as they are the tighter match.
            out.sort_by(|a, b| b.score.cmp(&a.score).then(self.items[a.index].label.len().cmp(&self.items[b.index].label.len())));
            out.truncate(500);
        }
        self.results = out;
        // Keep the highlight inside the (possibly shorter) new result list.
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// Payload of the highlighted row, if there is one.
    pub fn selected_payload(&self) -> Option<&Payload> {
        self.results.get(self.selected).map(|r| &self.items[r.index].payload)
    }

    /// Draw the overlay. Keyboard handling is done here because the text
    /// field owns focus.
    ///
    /// Returns what happened this frame; the caller is expected to `close`
    /// the palette on `Accept`/`Cancel`. Mutates `query`, `results` and
    /// `selected`.
    pub fn ui(&mut self, ctx: &egui::Context, theme: &crate::theme::Theme, ui_font: f32) -> PaletteEvent {
        if !self.open {
            return PaletteEvent::None;
        }
        let mut event = PaletteEvent::None;
        let prev_selected = self.selected;
        // Keys first, so the text edit does not consume them. Ctrl+P/N and
        // Ctrl+K/J are the Emacs/Vim-style alternatives to the arrows.
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
        // 60% of the window, but never narrower than a usable list nor wider than reads well.
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
                    // `lock_focus` keeps Tab inside the field (we use it as "next row").
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
                    // Lay out at most four pages of rows; the rest are reachable by typing.
                    let n = self.results.len().min(self.max_rows.max(1) * 4);
                    // Up/Down wrap around; Page Up/Down clamp.
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
                                // Only scroll on keyboard moves, so the mouse wheel is not fought.
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
                            // Walk the label once, grouping consecutive hit/non-hit chars into runs
                            // so the layout job gets few sections.
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
                                // Leave 90px on the right for the shortcut column; skip the
                                // detail entirely if the label has eaten nearly all the width.
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
        // Escape wins over everything; Enter accepts the highlighted row or,
        // with no results, the raw text (go-to-line, rename, ...).
        if escape {
            event = PaletteEvent::Cancel;
        } else if enter && event == PaletteEvent::None {
            event = match self.results.get(self.selected) {
                Some(r) => PaletteEvent::Accept(self.items[r.index].payload.clone(), self.query.clone()),
                None => PaletteEvent::AcceptText(self.query.clone()),
            };
        } else if event == PaletteEvent::None
            // Any key or text event may have changed the top result, so report a
            // highlight for live preview even when `selected` is still 0.
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

    /// Exact-label bonus beats boost, empty query orders by boost, and the
    /// Goto Anything operators are split off the match text.
    #[test]
    fn fuzzy_scoring_and_operators() {
        let mut p = Palette::default();
        p.show_with(PaletteMode::Anything, vec![item("src/main.rs", "", 0), item("src/app/model.rs", "", 0), item("README.md", "", 50)], "", "");
        // An exact label beats a boosted near match.
        let mut p2 = Palette::default();
        p2.show_with(PaletteMode::Commands, vec![item("Git: Stash Changes…", "", 500), item("Git: Stashes…", "", 0)], "t", "Stashes");
        p2.rescore();
        assert_eq!(p2.items[p2.results[0].index].label, "Git: Stashes…");
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
