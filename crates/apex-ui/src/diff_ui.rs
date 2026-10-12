//! Side-by-side diff view. A diff tab is an ordinary tab whose view shows the
//! base text (read-only); a second view, not in any tab, shows the other
//! side — the real document when comparing with the working copy, so it
//! stays editable with syntax colours, Vim mode and so on. The sides scroll
//! together, changed lines and words are tinted, and a strip between them
//! links each change, with buttons to revert it into the working copy or to
//! stage/unstage it.

use std::path::PathBuf;
use std::sync::Arc;

use apex_edit::{DocId, Document, Selection, SelectionSet};
use apex_project::diff_align::{self, Block};
use egui::{Color32, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::app::ApexApp;
use crate::command::Command;
use crate::view::{View, ViewId};
use crate::workspace::PaneId;

/// What the two sides are, for refreshing the base and for the strip's buttons.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffSource {
    /// Two files or texts; nothing to refresh.
    Plain,
    /// Index (left) against the working copy (right, editable).
    WorkingTree { workdir: PathBuf, path: PathBuf },
    /// HEAD (left) against the index (right, read-only).
    Staged { workdir: PathBuf, path: PathBuf },
    /// HEAD or index (left) against the buffer, no staging buttons.
    Revision { workdir: PathBuf, path: PathBuf, head: bool },
}

struct Computed {
    blocks: Vec<Block>,
    left_lines: Vec<(usize, Color32)>,
    right_lines: Vec<(usize, Color32)>,
    left_spans: Vec<(std::ops::Range<usize>, Color32)>,
    right_spans: Vec<(std::ops::Range<usize>, Color32)>,
}

pub struct DiffState {
    pub right_view: ViewId,
    pub right_focused: bool,
    pub source: DiffSource,
    pub left_label: String,
    pub right_label: String,
    computed: Option<(u64, u64, Arc<Computed>)>,
    /// Scroll positions last frame, to tell which side the user scrolled.
    last: Option<(usize, f32, usize, f32)>,
    /// Fold long unchanged stretches (both sides), and for which revisions
    /// the folds were made.
    pub fold_unchanged: bool,
    folded_for: Option<(u64, u64)>,
}

/// Lines of context kept around each change when unchanged stretches fold.
const CONTEXT: usize = 3;

/// Folds `(start, end)` (lines strictly between are hidden) for the long
/// unchanged blocks of one side.
fn unchanged_folds(blocks: &[Block], left: bool) -> Vec<(usize, usize)> {
    let n = blocks.len();
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !b.changed)
        .filter_map(|(i, b)| {
            let r = if left { &b.left } else { &b.right };
            let start = if r.start == 0 { 0 } else { r.start + CONTEXT - 1 };
            // The last block runs to the end of the file: no context needed after it.
            let end = if i + 1 == n { r.end.saturating_sub(1) } else { r.end.saturating_sub(CONTEXT) };
            (end > start + 2).then_some((start, end))
        })
        .collect()
}

/// Visual row of `line` given folds (hidden lines do not count).
fn visual_row(folds: &[(usize, usize)], line: usize) -> f32 {
    let mut hidden = 0usize;
    let mut l = line;
    for &(a, b) in folds {
        if l > a && l < b {
            l = a;
        }
        if l >= b {
            hidden += b - a - 1;
        }
    }
    (l - hidden.min(l)) as f32
}

/// Line tints and span tints for one side of a diff.
pub type Tints<'a> = (&'a [(usize, Color32)], &'a [(std::ops::Range<usize>, Color32)]);

