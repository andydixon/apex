//! A view onto a document: selections, scroll state and layout caches.
//!
//! Views own all per-window editing state; [`apex_edit::Document`] owns the
//! text and history. Several views may show one document.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use apex_buffer::motion::{self, next_grapheme_boundary, prev_grapheme_boundary};
use apex_buffer::position::{char_col_to_display_col, display_col_to_char_col, line_end_char, line_len_chars};
use apex_edit::{DocId, Document, Selection, SelectionSet};
use egui::Galley;

/// Process-unique identifier of a [`View`], allocated from a global counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub u64);

/// Next value handed out by [`ViewId::next`]; starts at 1 so 0 is never used.
static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);

impl ViewId {
    /// Allocate a fresh, never-reused id.
    pub fn next() -> Self {
        Self(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Stable egui widget id for this view (used for focus and memory).
    pub fn egui_id(self) -> egui::Id {
        egui::Id::new(("apex-view", self.0))
    }

    /// Focus this view, including when a dialog opens it after editor rendering.
    pub(crate) fn request_focus(self, ctx: &egui::Context) {
        // AccessKit needs a node even if the new view has not been rendered yet.
        ctx.accesskit_node_builder(self.egui_id(), |_| {});
        ctx.memory_mut(|m| m.request_focus(self.egui_id()));
    }
}

/// One laid-out document line.
pub struct LineLayout {
    /// Shaped text for the line, with tabs expanded (see `map`).
    pub galley: Arc<Galley>,
    /// Number of visual rows (1 unless soft-wrapped).
    pub rows: usize,
    /// `map[doc_col] = galley char index`, present only when the line contains
    /// tabs (which are expanded to spaces for layout) or other substituted chars.
    pub map: Option<Vec<u32>>,
    /// [`LayoutCache::epoch`] at build time; stale once the cache epoch moves on.
    pub epoch: u64,
    /// Built with syntax colours (a plain layout is replaced once colours are available).
    pub highlighted: bool,
    /// Wrap width the layout was produced for (`None` = no wrapping).
    pub wrap_width: Option<f32>,
}

impl LineLayout {
    /// Map a document char column to a galley char index; columns past the
    /// end clamp to the last mapped index.
    #[inline]
    pub fn to_galley_col(&self, doc_col: usize) -> usize {
        match &self.map {
            Some(m) => m.get(doc_col).copied().unwrap_or(*m.last().unwrap_or(&0)) as usize,
            None => doc_col,
        }
    }

    /// Map a galley char index back to a document char column. A galley index
    /// inside an expanded tab maps to the tab's column.
    #[inline]
    pub fn to_doc_col(&self, galley_col: usize) -> usize {
        match &self.map {
            Some(m) => match m.binary_search(&(galley_col as u32)) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            },
            None => galley_col,
        }
    }

    /// X offset (galley-local) of a document column.
    pub fn x_of(&self, doc_col: usize) -> f32 {
        let g = self.to_galley_col(doc_col);
        self.galley.pos_from_cursor(egui::text::CCursor { index: egui::text::CharIndex(g), prefer_next_row: false }).min.x
    }

    /// Laid-out width of the line in points.
    pub fn width(&self) -> f32 {
        self.galley.rect.width()
    }
}

/// Cache of laid-out lines keyed by line number. Entries are invalidated by
/// bumping `epoch` (on any document, font, theme or width change).
///
/// ponytail: whole-cache invalidation on every edit; a line-range invalidation
/// would avoid re-laying ~100 visible lines per keystroke if profiling shows it matters.
#[derive(Default)]
pub struct LayoutCache {
    /// Current generation; entries with a different epoch are treated as absent.
    pub epoch: u64,
    lines: HashMap<usize, LineLayout>,
}

impl LayoutCache {
    /// Mark every entry stale by bumping `epoch`. Stale entries stay in the map
    /// (and are overwritten on re-layout) until it grows past 4096 lines.
    pub fn invalidate(&mut self) {
        self.epoch += 1;
        if self.lines.len() > 4096 {
            self.lines.clear();
        }
    }

    /// Layout for document line `line`, if one exists for the current epoch.
    pub fn get(&self, line: usize) -> Option<&LineLayout> {
        self.lines.get(&line).filter(|l| l.epoch == self.epoch)
    }

    /// Store the layout for document line `line`, replacing any previous one.
    pub fn insert(&mut self, line: usize, layout: LineLayout) {
        self.lines.insert(line, layout);
    }