fn tint(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn compute(left: &Document, right: &Document, del: Color32, add: Color32) -> Computed {
    let (lt, rt) = (left.text(), right.text());
    let blocks = diff_align::blocks(&lt, &rt);
    let mut c = Computed { blocks, left_lines: Vec::new(), right_lines: Vec::new(), left_spans: Vec::new(), right_spans: Vec::new() };
    let (lr, rr) = (left.rope(), right.rope());
    for b in diff_align::changes(&c.blocks) {
        c.left_lines.extend(b.left.clone().map(|l| (l, tint(del, 38))));
        c.right_lines.extend(b.right.clone().map(|l| (l, tint(add, 38))));
        if b.left.is_empty() || b.right.is_empty() || b.left.len() + b.right.len() > 400 {
            continue;
        }
        let span = |rope: &apex_buffer::Rope, r: &std::ops::Range<usize>| {
            let a = rope.line_to_char(r.start.min(rope.len_lines()));
            let z = if r.end < rope.len_lines() { rope.line_to_char(r.end) } else { rope.len_chars() };
            (a, rope.slice(a..z).to_string())
        };
        let (la, ltext) = span(lr, &b.left);
        let (ra, rtext) = span(rr, &b.right);
        let (ls, rs) = diff_align::word_spans(&ltext, &rtext);
        c.left_spans.extend(ls.into_iter().map(|r| (la + r.start..la + r.end, tint(del, 110))));
        c.right_spans.extend(rs.into_iter().map(|r| (ra + r.start..ra + r.end, tint(add, 110))));
    }
    c
}

impl ApexApp {
    /// Open a side-by-side diff tab. `right` is an existing document (the
    /// editable side) or a text shown read-only.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_side_by_side(
        &mut self,
        ctx: &egui::Context,
        title: &str,
        left_text: &str,
        left_label: &str,
        right: Result<DocId, (String, Option<PathBuf>)>,
        right_label: &str,
        source: DiffSource,
    ) {
        let (right_doc, language) = match right {
            Ok(d) => (d, self.language_of(d).id.to_string()),
            Err((text, hint)) => {
                let lang = apex_syntax::detect(hint.as_deref(), text.lines().next().unwrap_or("")).id.to_string();
                let mut doc = Document::from_text(&text);
                doc.read_only = true;
                doc.untitled_index = None;
                doc.scratch_title = Some(right_label.to_string());
                doc.language_override = Some(lang.clone());
                let id = doc.id;
                self.docs.insert(id, doc);
                (id, lang)
            }
        };
        let mut left = Document::from_text(left_text);
        left.read_only = true;
        left.untitled_index = None;
        left.scratch_title = Some(title.to_string());
        left.language_override = Some(language);
        let tab = self.add_document(left);
        let rev = self.docs.get(&right_doc).map(|d| d.revision()).unwrap_or(0);
        let mut rv = View::new(right_doc, rev);
        rv.word_wrap = Some(false);
        let right_view = rv.id;
        self.views.insert(right_view, rv);
        if let Some(v) = self.views.get_mut(&tab) {
            v.word_wrap = Some(false);
        }
        self.diffs.insert(
            tab,
            DiffState {
                right_view,
                right_focused: true,
                source,
                left_label: left_label.to_string(),
                right_label: right_label.to_string(),
                computed: None,
                last: None,
                fold_unchanged: true,
                folded_for: None,
            },
        );
        self.focus_view(tab);
        self.focus_active_editor(ctx);
        // Start at the first change.
        self.diff_jump(tab, true, true);
    }

    /// The document for `path` without leaving a new tab open.
    pub(crate) fn doc_for_path(&mut self, path: &std::path::Path) -> Option<DocId> {
        let norm = crate::app::normalise_path(path);
        if let Some(d) = self.docs.iter().find(|(_, d)| d.path.as_ref() == Some(&norm)).map(|(id, _)| *id) {
            return Some(d);
        }
        let v = self.open_path(&norm)?;
        let d = self.views.get(&v)?.doc;
        for pane in self.panes.values_mut() {
            pane.remove(v);
        }
        self.views.remove(&v);
        Some(d)
    }

    fn diff_computed(&mut self, tab: ViewId) -> Option<Arc<Computed>> {
        let ld = self.views.get(&tab)?.doc;
        let st = self.diffs.get(&tab)?;
        let rd = self.views.get(&st.right_view)?.doc;
        let (l, r) = (self.docs.get(&ld)?, self.docs.get(&rd)?);
        let key = (l.revision(), r.revision());
        if let Some((a, b, c)) = &st.computed
            && (*a, *b) == key
        {
            return Some(c.clone());
        }
        let c = Arc::new(compute(l, r, self.theme.editor.git_deleted, self.theme.editor.git_added));
        self.diffs.get_mut(&tab)?.computed = Some((key.0, key.1, c.clone()));
        Some(c)
    }

    pub(crate) fn show_diff(&mut self, ui: &mut Ui, pane_id: PaneId, tab: ViewId, rect: Rect) {
        let Some(c) = self.diff_computed(tab) else { return };
        self.diff_apply_folds(tab, &c.blocks);
        let Some(st) = self.diffs.get(&tab) else { return };
        let (right_view, right_focused) = (st.right_view, st.right_focused);
        let fold_unchanged = st.fold_unchanged;
        let (left_label, right_label) = (st.left_label.clone(), st.right_label.clone());
        let source = st.source.clone();
        let theme = self.theme.clone();
        let font = crate::editor::font_for(&self.settings, self.font_zoom);
        let (lh, _) = crate::editor::metrics(ui.ctx(), &font, self.settings.line_height);

        let header_h = 24.0;
        let strip_w = 44.0;
        let half = ((rect.width() - strip_w) / 2.0).max(50.0);
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
        let left_rect = Rect::from_min_max(pos2(rect.min.x, rect.min.y + header_h), pos2(rect.min.x + half, rect.max.y));
        let strip = Rect::from_min_max(pos2(left_rect.max.x, left_rect.min.y), pos2(left_rect.max.x + strip_w, rect.max.y));
        let right_rect = Rect::from_min_max(pos2(strip.max.x, left_rect.min.y), rect.max);

        // Header: labels, change count, navigation.
        let p = ui.painter_at(header);
        p.rect_filled(header, 0.0, theme.ui.surface);
        let small = egui::FontId::proportional(self.settings.ui_font_size - 1.0);
        // Labels are clipped to their half; the navigation sits over the right end.
        let lp = ui.painter_at(Rect::from_min_max(header.min, pos2(left_rect.max.x - 4.0, header.max.y)));
        lp.text(pos2(left_rect.min.x + 8.0, header.center().y), egui::Align2::LEFT_CENTER, &left_label, small.clone(), theme.ui.text_muted);
        let rp = ui.painter_at(Rect::from_min_max(pos2(right_rect.min.x, header.min.y), pos2(header.max.x - 340.0, header.max.y)));
        rp.text(pos2(right_rect.min.x + 8.0, header.center().y), egui::Align2::LEFT_CENTER, &right_label, small.clone(), theme.ui.text_muted);
        let n = diff_align::changes(&c.blocks).count();
        let mut nav: Option<bool> = None;
        let mut unified = false;
        let mut hdr = ui.new_child(egui::UiBuilder::new().max_rect(header.shrink2(vec2(6.0, 2.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if hdr.small_button("Unified").on_hover_text("Open the same comparison as a unified diff").clicked() {
            unified = true;
        }
        let mut toggle_folds = false;
        if hdr
            .small_button(if fold_unchanged { "Expand" } else { "Collapse" })
            .on_hover_text(if fold_unchanged { "Show the unchanged lines" } else { "Fold unchanged lines, keeping some context" })
            .clicked()
        {
            toggle_folds = true;
        }
        if hdr.small_button("Next").on_hover_text("Next change").clicked() {
            nav = Some(true);
        }
        if hdr.small_button("Previous").on_hover_text("Previous change").clicked() {
            nav = Some(false);
        }
        hdr.label(
            egui::RichText::new(if n == 0 { "No differences".to_string() } else { format!("{n} change{}", if n == 1 { "" } else { "s" }) })
                .size(self.settings.ui_font_size - 1.0),
        );

        let active = pane_id == self.active_pane;
        self.show_view(ui, pane_id, tab, left_rect, active && !right_focused, Some((&c.left_lines, &c.left_spans)), Some((tab, false)));
        self.show_view(ui, pane_id, right_view, right_rect, active && right_focused, Some((&c.right_lines, &c.right_spans)), Some((tab, true)));

        // Connector strip.
        let (ltop, rtop) = match (self.views.get(&tab), self.views.get(&right_view)) {
            (Some(l), Some(r)) => (l.top_line as f32, r.top_line as f32),
            _ => return,
        };
        let right_editable = self.views.get(&right_view).and_then(|v| self.docs.get(&v.doc)).is_some_and(|d| !d.read_only);
        let painter = ui.painter_at(strip);
        painter.rect_filled(strip, 0.0, theme.editor.gutter_background);
        let lfolds = self.views.get(&tab).and_then(|v| self.docs.get(&v.doc).map(|d| v.folds(d))).unwrap_or_default();
        let rfolds = self.views.get(&right_view).and_then(|v| self.docs.get(&v.doc).map(|d| v.folds(d))).unwrap_or_default();
        let (ltop_row, rtop_row) = (visual_row(&lfolds, ltop as usize), visual_row(&rfolds, rtop as usize));
        let yl = |line: usize| strip.min.y + (visual_row(&lfolds, line) - ltop_row) * lh;
        let yr = |line: usize| strip.min.y + (visual_row(&rfolds, line) - rtop_row) * lh;
        let mut action: Option<(usize, bool)> = None; // (block index, revert?)
        for (i, b) in c.blocks.iter().enumerate().filter(|(_, b)| b.changed) {
            let (l0, l1) = (yl(b.left.start), yl(b.left.end));
            let (r0, r1) = (yr(b.right.start), yr(b.right.end));
            if (l1 < strip.min.y && r1 < strip.min.y) || (l0 > strip.max.y && r0 > strip.max.y) {
                continue;
            }
            let colour = if b.left.is_empty() {
                theme.editor.git_added
            } else if b.right.is_empty() {
                theme.editor.git_deleted
            } else {
                theme.editor.git_modified
            };
            let pts = vec![pos2(strip.min.x, l0), pos2(strip.max.x, r0), pos2(strip.max.x, r1.max(r0 + 1.0)), pos2(strip.min.x, l1.max(l0 + 1.0))];
            painter.add(egui::Shape::convex_polygon(pts, tint(colour, 45), Stroke::new(1.0, tint(colour, 160))));
            // Buttons sit at the middle of the link.
            let mid = ((l0 + l1 + r0 + r1) / 4.0).clamp(strip.min.y + 8.0, strip.max.y - 8.0);
            let mut bx = strip.min.x + 2.0;
            let mut button = |label: &str, tip: &str, revert: bool| {
                let r = Rect::from_min_size(pos2(bx, mid - 9.0), vec2(19.0, 18.0));
                bx += 21.0;
                if ui.put(r, egui::Button::new(egui::RichText::new(label).size(11.0)).small()).on_hover_text(tip).clicked() {
                    action = Some((i, revert));
                }
            };
            if right_editable {
                button(">", "Revert: replace this change with the left side (undoable)", true);
            }
            match source {
                DiffSource::WorkingTree { .. } => button("+", "Stage this change", false),
                DiffSource::Staged { .. } => button("-", "Unstage this change", false),
                _ => {}
            }
        }
        let _ = ui.interact(strip, ui.id().with(("diff-strip", tab.0)), Sense::hover());
        if let Some((i, revert)) = action
            && let Some(b) = c.blocks.get(i).cloned()
        {
            if revert {
                self.diff_revert_block(tab, &b);
            } else {
                self.diff_stage_block(&source, &b);
            }
        }
        if let Some(fwd) = nav {
            self.diff_jump(tab, fwd, false);
        }
        if unified {
            self.diff_open_unified(ui.ctx(), tab);
        }
        if toggle_folds && let Some(st) = self.diffs.get_mut(&tab) {
            st.fold_unchanged = !st.fold_unchanged;
            st.folded_for = None;
        }
        // Keep the sides aligned: whichever side moved this frame (scrolling,
        // caret movement, a jump) leads; the other follows on the next frame.
        if self.diff_sync_scroll(tab, &c.blocks) {
            ui.ctx().request_repaint();
        }
        self.diff_record_scroll(tab);
    }

    /// Fold (or unfold) the unchanged stretches of both sides when the
    /// texts or the setting changed.
    fn diff_apply_folds(&mut self, tab: ViewId, blocks: &[Block]) {
        let Some(st) = self.diffs.get(&tab) else { return };
        let key = st.computed.as_ref().map(|(a, b, _)| (*a, *b));
        if st.folded_for.is_some() && st.folded_for == key {
            return;
        }
        let (fold, rv) = (st.fold_unchanged, st.right_view);
        for (v, left) in [(tab, true), (rv, false)] {
            let Some(d) = self.views.get(&v).map(|x| x.doc) else { continue };
            let Some(doc) = self.docs.get(&d) else { continue };
            let folds = if fold { unchanged_folds(blocks, left) } else { Vec::new() };
            if let Some(view) = self.views.get_mut(&v) {
                view.fold_anchors.clear();
                for (a, b) in folds {
                    view.fold(doc, a, b);
                }
                view.cache.clear();
            }
        }
        if let Some(st) = self.diffs.get_mut(&tab) {
            st.folded_for = key;
        }
    }

    fn diff_positions(&self, tab: ViewId) -> Option<(usize, f32, usize, f32)> {
        let st = self.diffs.get(&tab)?;
        let (l, r) = (self.views.get(&tab)?, self.views.get(&st.right_view)?);
        Some((l.top_line, l.scroll.x, r.top_line, r.scroll.x))
    }

    fn diff_record_scroll(&mut self, tab: ViewId) {
        let pos = self.diff_positions(tab);
        if let Some(st) = self.diffs.get_mut(&tab) {
            st.last = pos;
        }
    }

    fn diff_sync_scroll(&mut self, tab: ViewId, blocks: &[Block]) -> bool {
        let (Some(now), Some(st)) = (self.diff_positions(tab), self.diffs.get(&tab)) else { return false };
        let right_view = st.right_view;
        // First frame: the focused side (where the first change was put in view) leads.
        let (left_moved, right_moved) = match st.last {
            Some(last) => (now.0 != last.0 || now.1 != last.1, now.2 != last.2 || now.3 != last.3),
            None => (!st.right_focused, st.right_focused),
        };
        if left_moved {
            let target = diff_align::map_line(blocks, now.0 as f32, true).round() as usize;
            if let Some(r) = self.views.get_mut(&right_view) {
                r.top_line = target;
                r.top_row = 0;
                r.scroll.x = now.1;
            }
        } else if right_moved {
            let target = diff_align::map_line(blocks, now.2 as f32, false).round() as usize;
            if let Some(l) = self.views.get_mut(&tab) {
                l.top_line = target;
                l.top_row = 0;
                l.scroll.x = now.3;
            }
        }
        left_moved || right_moved
    }

    /// Move to the next/previous change (on the focused side).
    pub(crate) fn diff_jump(&mut self, tab: ViewId, forward: bool, first: bool) {
        let Some(c) = self.diff_computed(tab) else { return };
        let Some(st) = self.diffs.get(&tab) else { return };
        let (v, right) = if st.right_focused { (st.right_view, true) } else { (tab, false) };
        let Some(view) = self.views.get(&v) else { return };
        let Some(doc) = self.docs.get(&view.doc) else { return };
        let line = if first { 0 } else { view.primary_line(doc) };
        let starts: Vec<usize> = diff_align::changes(&c.blocks).map(|b| if right { b.right.start } else { b.left.start }).collect();
        if starts.is_empty() {
            return;
        }
        let target = if first {
            starts[0]
        } else if forward {
            starts.iter().copied().find(|&s| s > line).unwrap_or(starts[0])
        } else {
            starts.iter().rev().copied().find(|&s| s < line).unwrap_or(*starts.last().expect("non-empty"))
        };
        self.goto_line_col(v, target + 1, 1);
        if let Some(view) = self.views.get_mut(&v) {
            view.want_centre = true;
        }
    }

    /// Replace a change on the editable right side with the left lines.
    fn diff_revert_block(&mut self, tab: ViewId, b: &Block) {
        let Some(st) = self.diffs.get(&tab) else { return };
        let rv = st.right_view;
        let Some(ld) = self.views.get(&tab).map(|v| v.doc) else { return };
        let Some(left) = self.docs.get(&ld) else { return };
        let lr = left.rope();
        let a = lr.line_to_char(b.left.start.min(lr.len_lines()));
        let z = if b.left.end < lr.len_lines() { lr.line_to_char(b.left.end) } else { lr.len_chars() };
        let mut replacement = lr.slice(a..z).to_string();
        let (rs, re) = (b.right.start, b.right.end);
        self.run_edit(rv, move |doc, _s| {
            let rope = doc.rope();
            let n = rope.len_lines();
            let start = if rs < n { rope.line_to_char(rs) } else { rope.len_chars() };
            let end = if re < n { rope.line_to_char(re) } else { rope.len_chars() };
            // Inserting before the end of a file without a final newline.
            if end == rope.len_chars() && start == end && start > 0 && !replacement.is_empty() && rope.char(start - 1) != '\n' {
                replacement.insert(0, '\n');
                replacement.pop();
            }
            let text = apex_buffer::line_ending::normalise(&replacement, doc.line_ending);
            let tx = apex_edit::Transaction::single(apex_edit::Change::replace(start, end, text));
            Some(apex_edit::ops::EditResult { tx, sels: SelectionSet::single(Selection::point(start)), kind: apex_edit::EditKind::Other })
        });
    }

    fn diff_stage_block(&mut self, source: &DiffSource, b: &Block) {
        let (workdir, path, unstage) = match source {
            DiffSource::WorkingTree { workdir, path } => (workdir.clone(), path.clone(), false),
            DiffSource::Staged { workdir, path } => (workdir.clone(), path.clone(), true),
            _ => return,
        };
        if !unstage && self.docs.values().any(|d| d.path.as_ref() == Some(&path) && d.is_dirty()) {
            self.set_status("Save the file before staging part of it".into(), false);
            return;
        }
        if !self.trust.is_trusted(&workdir) {
            self.set_status("Trust the workspace first (Project > Trust Workspace): staging runs git".into(), false);
            return;
        }
        let Some(rel) = crate::git_ops::rel_path(&workdir, &path) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let untracked = self.git_entry_for(&workdir, &path).is_some_and(|e| e.unstaged == Some(apex_project::git::FileStatus::Untracked));
        self.git_stage_ranges(workdir, rel, name, vec![b.right.clone()], untracked, "change", unstage);
    }

    fn diff_open_unified(&mut self, ctx: &egui::Context, tab: ViewId) {
        let Some(st) = self.diffs.get(&tab) else { return };
        let (ll, rl) = (st.left_label.clone(), st.right_label.clone());
        let rd = self.views.get(&st.right_view).map(|v| v.doc);
        let ld = self.views.get(&tab).map(|v| v.doc);
        let (Some(l), Some(r)) = (ld.and_then(|d| self.docs.get(&d)), rd.and_then(|d| self.docs.get(&d))) else { return };
        let text = apex_project::git::unified_diff(&l.text(), &r.text(), &ll, &rl);
        let title = format!("{}.diff", l.title());
        self.open_scratch_doc(&title, &text, Some("diff"), true, ctx);
    }

    /// After git changes the index or HEAD, reload the bases of open diffs.
    pub(crate) fn diff_refresh_bases(&mut self) {
        let tabs: Vec<(ViewId, DiffSource)> = self.diffs.iter().map(|(k, v)| (*k, v.source.clone())).collect();
        for (tab, source) in tabs {
            let (left, right) = match &source {
                DiffSource::WorkingTree { workdir, path } | DiffSource::Revision { workdir, path, head: false } => {
                    (apex_project::git::file_at(workdir, path, false).ok().flatten(), None)
                }
                DiffSource::Revision { workdir, path, head: true } => (apex_project::git::file_at(workdir, path, true).ok().flatten(), None),
                DiffSource::Staged { workdir, path } => {
                    (apex_project::git::file_at(workdir, path, true).ok().flatten(), apex_project::git::file_at(workdir, path, false).ok().flatten())
                }
                DiffSource::Plain => continue,
            };
            let ld = self.views.get(&tab).map(|v| v.doc);
            let rd = self.diffs.get(&tab).and_then(|s| self.views.get(&s.right_view)).map(|v| v.doc);
            for (d, text) in [(ld, left), (rd.filter(|_| matches!(source, DiffSource::Staged { .. })), right)] {
                if let (Some(d), Some(t)) = (d, text)
                    && let Some(doc) = self.docs.get_mut(&d)
                    && doc.text() != t
                {
                    doc.replace_all(&t, &SelectionSet::single(Selection::point(0)));
                    doc.mark_clean();
                }
            }
        }
    }

    /// The diff tab containing `v` (either side), if any.
    pub(crate) fn diff_tab_of(&self, v: ViewId) -> Option<ViewId> {
        if self.diffs.contains_key(&v) {
            return Some(v);
        }
        self.diffs.iter().find(|(_, s)| s.right_view == v).map(|(k, _)| *k)
    }

    // ----- opening from the commands ------------------------------------------

    /// Compare the active document (buffer) with saved/HEAD/index side by side.
    pub(crate) fn diff_compare_active(&mut self, ctx: &egui::Context, cmd: Command) {
        let Some((_, d)) = self.active_ids() else { return };
        let Some(path) = self.docs.get(&d).and_then(|doc| doc.path.clone()) else {
            self.set_status("Save the file first".into(), false);
            return;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let workdir = apex_project::git::workdir_of(&path);
        let (base, label, source) = match cmd {
            Command::CompareWithSaved => {
                let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
                (apex_project::fileio::load(&path, None, fallback).ok().map(|l| l.text), "saved", DiffSource::Plain)
            }
            Command::CompareWithHead | Command::CompareWithIndex => {
                let head = cmd == Command::CompareWithHead;
                let Some(w) = workdir else {
                    self.set_status("Not in a Git repository".into(), false);
                    return;
                };
                (
                    apex_project::git::file_at(&w, &path, head).ok().flatten(),
                    if head { "HEAD" } else { "index" },
                    DiffSource::Revision { workdir: w, path: path.clone(), head },
                )
            }
            _ => return,
        };
        let Some(base) = base else {
            self.set_status(format!("No {label} version of this file"), false);
            return;
        };
        self.open_side_by_side(ctx, &format!("{name} ({label} ↔ buffer)"), &base, &format!("{name} ({label})"), Ok(d), &format!("{name} (buffer)"), source);
    }

    /// Unstaged (index → working copy) or staged (HEAD → index) changes of a file.
    pub(crate) fn diff_open_changes(&mut self, ctx: &egui::Context, workdir: PathBuf, path: PathBuf, staged: bool) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let index = apex_project::git::file_at(&workdir, &path, false).ok().flatten().unwrap_or_default();
        if staged {
            let head = apex_project::git::file_at(&workdir, &path, true).ok().flatten().unwrap_or_default();
            if head == index {
                self.set_status(format!("No staged changes in {name}"), false);
                return;
            }
            let src = DiffSource::Staged { workdir, path: path.clone() };
            self.open_side_by_side(
                ctx,
                &format!("{name} (staged)"),
                &head,
                &format!("{name} (HEAD)"),
                Err((index, Some(path))),
                &format!("{name} (index)"),
                src,
            );
        } else if path.is_file() {
            let Some(d) = self.doc_for_path(&path) else { return };
            let src = DiffSource::WorkingTree { workdir, path: path.clone() };
            self.open_side_by_side(ctx, &format!("{name} (changes)"), &index, &format!("{name} (index)"), Ok(d), &format!("{name} (working tree)"), src);
        } else {
            // Deleted in the working tree.
            self.open_side_by_side(
                ctx,
                &format!("{name} (deleted)"),
                &index,
                &format!("{name} (index)"),
                Err((String::new(), Some(path))),
                "(deleted)",
                DiffSource::Plain,
            );
        }
    }

    /// `apex --diff old new`: the new file is opened editable on the right.
    pub(crate) fn diff_files(&mut self, ctx: &egui::Context, old: &std::path::Path, new: &std::path::Path) -> Result<(), String> {
        let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
        let left = apex_project::fileio::load(old, None, fallback).map_err(|e| format!("Could not read {}: {e}", old.display()))?.text;
        let d = self.doc_for_path(new).ok_or_else(|| format!("Could not open {}", new.display()))?;
        let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
        self.open_side_by_side(
            ctx,
            &format!("{} ↔ {}", name(old), name(new)),
            &left,
            &old.display().to_string(),
            Ok(d),
            &new.display().to_string(),
            DiffSource::Plain,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_keep_context_and_rows_skip_hidden_lines() {
        let left: String = (0..40).map(|i| format!("{i}\n")).collect();
        let mut right_lines: Vec<String> = (0..40).map(|i| format!("{i}\n")).collect();
        right_lines[20] = "changed\n".into();
        let right: String = right_lines.concat();
        let blocks = diff_align::blocks(&left, &right);
        let f = unchanged_folds(&blocks, true);
        // Lines 0..20 unchanged: fold from 0 to 17 (keeps 17, 18, 19); 21..40: 23 to 39.
        assert_eq!(f, vec![(0, 17), (23, 39)]);
        assert_eq!(visual_row(&f, 0), 0.0);
        assert_eq!(visual_row(&f, 17), 1.0);
        assert_eq!(visual_row(&f, 20), 4.0);
        assert_eq!(visual_row(&f, 10), 0.0, "a hidden line sits on its fold's first line");
        assert!(unchanged_folds(&diff_align::blocks("a\nb\n", "a\nc\n"), true).is_empty());
    }
}