    /// Drop all entries without changing the epoch.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Mouse drag in progress.
#[derive(Debug, Clone)]
pub struct DragState {
    /// Granularity of the drag, chosen by click count and modifiers.
    pub mode: DragMode,
    /// Selections present before the drag (for add-cursor drags).
    pub base: Vec<Selection>,
    /// The anchor selection of this drag (word/line extents for those modes).
    pub origin: Selection,
    /// Column mode: display column of the origin.
    pub origin_display_col: usize,
}

/// Unit that a mouse drag extends the selection by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    /// Grapheme-by-grapheme (single click).
    Char,
    /// Whole words (double click).
    Word,
    /// Whole lines (triple click or gutter).
    Line,
    /// Rectangular block: one selection per line between origin and pointer.
    Column,
}

/// Per-pane editing state for one document: selections, scroll position,
/// folds, layout cache and transient UI flags.
pub struct View {
    /// Identity of this view.
    pub id: ViewId,
    /// Document shown by this view.
    pub doc: DocId,
    /// Selections, as char indices into the document.
    pub sels: SelectionSet,
    /// Document revision that `sels` and `fold_anchors` were last mapped to.
    pub seen_revision: u64,
    /// Scroll offset in points. Only `x` (horizontal) is used; vertical
    /// position is held in `top_line`/`top_row`.
    pub scroll: egui::Vec2,
    /// Font-size steps relative to the configured size.
    pub zoom_steps: i32,
    /// Request for the next frame to scroll the primary caret into view; cleared once done.
    pub want_cursor_visible: bool,
    /// Like `want_cursor_visible`, but centres the caret vertically (used after jumps).
    pub want_centre: bool,
    /// Overwrite (Insert-key) mode: typed text replaces the following character.
    pub overwrite: bool,
    /// Per-view word-wrap override; `None` follows the global setting.
    pub word_wrap: Option<bool>,
    /// Laid-out lines for this view.
    pub cache: LayoutCache,
    /// Mouse drag in progress, if any.
    pub drag: Option<DragState>,
    /// Time (egui seconds) the caret blink cycle last restarted; reset on input.
    pub blink_epoch: f64,
    /// IME composition text and its cursor offset (in chars) within that text.
    pub ime_preedit: Option<(String, usize)>,
    /// Visual rows that fit in the viewport at the last layout; used for paging.
    pub lines_per_page: usize,
    /// First document line at the top of the viewport and the visual row within it.
    pub top_line: usize,
    /// Visual row within `top_line` shown at the top (non-zero only when wrapped).
    pub top_row: usize,
    /// Sub-row pixel offset accumulated from smooth scrolling.
    pub scroll_remainder: f32,
    /// Folded regions as char anchors `(start_of_first_line, start_of_last_line)`;
    /// lines strictly between are hidden. Kept in document order.
    pub fold_anchors: Vec<(usize, usize)>,
    /// Selection sets prior to multi-cursor additions, for "undo selection".
    pub selection_history: Vec<SelectionSet>,
    /// Quick Add Next started from a word expansion, so further additions match whole words.
    pub quick_add_word_mode: bool,
    /// Screen rectangle the editor widget occupied last frame.
    pub last_rect: egui::Rect,
    /// Screen rectangle of the primary caret as last painted.
    pub last_caret_rect: Option<egui::Rect>,
    /// Cached minimap shapes: (key, shapes, built at, epoch).
    pub minimap_cache: Option<(MinimapKey, Vec<egui::Shape>, std::time::Instant, u64)>,
}

/// Minimap cache key: (layout epoch, first line, end line, theme name length,
/// muted-syntax flag). A change in any field forces a rebuild.
pub type MinimapKey = (u64, usize, usize, usize, bool);

impl View {
    /// A fresh view of `doc` at `revision`: caret at 0, no scroll, no folds.
    pub fn new(doc: DocId, revision: u64) -> Self {
        Self {
            id: ViewId::next(),
            doc,
            sels: SelectionSet::default(),
            seen_revision: revision,
            scroll: egui::Vec2::ZERO,
            zoom_steps: 0,
            want_cursor_visible: true,
            want_centre: false,
            overwrite: false,
            word_wrap: None,
            cache: LayoutCache::default(),
            drag: None,
            blink_epoch: 0.0,
            ime_preedit: None,
            lines_per_page: 40,
            top_line: 0,
            top_row: 0,
            scroll_remainder: 0.0,
            fold_anchors: Vec::new(),
            selection_history: Vec::new(),
            quick_add_word_mode: false,
            last_rect: egui::Rect::NOTHING,
            minimap_cache: None,
            last_caret_rect: None,
        }
    }

    /// A second view of the same document with copied selection/scroll state.
    pub fn duplicate(&self) -> Self {
        let mut v = Self::new(self.doc, self.seen_revision);
        v.sels = self.sels.clone();
        v.scroll = self.scroll;
        v.top_line = self.top_line;
        v.top_row = self.top_row;
        v.fold_anchors = self.fold_anchors.clone();
        v.zoom_steps = self.zoom_steps;
        v
    }

    /// Bring selections up to date with the document and invalidate layout
    /// caches if the text changed under us.
    pub fn sync(&mut self, doc: &Document) {
        if self.seen_revision != doc.revision() {
            doc.sync_selections(&mut self.sels, self.seen_revision);
            // Map fold anchors through the edits, then snap each back to a
            // line start and drop folds that no longer hide any line.
            let mut flat: Vec<usize> = self.fold_anchors.iter().flat_map(|(a, b)| [*a, *b]).collect();
            doc.sync_positions(&mut flat, self.seen_revision);
            let rope = doc.rope();
            self.fold_anchors = flat
                .chunks(2)
                .map(|c| (rope.line_to_char(rope.char_to_line(c[0])), rope.line_to_char(rope.char_to_line(c[1]))))
                .filter(|(a, b)| rope.char_to_line(*b) > rope.char_to_line(*a) + 1)
                .collect();
            self.seen_revision = doc.revision();
            self.cache.invalidate();
        }
        // A caret inside a folded region unfolds it (never edit hidden text).
        if !self.fold_anchors.is_empty() {
            let rope = doc.rope();
            let heads: Vec<usize> = self.sels.iter().flat_map(|s| [s.head, s.anchor]).collect();
            self.fold_anchors.retain(|(a, b)| {
                let (la, lb) = (rope.char_to_line(*a), rope.char_to_line(*b));
                !heads.iter().any(|h| {
                    let l = rope.char_to_line((*h).min(rope.len_chars()));
                    l > la && l < lb
                })
            });
        }
    }

    // ----- folding ----------------------------------------------------------

    /// Folded regions as `(start_line, end_line)`; lines in between are hidden.
    pub fn folds(&self, doc: &Document) -> Vec<(usize, usize)> {
        let rope = doc.rope();
        self.fold_anchors.iter().map(|(a, b)| (rope.char_to_line((*a).min(rope.len_chars())), rope.char_to_line((*b).min(rope.len_chars())))).collect()
    }

    /// Whether document line `line` lies strictly inside a fold.
    pub fn is_line_hidden(&self, doc: &Document, line: usize) -> bool {
        self.folds(doc).iter().any(|(a, b)| line > *a && line < *b)
    }

    /// The fold whose first (visible) line is `line`, as `(start_line, end_line)`.
    pub fn fold_at(&self, doc: &Document, line: usize) -> Option<(usize, usize)> {
        self.folds(doc).into_iter().find(|(a, _)| *a == line)
    }

    /// Fold lines `start..=end`, hiding those strictly between. No-op if nothing
    /// would be hidden or a fold already starts at `start`. Carets in the hidden
    /// range move to the start of `start`; invalidates the layout cache.
    pub fn fold(&mut self, doc: &Document, start: usize, end: usize) {
        if end <= start + 1 {
            return;
        }
        let rope = doc.rope();
        let a = rope.line_to_char(start.min(rope.len_lines().saturating_sub(1)));
        let b = rope.line_to_char(end.min(rope.len_lines().saturating_sub(1)));
        // Drop nested folds inside the new one; keep others.
        self.fold_anchors.retain(|(x, y)| !(*x >= a && *y <= b));
        if !self.fold_anchors.iter().any(|(x, _)| *x == a) {
            self.fold_anchors.push((a, b));
            self.fold_anchors.sort_unstable();
        }
        // Move carets out of the hidden region.
        self.sels.transform(|s| {
            let l = rope.char_to_line(s.head.min(rope.len_chars()));
            if l > start && l < end { Selection::point(a) } else { s }
        });
        self.cache.invalidate();
    }

    /// Remove every fold whose line range (inclusive) contains `line`.
    /// Returns whether anything was unfolded.
    pub fn unfold(&mut self, doc: &Document, line: usize) -> bool {
        let rope = doc.rope();
        let before = self.fold_anchors.len();
        self.fold_anchors.retain(|(a, b)| {
            let (la, lb) = (rope.char_to_line(*a), rope.char_to_line(*b));
            !(line >= la && line <= lb)
        });
        if before != self.fold_anchors.len() {
            self.cache.invalidate();
        }
        before != self.fold_anchors.len()
    }

    /// Remove all folds and invalidate the layout cache.
    pub fn unfold_all(&mut self) {
        self.fold_anchors.clear();
        self.cache.invalidate();
    }

    /// Next visible (non-hidden) line after `line`, if any.
    pub fn next_visible_line(&self, doc: &Document, line: usize) -> Option<usize> {
        let n = doc.len_lines();
        let folds = self.folds(doc);
        let mut l = line + 1;
        while l < n {
            match folds.iter().find(|(a, b)| l > *a && l < *b) {
                // The fold's last line is visible, so jump straight to it.
                Some((_, b)) => l = *b,
                None => return Some(l),
            }
        }
        None
    }

    /// Previous visible (non-hidden) line before `line`, if any.
    pub fn prev_visible_line(&self, doc: &Document, line: usize) -> Option<usize> {
        if line == 0 {
            return None;
        }
        let folds = self.folds(doc);
        let mut l = line - 1;
        loop {
            match folds.iter().find(|(a, b)| l > *a && l < *b) {
                Some((a, _)) => l = *a,
                None => return Some(l),
            }
        }
    }

    /// Replace all selections, request the caret be scrolled into view and
    /// leave Quick Add whole-word mode.
    pub fn set_selections(&mut self, sels: SelectionSet) {
        self.sels = sels;
        self.want_cursor_visible = true;
        self.quick_add_word_mode = false;
    }

    /// Save the current selections for [`View::undo_selection`]; keeps the last 64.
    pub fn push_selection_history(&mut self) {
        self.selection_history.push(self.sels.clone());
        if self.selection_history.len() > 64 {
            self.selection_history.remove(0);
        }
    }

    /// Zero-based line of the primary caret.
    pub fn primary_line(&self, doc: &Document) -> usize {
        doc.rope().char_to_line(self.sels.primary().head.min(doc.len_chars()))
    }

    /// Primary caret as `(line, char column)`, both zero-based.
    pub fn primary_position(&self, doc: &Document) -> (usize, usize) {
        let rope = doc.rope();
        let head = self.sels.primary().head.min(rope.len_chars());
        let line = rope.char_to_line(head);
        (line, head - rope.line_to_char(line))
    }

    // ----- cursor motion -------------------------------------------------

    /// Apply `motion` to every selection. With `extend` the anchor stays put;
    /// without it a non-empty selection collapses (Left/Right collapse to its
    /// edge). Vertical motions skip folded lines and keep a display-column goal
    /// (`tab_width` expands tabs). Requests the caret be made visible.
    pub fn move_cursors(&mut self, doc: &Document, motion: Motion, extend: bool, tab_width: usize) {
        let rope = doc.rope();
        let text = doc.slice();
        let page = self.lines_per_page.max(1);
        let folds = self.folds(doc);
        let n_lines = rope.len_lines();
        // Local copies of `next_visible_line`/`prev_visible_line` that reuse one
        // `folds` computation instead of recomputing it per step.
        let next_visible = |l: usize| -> Option<usize> {
            let mut l = l + 1;
            while l < n_lines {
                match folds.iter().find(|(a, b)| l > *a && l < *b) {
                    Some((_, b)) => l = *b,
                    None => return Some(l),
                }
            }
            None
        };
        let prev_visible = |l: usize| -> Option<usize> {
            if l == 0 {
                return None;
            }
            let mut l = l - 1;
            loop {
                match folds.iter().find(|(a, b)| l > *a && l < *b) {
                    Some((a, _)) => l = *a,
                    None => return Some(l),
                }
            }
        };
        // When Left/Right collapse selections, empty carets stay put: after
        // Split into Lines a blank line's caret would otherwise step onto the
        // previous line while every other line's caret lands at its start.
        let collapsing = !extend && self.sels.iter().any(|s| !s.is_empty());
        self.sels.transform(|s| {
            let head = s.head.min(rope.len_chars());
            let collapse_to = |s: &Selection, start: bool| {
                if !extend && (collapsing || !s.is_empty()) { Some(if start { s.start() } else { s.end() }) } else { None }
            };
            match motion {
                Motion::Left => {
                    let p = collapse_to(&s, true).unwrap_or_else(|| prev_grapheme_boundary(text, head));
                    s.moved_to(p, extend)
                }
                Motion::Right => {
                    let p = collapse_to(&s, false).unwrap_or_else(|| next_grapheme_boundary(text, head));
                    s.moved_to(p, extend)
                }
                Motion::WordLeft => s.moved_to(motion::prev_word_start(text, head), extend),
                Motion::WordRight => s.moved_to(motion::next_word_end(text, head), extend),
                Motion::SubwordLeft => s.moved_to(prev_subword(text, head), extend),
                Motion::SubwordRight => s.moved_to(next_subword(text, head), extend),
                Motion::Up(n) | Motion::Down(n) => {
                    let line = rope.char_to_line(head);
                    let col = head - rope.line_to_char(line);
                    let goal = s.goal_col.unwrap_or_else(|| char_col_to_display_col(rope.line(line), col, tab_width));
                    let mut target = line;
                    for _ in 0..n {
                        let step = if matches!(motion, Motion::Up(_)) { prev_visible(target) } else { next_visible(target) };
                        match step {
                            Some(t) => target = t,
                            None => break,
                        }
                    }
                    // No line to move to: go to the document start/end instead.
                    if target == line {
                        return match motion {
                            Motion::Up(_) => s.moved_to(0, extend).with_goal(Some(goal)),
                            _ => s.moved_to(rope.len_chars(), extend).with_goal(Some(goal)),
                        };
                    }
                    let c = display_col_to_char_col(rope.line(target), goal, tab_width);
                    s.moved_to(rope.line_to_char(target) + c, extend).with_goal(Some(goal))
                }
                // Paging moves by document lines and ignores folds.
                Motion::PageUp => {
                    let line = rope.char_to_line(head);
                    let col = head - rope.line_to_char(line);
                    let goal = s.goal_col.unwrap_or_else(|| char_col_to_display_col(rope.line(line), col, tab_width));
                    let target = line.saturating_sub(page);
                    let c = display_col_to_char_col(rope.line(target), goal, tab_width);
                    s.moved_to(rope.line_to_char(target) + c, extend).with_goal(Some(goal))
                }
                Motion::PageDown => {
                    let line = rope.char_to_line(head);
                    let col = head - rope.line_to_char(line);
                    let goal = s.goal_col.unwrap_or_else(|| char_col_to_display_col(rope.line(line), col, tab_width));
                    let target = (line + page).min(rope.len_lines().saturating_sub(1));
                    let c = display_col_to_char_col(rope.line(target), goal, tab_width);
                    s.moved_to(rope.line_to_char(target) + c, extend).with_goal(Some(goal))
                }
                Motion::LineStart => {
                    // Smart home: toggle between first non-blank and column 0.
                    let line = rope.char_to_line(head);
                    let start = rope.line_to_char(line);
                    let fnb = start + motion::first_non_blank(rope.line(line));
                    let p = if head == fnb || fnb == line_end_char(rope, line) { start } else { fnb };
                    s.moved_to(p, extend)
                }
                Motion::LineEnd => s.moved_to(line_end_char(rope, rope.char_to_line(head)), extend),
                Motion::DocStart => s.moved_to(0, extend),
                Motion::DocEnd => s.moved_to(rope.len_chars(), extend),
                Motion::ParagraphUp => s.moved_to(paragraph_boundary(doc, head, false), extend),
                Motion::ParagraphDown => s.moved_to(paragraph_boundary(doc, head, true), extend),
            }
        });
        self.want_cursor_visible = true;
        self.quick_add_word_mode = false;
    }

    /// Expand every selection to whole lines (including the terminator).
    pub fn select_lines(&mut self, doc: &Document) {
        let rope = doc.rope();
        self.sels.transform(|s| {
            let (a, b) = apex_edit::ops::lines_of(rope, &s);
            let start = rope.line_to_char(a);
            let end = if b + 1 < rope.len_lines() { rope.line_to_char(b + 1) } else { rope.len_chars() };
            // If the selection already covers whole lines, grow by one more line.
            if s.start() == start && s.end() == end && b + 1 < rope.len_lines() {
                let end2 = if b + 2 < rope.len_lines() { rope.line_to_char(b + 2) } else { rope.len_chars() };
                return Selection::new(start, end2);
            }
            Selection::new(start, end)
        });
        self.want_cursor_visible = true;
    }

    /// Expand each selection to whole words: an empty one selects the word at
    /// the caret, a non-empty one grows to cover the words at both ends.
    pub fn select_words(&mut self, doc: &Document) {
        let text = doc.slice();
        self.sels.transform(|s| {
            if s.is_empty() {
                let (a, b) = motion::word_at(text, s.head);
                Selection::new(a, b)
            } else {
                let (a, _) = motion::word_at(text, s.start());
                // Look up the word containing the last selected char, not the one after it.
                let (_, b) = motion::word_at(text, s.end().saturating_sub(1).max(s.start()));
                Selection::new(a.min(s.start()), b.max(s.end()))
            }
        });
    }

    /// Replace all selections with one covering the whole document.
    pub fn select_all(&mut self, doc: &Document) {
        self.sels.set_single(Selection::new(0, doc.len_chars()));
    }

    /// Select the contents of the innermost brackets around each caret; if the
    /// contents are already selected, include the brackets themselves.
    pub fn select_to_brackets(&mut self, doc: &Document) {
        let rope = doc.rope();
        self.sels.transform(|s| match apex_edit::brackets::enclosing_brackets(rope, s.head) {
            Some((o, c)) => {
                if s.start() == o + 1 && s.end() == c {
                    Selection::new(o, c + 1)
                } else {
                    Selection::new(o + 1, c)
                }
            }
            None => s,
        });
        self.want_cursor_visible = true;
    }

    /// Move each caret to its matching bracket (or, failing that, the enclosing
    /// pair): from the opening side to just after the closer, otherwise to the opener.
    pub fn jump_to_bracket(&mut self, doc: &Document, extend: bool) {
        let rope = doc.rope();
        self.sels.transform(|s| {
            let m = apex_edit::brackets::matching_bracket(rope, s.head).or_else(|| apex_edit::brackets::enclosing_brackets(rope, s.head));
            match m {
                Some((o, c)) => {
                    let target = if s.head == o || s.head == o + 1 { c + 1 } else { o };
                    s.moved_to(target, extend)
                }
                None => s,
            }
        });
        self.want_cursor_visible = true;
    }

    /// Expand to the lines sharing the current indentation level (and their parent line).
    pub fn select_indentation(&mut self, doc: &Document) {
        let rope = doc.rope();
        // Indent width in chars, or `None` for an empty line (which never ends a block).
        let indent_of = |line: usize| -> Option<usize> {
            let l = rope.line(line);
            if line_len_chars(rope, line) == 0 { None } else { Some(motion::first_non_blank(l)) }
        };
        self.sels.transform(|s| {
            let (first, last) = apex_edit::ops::lines_of(rope, &s);
            let level = (first..=last).filter_map(indent_of).min().unwrap_or(0);
            let whole_lines = s.start() == rope.line_to_char(first) && s.end() == line_end_char(rope, last);
            if whole_lines {
                // Already a block: grow to the enclosing lines with a smaller indent.
                let mut a = first;
                while a > 0 {
                    a -= 1;
                    if indent_of(a).is_some_and(|i| i < level) {
                        break;
                    }
                }
                let mut b = last;
                while b + 1 < rope.len_lines() {
                    b += 1;
                    if indent_of(b).is_some_and(|i| i < level) {
                        break;
                    }
                }
                // Don't include trailing empty lines.
                while b > a && indent_of(b).is_none() {
                    b -= 1;
                }
                return Selection::new(rope.line_to_char(a), line_end_char(rope, b));
            }
            let mut a = first;
            while a > 0 && indent_of(a - 1).is_none_or(|i| i >= level) {
                a -= 1;
            }
            let mut b = last;
            while b + 1 < rope.len_lines() && indent_of(b + 1).is_none_or(|i| i >= level) {
                b += 1;
            }
            while b > a && indent_of(b).is_none() {
                b -= 1;
            }
            Selection::new(rope.line_to_char(a), line_end_char(rope, b))
        });
        self.want_cursor_visible = true;
    }

    /// Expand each selection to the paragraph boundaries (blank-line separated) around it.
    pub fn select_paragraph(&mut self, doc: &Document) {
        self.sels.transform(|s| Selection::new(paragraph_boundary(doc, s.start(), false), paragraph_boundary(doc, s.end(), true)));
    }

    /// Add a caret one line below (`down`) or above every existing caret, at the
    /// same display column. The newest caret becomes primary; saves selection history.
    pub fn add_cursor_vertically(&mut self, doc: &Document, down: bool, tab_width: usize) {
        let rope = doc.rope();
        self.push_selection_history();
        let mut new = Vec::new();
        for s in self.sels.iter() {
            let line = rope.char_to_line(s.head.min(rope.len_chars()));
            // `wrapping_sub` on line 0 yields usize::MAX, rejected by the bounds check below.
            let target = if down { line + 1 } else { line.wrapping_sub(1) };
            if target >= rope.len_lines() {
                continue;
            }
            let col = s.head - rope.line_to_char(line);
            let goal = s.goal_col.unwrap_or_else(|| char_col_to_display_col(rope.line(line), col, tab_width));
            let c = display_col_to_char_col(rope.line(target), goal, tab_width);
            new.push(Selection::point(rope.line_to_char(target) + c).with_goal(Some(goal)));
        }
        for n in new {
            self.sels.push_primary(n);
        }
        self.want_cursor_visible = true;
    }

    /// Quick Add Next (Ctrl+D): select the word under the caret, or add the
    /// next occurrence of the primary selection as a new selection. With
    /// `skip_current` the primary is replaced rather than kept.
    pub fn add_next_occurrence(&mut self, doc: &Document, skip_current: bool) {
        let rope = doc.rope();
        let text = doc.slice();
        let primary = self.sels.primary();
        if primary.is_empty() {
            let (a, b) = motion::word_at(text, primary.head);
            if a == b {
                return;
            }
            self.push_selection_history();
            let mut ns = self.sels.clone();
            ns.transform(|s| if s == primary { Selection::new(a, b) } else { s });
            self.sels = ns;
            self.quick_add_word_mode = true;
            self.want_cursor_visible = true;
            return;
        }
        let needle: String = rope.slice(primary.range()).into();
        // Byte-length cap keeps the full-text search cheap.
        if needle.is_empty() || needle.len() > 10_000 {
            return;
        }
        let whole_word = self.quick_add_word_mode;
        let search_from = self.sels.iter().map(|s| s.end()).max().unwrap_or(0);
        let Some((a, b)) = find_occurrence(rope, &needle, search_from, whole_word, &self.sels) else { return };
        self.push_selection_history();
        if skip_current {
            self.sels.remove_primary();
        }
        self.sels.push_primary(if primary.is_reversed() { Selection::new(b, a) } else { Selection::new(a, b) });
        self.want_cursor_visible = true;
    }

    /// Select every occurrence of the primary selection (or, if empty, every
    /// whole-word occurrence of the word under the caret). Stops after 100,000
    /// matches; the match at the old primary stays primary.
    pub fn add_all_occurrences(&mut self, doc: &Document) {
        let rope = doc.rope();
        let text = doc.slice();
        let primary = self.sels.primary();
        let (needle, whole_word) = if primary.is_empty() {
            let (a, b) = motion::word_at(text, primary.head);
            if a == b {
                return;
            }
            (rope.slice(a..b).to_string(), true)
        } else {
            (rope.slice(primary.range()).to_string(), false)
        };
        if needle.is_empty() || needle.len() > 10_000 {
            return;
        }
        // Search bytes in a flat `String`, tracking the char index alongside so
        // selections stay in char units.
        let hay = rope.to_string();
        let mut sels = Vec::new();
        let mut byte = 0;
        let mut char_pos = 0;
        let mut limit = 0;
        while let Some(off) = hay[byte..].find(&needle) {
            let start_b = byte + off;
            char_pos += hay[byte..start_b].chars().count();
            let n = needle.chars().count();
            let ok = !whole_word || {
                let (a, b) = motion::word_at(text, char_pos);
                a == char_pos && b == char_pos + n
            };
            if ok {
                sels.push(Selection::new(char_pos, char_pos + n));
            }
            byte = start_b + needle.len();
            char_pos += n;
            limit += 1;
            if limit > 100_000 {
                break;
            }
        }
        if sels.is_empty() {
            return;
        }
        self.push_selection_history();
        let primary_idx = sels.iter().position(|s| s.start() == primary.start()).unwrap_or(0);
        self.sels = SelectionSet::from_vec(sels, primary_idx);
        self.want_cursor_visible = true;
    }

    /// Restore the selections saved by the last [`View::push_selection_history`].
    pub fn undo_selection(&mut self) {
        if let Some(prev) = self.selection_history.pop() {
            self.sels = prev;
            self.want_cursor_visible = true;
        }
    }

    /// Escape: collapse to the primary cursor only.
    pub fn single_selection(&mut self) {
        if self.sels.len() > 1 {
            self.push_selection_history();
            self.sels.keep_primary();
        } else if !self.sels.primary().is_empty() {
            let p = self.sels.primary();
            self.sels.set_single(p.collapse());
        }
        self.want_cursor_visible = true;
    }

    /// Column selection: grow the block of per-line
    /// selections by one line, copying the primary's anchor and head display
    /// columns, or shrink it if the primary is at the opposite edge.
    pub fn column_select(&mut self, doc: &Document, down: bool, tab_width: usize) {
        let rope = doc.rope();
        // Extend the block of cursors by one line in the given direction, or
        // shrink if the block already extends the other way.
        let lines: Vec<usize> = self.sels.iter().map(|s| rope.char_to_line(s.head.min(rope.len_chars()))).collect();
        let (min_l, max_l) = (*lines.iter().min().unwrap_or(&0), *lines.iter().max().unwrap_or(&0));
        let primary_line = self.primary_line(doc);
        self.push_selection_history();
        let goal = {
            let s = self.sels.primary();
            let line = rope.char_to_line(s.head.min(rope.len_chars()));
            s.goal_col.unwrap_or_else(|| char_col_to_display_col(rope.line(line), s.head - rope.line_to_char(line), tab_width))
        };
        let anchor_goal = {
            let s = self.sels.primary();
            let line = rope.char_to_line(s.anchor.min(rope.len_chars()));
            char_col_to_display_col(rope.line(line), s.anchor - rope.line_to_char(line), tab_width)
        };
        let extend_from = if down { max_l } else { min_l };
        let shrinking = (down && primary_line == min_l && max_l > min_l) || (!down && primary_line == max_l && max_l > min_l);
        let target = if shrinking {
            None
        } else if down {
            (extend_from + 1 < rope.len_lines()).then_some(extend_from + 1)
        } else {
            extend_from.checked_sub(1)
        };
        if shrinking {
            let remove_line = if down { min_l } else { max_l };
            let keep: Vec<Selection> = self.sels.iter().copied().filter(|s| rope.char_to_line(s.head.min(rope.len_chars())) != remove_line).collect();
            if !keep.is_empty() {
                let p = if down { keep.len() - 1 } else { 0 };
                self.sels = SelectionSet::from_vec(keep, if down { 0 } else { p });
            }
        } else if let Some(t) = target {
            let line_start = rope.line_to_char(t);
            let head = line_start + display_col_to_char_col(rope.line(t), goal, tab_width);
            let anchor = line_start + display_col_to_char_col(rope.line(t), anchor_goal, tab_width);
            self.sels.push_primary(Selection::new(anchor, head).with_goal(Some(goal)));
        }
        self.want_cursor_visible = true;
    }

    /// Scroll by whole rows (negative = up). Wrapped rows are handled by the editor widget.
    pub fn scroll_lines(&mut self, lines: f32, line_height: f32) {
        self.scroll_remainder += lines * line_height;
    }
}

/// Find `needle` after `from` (wrapping), skipping ranges already selected.
fn find_occurrence(rope: &apex_buffer::Rope, needle: &str, from: usize, whole_word: bool, sels: &SelectionSet) -> Option<(usize, usize)> {
    let text = rope.slice(..);
    let hay = rope.to_string();
    let n = needle.chars().count();
    let from_b = rope.char_to_byte(from.min(rope.len_chars()));
    let check = |char_pos: usize| -> bool {
        if sels.iter().any(|s| s.start() == char_pos && s.end() == char_pos + n) {
            return false;
        }
        if whole_word {
            let (a, b) = motion::word_at(text, char_pos);
            return a == char_pos && b == char_pos + n;
        }
        true
    };
    let scan = |range_start: usize, range_end: usize| -> Option<(usize, usize)> {
        let mut byte = range_start;
        let mut char_pos = rope.byte_to_char(range_start);
        while byte < range_end {
            let off = hay[byte..range_end].find(needle)?;
            let start_b = byte + off;
            char_pos += hay[byte..start_b].chars().count();
            if check(char_pos) {
                return Some((char_pos, char_pos + n));
            }
            byte = start_b + needle.len().max(1);
            char_pos += n.max(1);
        }
        None
    };
    scan(from_b, hay.len()).or_else(|| scan(0, from_b.min(hay.len())))
}

/// Char index of the paragraph boundary from char index `pos`. Forward: the
/// start of the next non-blank line after the following blank run, or the
/// document end. Backward: the start of the current/previous paragraph's first line.
fn paragraph_boundary(doc: &Document, pos: usize, forward: bool) -> usize {
    let rope = doc.rope();
    let is_blank = |l: usize| rope.line(l).chars().all(|c| c.is_whitespace());
    let mut line = rope.char_to_line(pos.min(rope.len_chars()));
    let last = rope.len_lines().saturating_sub(1);
    if forward {
        while line < last && !is_blank(line) {
            line += 1;
        }
        while line < last && is_blank(line) {
            line += 1;
        }
        if line == last && !is_blank(line) {
            return rope.len_chars();
        }
        rope.line_to_char(line)
    } else {
        line = line.saturating_sub(1);
        while line > 0 && is_blank(line) {
            line -= 1;
        }
        while line > 0 && !is_blank(line - 1) {
            line -= 1;
        }
        rope.line_to_char(line)
    }
}

/// Subword motion: stops at camelCase humps and underscores inside identifiers.
fn next_subword(text: apex_buffer::RopeSlice<'_>, pos: usize) -> usize {
    let len = text.len_chars();
    let mut prev: Option<char> = None;
    let mut started = false;
    for (i, c) in (pos.min(len)..).zip(text.chars_at(pos.min(len))) {
        let boundary = match prev {
            None => false,
            Some(p) => {
                (p.is_lowercase() && c.is_uppercase())
                    || (p == '_' && c != '_')
                    || (c == '_' && p != '_')
                    || (p.is_alphanumeric() != c.is_alphanumeric())
                    || (c.is_whitespace() != p.is_whitespace())
            }
        };
        // Leading whitespace is skipped before any boundary counts.
        if boundary && started {
            return i;
        }
        if !c.is_whitespace() {
            started = true;
        }
        prev = Some(c);
    }
    len
}

/// Backward counterpart of [`next_subword`]; returns a char index.
fn prev_subword(text: apex_buffer::RopeSlice<'_>, pos: usize) -> usize {
    let mut i = pos.min(text.len_chars());
    let mut it = text.chars_at(i);
    let mut next: Option<char> = None;
    let mut started = false;
    while let Some(c) = it.prev() {
        let boundary = match next {
            None => false,
            Some(nx) => {
                (c.is_lowercase() && nx.is_uppercase())
                    || (c == '_' && nx != '_')
                    || (nx == '_' && c != '_')
                    || (c.is_alphanumeric() != nx.is_alphanumeric())
                    || (c.is_whitespace() != nx.is_whitespace())
            }
        };
        if boundary && started {
            return i;
        }
        if !c.is_whitespace() {
            started = true;
        }
        next = Some(c);
        i -= 1;
    }
    0
}

/// Caret motion for [`View::move_cursors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    /// Up by this many visible lines.
    Up(usize),
    /// Down by this many visible lines.
    Down(usize),
    /// To the previous word start.
    WordLeft,
    /// To the next word end.
    WordRight,
    /// To the previous camelCase/underscore boundary.
    SubwordLeft,
    /// To the next camelCase/underscore boundary.
    SubwordRight,
    /// Smart home: toggles between first non-blank and column 0.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    /// Up by `lines_per_page` document lines.
    PageUp,
    /// Down by `lines_per_page` document lines.
    PageDown,
    /// To the previous paragraph start (see `paragraph_boundary`).
    ParagraphUp,
    /// To the next paragraph start (see `paragraph_boundary`).
    ParagraphDown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focusing_a_new_view_keeps_the_accessibility_tree_valid() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let view = ViewId::next();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| view.request_focus(ui.ctx()));
        output.textures_delta.clear();
        let update = output.platform_output.accesskit_update.unwrap();
        assert_eq!(update.focus, view.egui_id().accesskit_id());
        assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
    }

    /// A view of `doc` at its current revision.
    fn view_for(doc: &Document) -> View {
        View::new(doc.id, doc.revision())
    }

    /// Up/Down through a short line restores the original display column.
    #[test]
    fn vertical_motion_keeps_goal_column() {
        let d = Document::from_text("longer line\nab\nanother long");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::point(8));
        v.move_cursors(&d, Motion::Down(1), false, 4);
        assert_eq!(v.sels.primary().head, 14); // end of "ab"
        v.move_cursors(&d, Motion::Down(1), false, 4);
        assert_eq!(v.sels.primary().head, 15 + 8);
        v.move_cursors(&d, Motion::Up(1), true, 4);
        assert_eq!(v.sels.primary(), Selection::new(23, 14).with_goal(Some(8)));
    }

    /// Home alternates between first non-blank and column 0.
    #[test]
    fn smart_home_toggles() {
        let d = Document::from_text("    x");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::point(5));
        v.move_cursors(&d, Motion::LineStart, false, 4);
        assert_eq!(v.sels.primary().head, 4);
        v.move_cursors(&d, Motion::LineStart, false, 4);
        assert_eq!(v.sels.primary().head, 0);
        v.move_cursors(&d, Motion::LineStart, false, 4);
        assert_eq!(v.sels.primary().head, 4);
    }

    /// Split into Lines then Left puts every caret at its own line's start;
    /// a blank line's (empty) caret must not step back onto the line above.
    #[test]
    fn left_after_split_keeps_blank_line_carets() {
        let d = Document::from_text("ab\n\ncd\n");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::new(0, 6));
        v.sels = apex_edit::ops::split_into_lines(&d, &v.sels);
        v.move_cursors(&d, Motion::Left, false, 4);
        let heads: Vec<usize> = v.sels.iter().map(|s| s.head).collect();
        assert_eq!(heads, vec![0, 3, 4]);
        // A lone caret still moves.
        v.sels.set_single(Selection::point(3));
        v.move_cursors(&d, Motion::Left, false, 4);
        assert_eq!(v.sels.primary().head, 2);
    }

    /// Quick Add Next, skip, undo and add-all on repeated words.
    #[test]
    fn quick_add_next_and_all() {
        let d = Document::from_text("foo bar foo foobar foo");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::point(1));
        v.add_next_occurrence(&d, false);
        assert_eq!(v.sels.primary(), Selection::new(0, 3));
        v.add_next_occurrence(&d, false);
        assert_eq!(v.sels.len(), 2);
        assert_eq!(v.sels.primary(), Selection::new(8, 11));
        v.add_next_occurrence(&d, true); // skip 8..11, take 19..22 (whole word: skips foobar)
        assert_eq!(v.sels.len(), 2);
        assert_eq!(v.sels.primary(), Selection::new(19, 22));
        v.undo_selection();
        assert_eq!(v.sels.len(), 2);
        v.sels.set_single(Selection::point(1));
        v.add_all_occurrences(&d);
        assert_eq!(v.sels.len(), 3);
    }

    /// Adding carets below and growing/shrinking a column selection.
    #[test]
    fn add_cursor_below_and_column_select() {
        let d = Document::from_text("abc\nde\nfghij");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::point(2));
        v.add_cursor_vertically(&d, true, 4);
        assert_eq!(v.sels.iter().map(|s| s.with_goal(None)).collect::<Vec<_>>(), vec![Selection::point(2), Selection::point(6)]);
        v.sels.set_single(Selection::new(0, 2));
        v.column_select(&d, true, 4);
        v.column_select(&d, true, 4);
        assert_eq!(v.sels.len(), 3);
        assert_eq!(v.sels.as_slice()[2].with_goal(None), Selection::new(7, 9));
        v.column_select(&d, false, 4);
        assert_eq!(v.sels.len(), 2);
    }

    /// Repeated line selection grows by one line each time.
    #[test]
    fn select_lines_grows() {
        let d = Document::from_text("a\nb\nc");
        let mut v = view_for(&d);
        v.select_lines(&d);
        assert_eq!(v.sels.primary(), Selection::new(0, 2));
        v.select_lines(&d);
        assert_eq!(v.sels.primary(), Selection::new(0, 4));
    }

    /// Subword stops at camelCase humps and underscores.
    #[test]
    fn subword_motion() {
        let r = apex_buffer::Rope::from_str("someCamelCase_snake x");
        let s = r.slice(..);
        assert_eq!(next_subword(s, 0), 4);
        assert_eq!(next_subword(s, 4), 9);
        assert_eq!(next_subword(s, 9), 13);
        assert_eq!(prev_subword(s, 13), 9);
        assert_eq!(prev_subword(s, 4), 0);
    }

    /// Indentation selection takes the block, then its enclosing lines.
    #[test]
    fn indentation_selection() {
        let d = Document::from_text("fn x() {\n    a;\n    b;\n}\n");
        let mut v = view_for(&d);
        v.sels.set_single(Selection::point(14));
        v.select_indentation(&d);
        assert_eq!(v.sels.primary(), Selection::new(9, 22));
        v.select_indentation(&d);
        assert_eq!(v.sels.primary(), Selection::new(0, 24));
    }
}
