//! The custom editor widget: viewport-only layout, soft wrap, folding,
//! multi-cursor rendering, minimap, mouse and keyboard input.
//!
//! # Display model
//!
//! The viewport is anchored at a document line and a visual row within it
//! (`View::top_line` / `View::top_row`) rather than a pixel offset, so soft
//! wrapped lines and folded regions cost nothing to skip and only the lines
//! that are actually on screen are ever laid out. Horizontal scrolling is in
//! pixels and only applies when wrapping is off.
//!
//! Input and painting are separate passes so that the application can execute
//! the actions produced by input (which may mutate the document) before the
//! frame is painted.

use std::sync::Arc;

use apex_buffer::motion::{self, snap_to_grapheme};
use apex_buffer::position::{char_col_to_display_col, char_display_width, display_col_to_char_col, line_end_char, line_len_chars};
use apex_edit::{Document, Selection, SelectionSet};
use egui::text::{CCursor, CharIndex, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{Color32, Event, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::command::Command;
use crate::keymap::{Context, Dispatch, Keymap};
use crate::settings::{Settings, WhitespaceMode};
use crate::theme::Theme;
use crate::view::{DragMode, DragState, LineLayout, View};

/// Lines longer than this are laid out only up to the limit; the rest is
/// reachable by keyboard but not drawn. Keeps a 1 MB line from stalling the UI.
pub const MAX_LAYOUT_CHARS: usize = 20_000;

const GUTTER_PAD: f32 = 10.0;
const TEXT_PAD: f32 = 6.0;
const SCROLLBAR_W: f32 = 10.0;
const FOLD_W: f32 = 14.0;
const MINIMAP_LINE_H: f32 = 2.0;
const MINIMAP_CHAR_W: f32 = 1.0;

/// Per-frame styling inputs for the widget.
pub struct EditorStyle<'a> {
    pub theme: &'a Theme,
    pub settings: &'a Settings,
    pub font_id: egui::FontId,
    pub line_height: f32,
    pub char_width: f32,
    /// This view is the focused editor of the active pane.
    pub focused: bool,
    /// Search matches to highlight (char ranges), and which one is current.
    pub matches: &'a [std::ops::Range<usize>],
    pub current_match: Option<usize>,
    /// Lines with git markers: (line, kind) where kind 0=added 1=modified 2=deleted-after.
    pub git_marks: &'a [(usize, u8)],
    /// Diagnostics to underline: (char start, char end, severity 1=error 2=warning other=info).
    pub diagnostics: &'a [(usize, usize, u8)],
    /// Fold regions available (start line → end line), from the syntax layer.
    pub fold_regions: &'a [(usize, usize)],
    pub wrap: bool,
    pub large_file: bool,
}

/// What the widget asks the application to do, in input order.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Insert(String),
    Paste(String),
    Command(Command, Option<String>),
    /// The user clicked in this view; the application should make it active.
    Focus,
    Dropped(Vec<std::path::PathBuf>),
    ContextMenu(Pos2),
    ImeCommit(String),
    /// Fold toggled from the gutter at this line.
    ToggleFold(usize),
    /// Hover position changed: (char offset) for hover information requests.
    Hover(Option<usize>),
}

/// Provides syntax colouring for a line: returns `(byte_start, byte_end, style)`
/// spans in line-local byte offsets, sorted and non-overlapping.
pub type HighlightFn<'a> = dyn Fn(usize) -> Vec<(usize, usize, crate::theme::SyntaxStyle)> + 'a;

/// Geometry shared by the input and paint passes.
#[derive(Clone, Copy)]
pub struct Geometry {
    pub rect: Rect,
    pub gutter_w: f32,
    pub text: Rect,
    pub minimap: Option<Rect>,
    pub line_height: f32,
    pub total_lines: usize,
    /// Wrap width in points, if wrapping.
    pub wrap_width: Option<f32>,
}

pub fn geometry(rect: Rect, doc: &Document, style: &EditorStyle) -> Geometry {
    let total_lines = doc.len_lines();
    let digits = total_lines.max(1).to_string().len().max(2);
    let fold_w = if style.settings.line_numbers { FOLD_W } else { 0.0 };
    let gutter_w = if style.settings.line_numbers { digits as f32 * style.char_width + GUTTER_PAD * 2.0 + fold_w } else { GUTTER_PAD + 8.0 };
    let minimap_w = if style.settings.minimap && !style.large_file && rect.width() > 500.0 { style.settings.minimap_width } else { 0.0 };
    let text = Rect::from_min_max(pos2(rect.min.x + gutter_w, rect.min.y), pos2(rect.max.x - SCROLLBAR_W - minimap_w, rect.max.y));
    let minimap = (minimap_w > 0.0).then(|| Rect::from_min_max(pos2(text.max.x, rect.min.y), pos2(rect.max.x - SCROLLBAR_W, rect.max.y)));
    let wrap_width = if style.wrap {
        let w = if style.settings.wrap_column > 0 { style.settings.wrap_column as f32 * style.char_width } else { text.width() - TEXT_PAD * 2.0 };
        Some(w.max(style.char_width * 8.0))
    } else {
        None
    };
    Geometry { rect, gutter_w, text, minimap, line_height: style.line_height, total_lines, wrap_width }
}

impl Geometry {
    pub fn rows_fit(&self) -> usize {
        ((self.text.height() / self.line_height).floor() as usize).max(1)
    }
}

/// Expand tabs and substitute control characters so the galley has one glyph
/// per displayed cell. Returns the text and an optional doc→galley column map.
fn expand_for_layout(line: apex_buffer::RopeSlice<'_>, len: usize, tab_width: usize) -> (String, Option<Vec<u32>>) {
    let mut out = String::with_capacity(len + 8);
    let mut map: Option<Vec<u32>> = None;
    let mut gcol: u32 = 0;
    for (dcol, c) in line.chars().take(len.min(MAX_LAYOUT_CHARS)).enumerate() {
        match c {
            '\t' => {
                if map.is_none() {
                    map = Some((0..=dcol as u32).collect());
                }
                let n = char_display_width('\t', gcol as usize, tab_width);
                for _ in 0..n {
                    out.push(' ');
                }
                gcol += n as u32;
            }
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push(char::from_u32(0x2400 + (c as u32).min(0x20)).unwrap_or('\u{FFFD}'));
                gcol += 1;
            }
            c => {
                out.push(c);
                gcol += 1;
            }
        }
        if let Some(m) = &mut map {
            m.push(gcol);
        }
    }
    (out, map)
}

/// Lay out one line, consulting the cache. A cached layout built without
/// highlighting is replaced once a highlighter is available.
pub fn layout_line<'a>(
    ui: &Ui,
    geo: &Geometry,
    view: &'a mut View,
    doc: &Document,
    line: usize,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
) -> &'a LineLayout {
    let same_wrap = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() < 0.5,
        _ => false,
    };
    let needs = match view.cache.get(line) {
        Some(l) => (!l.highlighted && highlight.is_some()) || !same_wrap(l.wrap_width, geo.wrap_width),
        None => true,
    };
    if needs {
        let rope = doc.rope();
        let len = line_len_chars(rope, line);
        let slice = rope.line(line);
        let (text, map) = expand_for_layout(slice, len, style.settings.tab_width);
        let mut job = LayoutJob {
            wrap: match geo.wrap_width {
                Some(w) => TextWrapping { max_width: w, max_rows: 2000, break_anywhere: false, overflow_character: None },
                None => TextWrapping::no_max_width(),
            },
            break_on_newline: false,
            round_output_to_gui: true,
            ..Default::default()
        };
        let base_colour = style.theme.editor.foreground;
        let font = style.font_id.clone();
        let lh = style.line_height;
        let push = |job: &mut LayoutJob, start: usize, end: usize, st: Option<crate::theme::SyntaxStyle>| {
            if start >= end {
                return;
            }
            let mut fmt = TextFormat { font_id: font.clone(), color: st.map(|s| s.fg).unwrap_or(base_colour), line_height: Some(lh), ..Default::default() };
            if let Some(s) = st {
                fmt.italics = s.italic;
                if s.underline {
                    fmt.underline = Stroke::new(1.0, s.fg);
                }
            }
            job.sections.push(LayoutSection { leading_space: 0.0, byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(end), format: fmt });
        };
        let spans = highlight.map(|h| h(line)).unwrap_or_default();
        if spans.is_empty() || len > MAX_LAYOUT_CHARS {
            push(&mut job, 0, text.len(), None);
        } else if let Some(m) = map.as_ref() {
            // With tabs the byte offsets shift; remap spans through the char map.
            let line_str: String = slice.chars().take(len).collect();
            let mut byte_to_char = vec![0u32; line_str.len() + 1];
            let mut ci = 0u32;
            for (bi, _) in line_str.char_indices() {
                byte_to_char[bi] = ci;
                ci += 1;
            }
            byte_to_char[line_str.len()] = ci;
            let g_char_to_byte: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
            let mut cursor = 0usize;
            for (bs, be, st) in spans {
                let cs = byte_to_char.get(bs).copied().unwrap_or(ci) as usize;
                let ce = byte_to_char.get(be).copied().unwrap_or(ci) as usize;
                let gs = g_char_to_byte[(m.get(cs).copied().unwrap_or(0) as usize).min(g_char_to_byte.len() - 1)];
                let ge = g_char_to_byte[(m.get(ce).copied().unwrap_or(0) as usize).min(g_char_to_byte.len() - 1)];
                push(&mut job, cursor, gs, None);
                push(&mut job, gs.max(cursor), ge, Some(st));
                cursor = ge.max(cursor);
            }
            push(&mut job, cursor, text.len(), None);
        } else {
            let mut cursor = 0usize;
            for (bs, be, st) in spans {
                let bs = bs.min(text.len());
                let be = be.min(text.len());
                push(&mut job, cursor, bs, None);
                push(&mut job, bs.max(cursor), be, Some(st));
                cursor = be.max(cursor);
            }
            push(&mut job, cursor, text.len(), None);
        }
        if job.sections.is_empty() {
            push(&mut job, 0, 0, None);
        }
        job.text = text;
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        let rows = galley.rows.len().max(1);
        let epoch = view.cache.epoch;
        view.cache.insert(line, LineLayout { galley, rows, map, epoch, highlighted: highlight.is_some(), wrap_width: geo.wrap_width });
    }
    view.cache.get(line).expect("just inserted")
}

/// A visual row on screen.
#[derive(Clone, Copy, Debug)]
pub struct VisibleRow {
    pub line: usize,
    pub row: usize,
    pub y: f32,
    /// This row is the last row of its line.
    pub last_row: bool,
}

/// Compute the rows that fit in the viewport from the current anchor, laying
/// out lines as needed. Also normalises the anchor onto a visible line.
pub fn visible_rows(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>) -> Vec<VisibleRow> {
    let mut out = Vec::with_capacity(geo.rows_fit() + 2);
    let n = geo.total_lines;
    if n == 0 {
        return out;
    }
    view.top_line = view.top_line.min(n - 1);
    if view.is_line_hidden(doc, view.top_line) {
        view.top_line = view.prev_visible_line(doc, view.top_line).unwrap_or(0);
        view.top_row = 0;
    }
    let mut line = view.top_line;
    let mut row = view.top_row;
    let mut y = geo.text.min.y;
    let lh = geo.line_height;
    loop {
        let rows = layout_line(ui, geo, view, doc, line, style, highlight).rows;
        if row >= rows {
            row = 0;
        }
        if line == view.top_line {
            view.top_row = row.min(rows - 1);
            row = view.top_row;
        }
        while row < rows && y < geo.text.max.y + lh {
            out.push(VisibleRow { line, row, y, last_row: row + 1 == rows });
            y += lh;
            row += 1;
        }
        if y >= geo.text.max.y + lh {
            break;
        }
        match view.next_visible_line(doc, line) {
            Some(l) => {
                line = l;
                row = 0;
            }
            None => break,
        }
    }
    out
}

/// Lines that will be drawn (for callers that want to precompute per-line data).
pub fn visible_lines(ui: &Ui, rect: Rect, view: &mut View, doc: &Document, style: &EditorStyle) -> Vec<usize> {
    let geo = geometry(rect, doc, style);
    let mut lines: Vec<usize> = visible_rows(ui, &geo, view, doc, style, None).iter().map(|r| r.line).collect();
    lines.dedup();
    lines
}

/// Scroll the anchor by `delta` rows (positive = down).
fn scroll_rows(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>, delta: i64) {
    if delta == 0 || geo.total_lines == 0 {
        return;
    }
    let mut remaining = delta;
    if remaining > 0 {
        // Do not scroll past the end: keep at least one row on screen (or allow
        // scrolling the last line to the top when scroll_past_end is set).
        let max_top = geo.total_lines - 1;
        while remaining > 0 {
            let rows = layout_line(ui, geo, view, doc, view.top_line, style, highlight).rows;
            if view.top_row + 1 < rows {
                view.top_row += 1;
            } else {
                match view.next_visible_line(doc, view.top_line) {
                    Some(l) if l <= max_top => {
                        view.top_line = l;
                        view.top_row = 0;
                    }
                    _ => break,
                }
            }
            remaining -= 1;
        }
        if !style.settings.scroll_past_end {
            // Pull back so the viewport is full when possible.
            clamp_top_to_content(ui, geo, view, doc, style, highlight);
        }
    } else {
        while remaining < 0 {
            if view.top_row > 0 {
                view.top_row -= 1;
            } else {
                match view.prev_visible_line(doc, view.top_line) {
                    Some(l) => {
                        view.top_line = l;
                        view.top_row = layout_line(ui, geo, view, doc, l, style, highlight).rows - 1;
                    }
                    None => break,
                }
            }
            remaining += 1;
        }
    }
}

/// Ensure the viewport is not showing more empty space below the last line
/// than necessary.
fn clamp_top_to_content(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>) {
    let fit = geo.rows_fit();
    // Count rows from the anchor to the end.
    let mut rows_below = 0usize;
    let mut line = view.top_line;
    let mut first = true;
    loop {
        let rows = layout_line(ui, geo, view, doc, line, style, highlight).rows;
        rows_below += if first { rows - view.top_row.min(rows - 1) } else { rows };
        first = false;
        if rows_below >= fit {
            return;
        }
        match view.next_visible_line(doc, line) {
            Some(l) => line = l,
            None => break,
        }
    }
    // Not enough content below: move the anchor up until it is.
    let mut deficit = fit - rows_below;
    while deficit > 0 {
        if view.top_row > 0 {
            view.top_row -= 1;
        } else {
            match view.prev_visible_line(doc, view.top_line) {
                Some(l) => {
                    view.top_line = l;
                    view.top_row = layout_line(ui, geo, view, doc, l, style, highlight).rows - 1;
                }
                None => break,
            }
        }
        deficit -= 1;
    }
}

/// Where a char offset sits: its line, row within the line and x offset.
fn locate(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>, pos: usize) -> (usize, usize, f32) {
    let rope = doc.rope();
    let pos = pos.min(rope.len_chars());
    let line = rope.char_to_line(pos);
    let col = pos - rope.line_to_char(line);
    let layout = layout_line(ui, geo, view, doc, line, style, highlight);
    let g = layout.to_galley_col(col);
    let c = CCursor { index: CharIndex(g), prefer_next_row: false };
    let lc = layout.galley.layout_from_cursor(c);
    let x = layout.galley.pos_from_cursor(c).min.x;
    (line, lc.row.min(layout.rows - 1), x)
}

/// Adjust the anchor so the primary caret is visible (or centred).
pub fn scroll_to_cursor(ui: &Ui, rect: Rect, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>) {
    if !(view.want_cursor_visible || view.want_centre) {
        return;
    }
    let geo = geometry(rect, doc, style);
    if geo.total_lines == 0 {
        return;
    }
    let head = view.sels.primary().head;
    let (line, row, x) = locate(ui, &geo, view, doc, style, highlight, head);
    // Unfold if the caret is inside a fold (sync normally handles it; be safe).
    if view.is_line_hidden(doc, line) {
        view.unfold(doc, line);
    }
    let fit = geo.rows_fit();
    let centre = view.want_centre;
    view.want_cursor_visible = false;
    view.want_centre = false;
    // Position of the caret row relative to the anchor, counting visible rows.
    let rel = rows_between(ui, &geo, view, doc, style, highlight, (view.top_line, view.top_row), (line, row));
    match rel {
        Some(r) if !centre && r >= 1 && r + 2 < fit as i64 => {}
        Some(r) if !centre && r < 1 => {
            // Above (or at) the top: put the caret one row below the top.
            scroll_rows(ui, &geo, view, doc, style, highlight, r - 1);
        }
        Some(r) if !centre => {
            scroll_rows(ui, &geo, view, doc, style, highlight, r + 3 - fit as i64);
        }
        _ => {
            // Centre: anchor at caret then move up half a page.
            view.top_line = line;
            view.top_row = row;
            scroll_rows(ui, &geo, view, doc, style, highlight, -((fit / 2) as i64));
        }
    }
    // Horizontal.
    if geo.wrap_width.is_none() {
        if x < view.scroll.x + style.char_width {
            view.scroll.x = (x - style.char_width * 4.0).max(0.0);
        } else if x > view.scroll.x + geo.text.width() - TEXT_PAD * 2.0 - style.char_width {
            view.scroll.x = x - geo.text.width() + TEXT_PAD * 2.0 + style.char_width * 4.0;
        }
    } else {
        view.scroll.x = 0.0;
    }
}

/// Signed number of visible rows from `a` to `b` (negative if `b` is above),
/// or `None` if the distance exceeds a few pages (then the caller recentres).
#[allow(clippy::too_many_arguments)]
fn rows_between(
    ui: &Ui,
    geo: &Geometry,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
    a: (usize, usize),
    b: (usize, usize),
) -> Option<i64> {
    let limit = (geo.rows_fit() * 4) as i64;
    if b.0 == a.0 {
        return Some(b.1 as i64 - a.1 as i64);
    }
    if b.0 > a.0 {
        let mut rows = layout_line(ui, geo, view, doc, a.0, style, highlight).rows as i64 - a.1 as i64;
        let mut line = view.next_visible_line(doc, a.0)?;
        while line < b.0 {
            rows += layout_line(ui, geo, view, doc, line, style, highlight).rows as i64;
            if rows > limit {
                return None;
            }
            line = view.next_visible_line(doc, line)?;
        }
        if line != b.0 {
            return None; // b is hidden
        }
        Some(rows + b.1 as i64)
    } else {
        let mut rows = -(a.1 as i64);
        let mut line = view.prev_visible_line(doc, a.0)?;
        loop {
            let r = layout_line(ui, geo, view, doc, line, style, highlight).rows as i64;
            if line == b.0 {
                return Some(rows - (r - b.1 as i64));
            }
            rows -= r;
            if rows < -limit {
                return None;
            }
            line = view.prev_visible_line(doc, line)?;
        }
    }
}

/// Document position under a screen point.
#[allow(clippy::too_many_arguments)]
fn pos_at(
    ui: &Ui,
    geo: &Geometry,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
    rows: &[VisibleRow],
    p: Pos2,
) -> usize {
    let rope = doc.rope();
    if geo.total_lines == 0 || rows.is_empty() {
        return 0;
    }
    let lh = geo.line_height;
    let vr = rows.iter().find(|r| p.y >= r.y && p.y < r.y + lh).copied().unwrap_or(if p.y < rows[0].y { rows[0] } else { rows[rows.len() - 1] });
    let len = line_len_chars(rope, vr.line);
    let gx = p.x - geo.text.min.x - TEXT_PAD + view.scroll.x;
    let layout = layout_line(ui, geo, view, doc, vr.line, style, highlight);
    let gy = layout.galley.rows.get(vr.row).map(|r| r.pos.y + lh * 0.5).unwrap_or(lh * 0.5);
    let gcol = layout.galley.cursor_from_pos(vec2(gx, gy)).index.0;
    let col = layout.to_doc_col(gcol).min(len);
    let pos = rope.line_to_char(vr.line) + col;
    snap_to_grapheme(rope.slice(..), pos)
}

/// Move carets up/down by visual rows (used when wrapping or folding is active).
#[allow(clippy::too_many_arguments)]
fn move_visual(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>, delta: i64, extend: bool) {
    let rope = doc.rope();
    let sels: Vec<Selection> = view.sels.iter().copied().collect();
    let mut out = Vec::with_capacity(sels.len());
    for s in sels {
        let head = if !extend && !s.is_empty() { if delta < 0 { s.start() } else { s.end() } } else { s.head };
        let (line, row, x) = locate(ui, geo, view, doc, style, highlight, head);
        let goal_x = s.goal_col.map(|g| g as f32 * style.char_width).unwrap_or(x);
        let mut cur = (line, row);
        let mut steps = delta.abs();
        while steps > 0 {
            if delta < 0 {
                if cur.1 > 0 {
                    cur.1 -= 1;
                } else {
                    match view.prev_visible_line(doc, cur.0) {
                        Some(l) => cur = (l, layout_line(ui, geo, view, doc, l, style, highlight).rows - 1),
                        None => break,
                    }
                }
            } else {
                let rows = layout_line(ui, geo, view, doc, cur.0, style, highlight).rows;
                if cur.1 + 1 < rows {
                    cur.1 += 1;
                } else {
                    match view.next_visible_line(doc, cur.0) {
                        Some(l) => cur = (l, 0),
                        None => break,
                    }
                }
            }
            steps -= 1;
        }
        let target = if cur == (line, row) {
            if delta < 0 { 0 } else { rope.len_chars() }
        } else {
            let layout = layout_line(ui, geo, view, doc, cur.0, style, highlight);
            let gy = layout.galley.rows.get(cur.1).map(|r| r.pos.y + geo.line_height * 0.5).unwrap_or(0.0);
            let gcol = layout.galley.cursor_from_pos(vec2(goal_x, gy)).index.0;
            let col = layout.to_doc_col(gcol).min(line_len_chars(rope, cur.0));
            rope.line_to_char(cur.0) + col
        };
        let goal = Some((goal_x / style.char_width.max(1.0)).round() as usize);
        out.push(s.moved_to(target, extend).with_goal(goal));
    }
    view.sels = SelectionSet::from_vec(out, view.sels.primary_index());
    view.want_cursor_visible = true;
}

/// Process input for this frame. Mouse selection is applied to `view`
/// directly; everything that edits the document becomes an [`Action`].
#[allow(clippy::too_many_arguments)]
pub fn input(
    ui: &mut Ui,
    rect: Rect,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    keymap: &mut Keymap,
    highlight: Option<&HighlightFn>,
    out: &mut Vec<Action>,
) {
    let geo = geometry(rect, doc, style);
    let id = view.id.egui_id();
    let response = ui.interact(rect, id, Sense::click_and_drag());
    // egui clears focus for ids that do not declare interest every frame.
    let layer = ui.layer_id();
    ui.memory_mut(|m| m.interested_in_focus(id, layer));
    let has_focus = ui.memory(|m| m.has_focus(id));
    ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
    let rope = doc.rope();
    let tab_width = style.settings.tab_width;
    let now = ui.input(|i| i.time);
    let lh = geo.line_height;

    // --- Scrolling -----------------------------------------------------
    if response.hovered() {
        let (delta, zoom, mods) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
        if zoom != 1.0 {
            out.push(Action::Command(if zoom > 1.0 { Command::ZoomIn } else { Command::ZoomOut }, None));
        } else if delta != Vec2::ZERO {
            let d = if mods.shift && delta.x == 0.0 { vec2(delta.y, 0.0) } else { delta };
            if geo.wrap_width.is_none() {
                view.scroll.x = (view.scroll.x - d.x).max(0.0);
            }
            view.scroll_remainder -= d.y;
        }
    }
    // Apply accumulated vertical scroll in whole rows.
    if view.scroll_remainder.abs() >= lh {
        let rows = (view.scroll_remainder / lh).trunc() as i64;
        view.scroll_remainder -= rows as f32 * lh;
        scroll_rows(ui, &geo, view, doc, style, highlight, rows);
    }
    if ui.input(|i| !i.raw.dropped_files.is_empty()) && response.hovered() {
        let paths: Vec<_> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !paths.is_empty() {
            out.push(Action::Dropped(paths));
        }
    }

    // --- Mouse ----------------------------------------------------------
    let pointer = ui.input(|i| i.pointer.clone());
    let mods = ui.input(|i| i.modifiers);
    let rows = visible_rows(ui, &geo, view, doc, style, highlight);
    if let Some(p) = pointer.interact_pos() {
        let in_gutter = rect.contains(p) && p.x < geo.text.min.x;
        let in_fold = in_gutter && style.settings.line_numbers && p.x >= geo.text.min.x - FOLD_W - 2.0;
        let in_scrollbar = rect.contains(p) && p.x >= rect.max.x - SCROLLBAR_W;
        let in_minimap = geo.minimap.is_some_and(|m| m.contains(p));
        let in_text = geo.text.contains(p);
        if pointer.primary_pressed() && rect.contains(p) && response.hovered() {
            ui.memory_mut(|m| m.request_focus(id));
            out.push(Action::Focus);
            view.blink_epoch = now;
            if in_scrollbar || in_minimap {
                view.drag =
                    Some(DragState { mode: DragMode::Char, base: Vec::new(), origin: Selection::point(usize::MAX), origin_display_col: in_minimap as usize });
                scrollbar_jump(ui, &geo, view, doc, style, highlight, p, in_minimap);
            } else if in_fold {
                if let Some(vr) = rows.iter().find(|r| p.y >= r.y && p.y < r.y + lh) {
                    out.push(Action::ToggleFold(vr.line));
                }
            } else if in_gutter {
                if let Some(vr) = rows.iter().find(|r| p.y >= r.y && p.y < r.y + lh) {
                    let line = vr.line;
                    let start = rope.line_to_char(line);
                    let end = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
                    let sel = Selection::new(start, end);
                    view.sels.set_single(sel);
                    view.drag = Some(DragState { mode: DragMode::Line, base: Vec::new(), origin: sel, origin_display_col: 0 });
                }
            } else if in_text {
                let pos = pos_at(ui, &geo, view, doc, style, highlight, &rows, p);
                let triple = pointer.button_triple_clicked(egui::PointerButton::Primary);
                let double = pointer.button_double_clicked(egui::PointerButton::Primary);
                let add = mods.command || mods.ctrl;
                let column = mods.alt && !mods.command && !mods.ctrl;
                let base: Vec<Selection> = if add { view.sels.iter().copied().collect() } else { Vec::new() };
                let (mode, origin) = if triple {
                    let line = rope.char_to_line(pos);
                    let start = rope.line_to_char(line);
                    let end = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
                    (DragMode::Line, Selection::new(start, end))
                } else if double {
                    let (a, b) = motion::word_at(rope.slice(..), pos);
                    (DragMode::Word, Selection::new(a, b))
                } else if column {
                    (DragMode::Column, Selection::point(pos))
                } else if mods.shift {
                    let anchor = view.sels.primary().anchor;
                    (DragMode::Char, Selection::new(anchor, pos))
                } else {
                    (DragMode::Char, Selection::point(pos))
                };
                let line = rope.char_to_line(pos);
                let origin_display_col = char_col_to_display_col(rope.line(line), pos - rope.line_to_char(line), tab_width);
                if add && !double && !triple && !column {
                    view.push_selection_history();
                    if let Some(i) = view.sels.find_containing(pos).filter(|_| view.sels.len() > 1) {
                        let mut v: Vec<Selection> = view.sels.iter().copied().collect();
                        v.remove(i);
                        view.sels = SelectionSet::from_vec(v, 0);
                        view.drag = None;
                    } else {
                        view.sels.push_primary(origin);
                        view.drag = Some(DragState { mode, base, origin, origin_display_col });
                    }
                } else {
                    if add {
                        view.push_selection_history();
                        view.sels.push_primary(origin);
                    } else {
                        view.sels.set_single(origin);
                    }
                    view.drag = Some(DragState { mode, base, origin, origin_display_col });
                }
                view.want_cursor_visible = false;
            }
        }
        if pointer.primary_down() {
            if let Some(drag) = view.drag.clone() {
                if drag.origin.anchor == usize::MAX {
                    scrollbar_jump(ui, &geo, view, doc, style, highlight, p, drag.origin_display_col == 1);
                } else {
                    // Auto-scroll when dragging outside the text area.
                    if p.y < geo.text.min.y {
                        scroll_rows(ui, &geo, view, doc, style, highlight, -1);
                    } else if p.y > geo.text.max.y {
                        scroll_rows(ui, &geo, view, doc, style, highlight, 1);
                    }
                    let rows = visible_rows(ui, &geo, view, doc, style, highlight);
                    let pos = pos_at(ui, &geo, view, doc, style, highlight, &rows, pos2(p.x, p.y.clamp(geo.text.min.y, geo.text.max.y - 1.0)));
                    match drag.mode {
                        DragMode::Column => {
                            let l0 = rope.char_to_line(drag.origin.anchor);
                            let l1 = rope.char_to_line(pos);
                            let c1 = char_col_to_display_col(rope.line(l1), pos - rope.line_to_char(l1), tab_width);
                            let c0 = drag.origin_display_col;
                            let (top, bottom) = (l0.min(l1), l0.max(l1));
                            let mut sels: Vec<Selection> = drag.base.clone();
                            for line in top..=bottom {
                                if view.is_line_hidden(doc, line) {
                                    continue;
                                }
                                let ls = rope.line_to_char(line);
                                let a = ls + display_col_to_char_col(rope.line(line), c0, tab_width);
                                let h = ls + display_col_to_char_col(rope.line(line), c1, tab_width);
                                if a == h && c0 != c1 && line_len_chars(rope, line) < c0.min(c1) {
                                    continue;
                                }
                                sels.push(Selection::new(a, h));
                            }
                            let primary = sels.len().saturating_sub(1);
                            view.sels = SelectionSet::from_vec(sels, primary);
                        }
                        _ => {
                            let sel = match drag.mode {
                                DragMode::Word => {
                                    let (a, b) = motion::word_at(rope.slice(..), pos);
                                    if pos < drag.origin.start() {
                                        Selection::new(drag.origin.end(), a.min(pos))
                                    } else {
                                        Selection::new(drag.origin.start(), b.max(pos))
                                    }
                                }
                                DragMode::Line => {
                                    let line = rope.char_to_line(pos);
                                    let ls = rope.line_to_char(line);
                                    let le = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
                                    if ls < drag.origin.start() { Selection::new(drag.origin.end(), ls) } else { Selection::new(drag.origin.start(), le) }
                                }
                                _ => Selection::new(drag.origin.anchor, pos),
                            };
                            let mut all = drag.base.clone();
                            all.push(sel);
                            let primary = all.len() - 1;
                            view.sels = SelectionSet::from_vec(all, primary);
                        }
                    }
                    view.want_cursor_visible = false;
                    view.blink_epoch = now;
                }
            }
        } else {
            view.drag = None;
        }
        if pointer.secondary_clicked() && rect.contains(p) && response.hovered() {
            ui.memory_mut(|m| m.request_focus(id));
            out.push(Action::Focus);
            if in_text {
                let pos = pos_at(ui, &geo, view, doc, style, highlight, &rows, p);
                if view.sels.find_containing(pos).is_none() {
                    view.sels.set_single(Selection::point(pos));
                }
            }
            out.push(Action::ContextMenu(p));
        }
        if pointer.button_clicked(egui::PointerButton::Middle) && in_text {
            let pos = pos_at(ui, &geo, view, doc, style, highlight, &rows, p);
            view.sels.set_single(Selection::point(pos));
            out.push(Action::Command(Command::Paste, Some("primary".into())));
        }
        if in_text || in_gutter {
            ui.ctx().set_cursor_icon(if in_text { egui::CursorIcon::Text } else { egui::CursorIcon::Default });
        }
        // Hover position for the application (tooltips, LSP hover).
        if in_text && response.hovered() && !pointer.any_down() && pointer.is_moving() {
            let pos = pos_at(ui, &geo, view, doc, style, highlight, &rows, p);
            out.push(Action::Hover(Some(pos)));
        } else if !in_text {
            out.push(Action::Hover(None));
        }
    }

    // --- Keyboard -------------------------------------------------------
    if !has_focus || !style.focused {
        return;
    }
    let events = ui.input(|i| i.events.clone());
    let mut swallow_text: Option<Key> = None;
    let visual_motion = geo.wrap_width.is_some() || !view.fold_anchors.is_empty();
    for ev in events {
        match ev {
            Event::Text(t) => {
                if let Some(k) = swallow_text.take() {
                    let kname = k.symbol_or_name();
                    if kname.eq_ignore_ascii_case(&t) {
                        continue;
                    }
                }
                if !t.is_empty() && !t.chars().any(|c| c.is_control()) {
                    out.push(Action::Insert(t));
                    view.blink_epoch = now;
                }
            }
            Event::Paste(t) => out.push(Action::Paste(t)),
            Event::Copy => out.push(Action::Command(Command::Copy, None)),
            Event::Cut => out.push(Action::Command(Command::Cut, None)),
            Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                match keymap.dispatch_event(key, physical_key, modifiers, &[Context::Editor, Context::Global]) {
                    Dispatch::Command(c, arg) => {
                        swallow_text = Some(key);
                        view.blink_epoch = now;
                        // Visual-row motion needs layout, so handle it here.
                        let handled = visual_motion
                            && match c {
                                Command::MoveUp => {
                                    move_visual(ui, &geo, view, doc, style, highlight, -1, false);
                                    true
                                }
                                Command::MoveDown => {
                                    move_visual(ui, &geo, view, doc, style, highlight, 1, false);
                                    true
                                }
                                Command::SelectUp => {
                                    move_visual(ui, &geo, view, doc, style, highlight, -1, true);
                                    true
                                }
                                Command::SelectDown => {
                                    move_visual(ui, &geo, view, doc, style, highlight, 1, true);
                                    true
                                }
                                Command::MovePageUp | Command::SelectPageUp => {
                                    move_visual(ui, &geo, view, doc, style, highlight, -(geo.rows_fit() as i64), c == Command::SelectPageUp);
                                    true
                                }
                                Command::MovePageDown | Command::SelectPageDown => {
                                    move_visual(ui, &geo, view, doc, style, highlight, geo.rows_fit() as i64, c == Command::SelectPageDown);
                                    true
                                }
                                _ => false,
                            };
                        if !handled {
                            out.push(Action::Command(c, arg));
                        }
                    }
                    Dispatch::Pending => swallow_text = Some(key),
                    Dispatch::Unbound => {}
                }
            }
            Event::Ime(ime) => match ime {
                egui::ImeEvent::Preedit { text, active_range_chars } => {
                    view.ime_preedit = if text.is_empty() { None } else { Some((text, active_range_chars.map(|r| r.start).unwrap_or(0))) };
                }
                egui::ImeEvent::Commit(t) => {
                    view.ime_preedit = None;
                    if !t.is_empty() {
                        out.push(Action::ImeCommit(t));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Jump the viewport from a scrollbar or minimap position.
#[allow(clippy::too_many_arguments)]
fn scrollbar_jump(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>, p: Pos2, minimap: bool) {
    let total = geo.total_lines.max(1);
    let target_line = if minimap {
        let (start, _) = minimap_window(geo, view, total);
        let line = start + ((p.y - geo.rect.min.y) / MINIMAP_LINE_H) as usize;
        line.saturating_sub(geo.rows_fit() / 2)
    } else {
        let frac = ((p.y - geo.text.min.y) / geo.text.height()).clamp(0.0, 1.0);
        ((total as f32) * frac) as usize
    };
    view.top_line = target_line.min(total - 1);
    view.top_row = 0;
    if view.is_line_hidden(doc, view.top_line) {
        view.top_line = view.prev_visible_line(doc, view.top_line).unwrap_or(0);
    }
    if !style.settings.scroll_past_end {
        clamp_top_to_content(ui, geo, view, doc, style, highlight);
    }
}

/// First line shown in the minimap and how many fit.
fn minimap_window(geo: &Geometry, view: &View, total: usize) -> (usize, usize) {
    let fit = (geo.rect.height() / MINIMAP_LINE_H) as usize;
    if total <= fit {
        return (0, fit);
    }
    let frac = view.top_line as f32 / (total.saturating_sub(geo.rows_fit())).max(1) as f32;
    let start = ((total - fit) as f32 * frac.clamp(0.0, 1.0)) as usize;
    (start, fit)
}

/// Paint the view. Returns the geometry used (for the status bar etc.).
pub fn paint(ui: &mut Ui, rect: Rect, view: &mut View, doc: &Document, style: &EditorStyle, highlight: Option<&HighlightFn>) -> Geometry {
    let geo = geometry(rect, doc, style);
    let theme = style.theme;
    let ed = &theme.editor;
    let rope = doc.rope();
    let painter = ui.painter_at(rect);
    let lh = geo.line_height;
    let tab_width = style.settings.tab_width;
    view.lines_per_page = geo.rows_fit();
    view.last_rect = rect;
    scroll_to_cursor(ui, rect, view, doc, style, highlight);
    if geo.wrap_width.is_some() {
        view.scroll.x = 0.0;
    }

    painter.rect_filled(rect, 0.0, ed.background);
    let rows = visible_rows(ui, &geo, view, doc, style, highlight);
    let text_painter = ui.painter_at(geo.text);
    let x0 = geo.text.min.x + TEXT_PAD - view.scroll.x;
    let is_focused = style.focused && ui.memory(|m| m.has_focus(view.id.egui_id()));
    let folds = view.folds(doc);
    let sels: Vec<Selection> = view.sels.iter().copied().collect();

    // Line-level data per visible row group.
    let row_y = |line: usize, row: usize| -> Option<f32> { rows.iter().find(|r| r.line == line && r.row == row).map(|r| r.y) };

    // Current line highlight.
    if style.settings.highlight_current_line && !style.settings.low_distraction {
        for s in sels.iter().filter(|s| s.is_empty()) {
            let line = rope.char_to_line(s.head.min(rope.len_chars()));
            let rows_of_line: Vec<&VisibleRow> = rows.iter().filter(|r| r.line == line).collect();
            if let (Some(first), Some(last)) = (rows_of_line.first(), rows_of_line.last()) {
                painter.rect_filled(Rect::from_min_max(pos2(geo.text.min.x, first.y), pos2(geo.text.max.x, last.y + lh)), 0.0, ed.current_line);
            }
        }
    }

    // Rulers.
    if geo.wrap_width.is_none() {
        for &r in &style.settings.rulers {
            let x = x0 + r as f32 * style.char_width;
            if x > geo.text.min.x && x < geo.text.max.x {
                text_painter.vline(x, geo.text.y_range(), Stroke::new(1.0, ed.ruler));
            }
        }
    }

    // Search matches.
    for (i, m) in style.matches.iter().enumerate() {
        let colour = if style.current_match == Some(i) { ed.search_match_current } else { ed.search_match };
        paint_range(ui, &geo, view, doc, style, highlight, &text_painter, &rows, m.start, m.end, colour, x0, false);
    }

    // Selections.
    let sel_colour = if is_focused { ed.selection } else { ed.selection_inactive };
    for s in &sels {
        if !s.is_empty() {
            paint_range(ui, &geo, view, doc, style, highlight, &text_painter, &rows, s.start(), s.end(), sel_colour, x0, true);
        }
    }

    // Bracket match.
    if style.settings.highlight_matching_brackets
        && sels.len() == 1
        && sels[0].is_empty()
        && let Some((o, c)) = apex_edit::brackets::matching_bracket(rope, sels[0].head)
    {
        for p in [o, c] {
            let (line, row, xa) = locate(ui, &geo, view, doc, style, highlight, p);
            if let Some(y) = row_y(line, row) {
                let xb = x0 + layout_line(ui, &geo, view, doc, line, style, highlight).x_of(p - rope.line_to_char(line) + 1);
                text_painter.rect_stroke(
                    Rect::from_min_max(pos2(x0 + xa, y + 1.0), pos2(xb.max(x0 + xa + style.char_width), y + lh - 1.0)),
                    2.0,
                    Stroke::new(1.0, ed.bracket_match),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    // Indent guides.
    if style.settings.indent_guides && !style.settings.low_distraction && geo.wrap_width.is_none() {
        let unit = if style.settings.use_tabs { tab_width } else { style.settings.indent_width.max(1) };
        let mut prev_indent = 0usize;
        let mut done = std::collections::HashSet::new();
        for vr in &rows {
            if !done.insert(vr.line) {
                continue;
            }
            let l = rope.line(vr.line);
            let n = motion::first_non_blank(l);
            let blank = n >= line_len_chars(rope, vr.line);
            let cols = if blank { prev_indent } else { char_col_to_display_col(l, n, tab_width) };
            if !blank {
                prev_indent = cols;
            }
            let y = vr.y;
            let mut c = unit;
            while c <= cols.saturating_sub(1) {
                let x = x0 + c as f32 * style.char_width;
                text_painter.vline(x, y..=y + lh, Stroke::new(1.0, ed.indent_guide));
                c += unit;
            }
        }
    }

    // Text.
    let mut max_width: f32 = 0.0;
    let mut painted = std::collections::HashSet::new();
    for vr in &rows {
        if !painted.insert(vr.line) {
            continue;
        }
        let top = vr.y - vr.row as f32 * lh;
        let layout = layout_line(ui, &geo, view, doc, vr.line, style, highlight);
        max_width = max_width.max(layout.width());
        let galley = layout.galley.clone();
        text_painter.galley(pos2(x0, top), galley, ed.foreground);
        if line_len_chars(rope, vr.line) > MAX_LAYOUT_CHARS {
            let x = x0 + layout.width() + 4.0;
            text_painter.text(pos2(x, top + lh / 2.0), egui::Align2::LEFT_CENTER, "... (line truncated for display)", style.font_id.clone(), ed.line_number);
        }
        // Fold placeholder after the first line of a folded region.
        if let Some((_, end)) = folds.iter().find(|(a, _)| *a == vr.line) {
            let x = x0 + layout.width() + style.char_width;
            let hidden = end - vr.line - 1;
            let label = format!("... {hidden} lines");
            let g =
                ui.ctx().fonts_mut(|f| f.layout_no_wrap(label, egui::FontId::new(style.font_id.size * 0.85, egui::FontFamily::Proportional), ed.fold_marker));
            let r = Rect::from_min_size(pos2(x, top + 2.0), vec2(g.rect.width() + 8.0, lh - 4.0));
            text_painter.rect_stroke(r, 3.0, Stroke::new(1.0, ed.fold_marker), egui::StrokeKind::Inside);
            text_painter.galley(pos2(x + 4.0, top + (lh - g.rect.height()) / 2.0), g, ed.fold_marker);
        }
    }

    // Diagnostics underlines.
    for &(a, b, sev) in style.diagnostics {
        let colour = match sev {
            1 => ed.diagnostic_error,
            2 => ed.diagnostic_warning,
            _ => ed.diagnostic_info,
        };
        let (line, row, xa) = locate(ui, &geo, view, doc, style, highlight, a);
        let (lb, rb, xb) = locate(ui, &geo, view, doc, style, highlight, b.max(a + 1));
        if let Some(y) = row_y(line, row) {
            let xb = if (lb, rb) == (line, row) { xb } else { xa + style.char_width * 3.0 };
            let y = y + lh - 2.0;
            let mut x = x0 + xa;
            let end = x0 + xb;
            // Wavy underline: also visible without colour vision.
            let mut up = true;
            while x < end {
                let nx = (x + 3.0).min(end);
                text_painter.line_segment([pos2(x, if up { y } else { y + 2.0 }), pos2(nx, if up { y + 2.0 } else { y })], Stroke::new(1.0, colour));
                x = nx;
                up = !up;
            }
        }
    }

    // Whitespace.
    if style.settings.show_whitespace != WhitespaceMode::None {
        let only_selection = style.settings.show_whitespace == WhitespaceMode::Selection;
        let mut done = std::collections::HashSet::new();
        for vr in &rows {
            if !done.insert(vr.line) {
                continue;
            }
            let line = vr.line;
            let ls = rope.line_to_char(line);
            let len = line_len_chars(rope, line);
            if len == 0 {
                continue;
            }
            let xs: Vec<(usize, char)> = rope.line(line).chars().take(len.min(MAX_LAYOUT_CHARS)).enumerate().filter(|(_, c)| *c == ' ' || *c == '\t').collect();
            for (col, c) in xs {
                let pos = ls + col;
                if only_selection && !sels.iter().any(|s| s.contains(pos)) {
                    continue;
                }
                let (_, row, xa) = locate(ui, &geo, view, doc, style, highlight, pos);
                let Some(y) = row_y(line, row) else { continue };
                let xb = x0 + layout_line(ui, &geo, view, doc, line, style, highlight).x_of(col + 1);
                let xa = x0 + xa;
                let cy = y + lh / 2.0;
                if c == ' ' {
                    text_painter.circle_filled(pos2((xa + xb) / 2.0, cy), 1.2, ed.whitespace);
                } else {
                    text_painter.line_segment([pos2(xa + 2.0, cy), pos2(xb - 2.0, cy)], Stroke::new(1.0, ed.whitespace));
                    text_painter.line_segment([pos2(xb - 5.0, cy - 3.0), pos2(xb - 2.0, cy)], Stroke::new(1.0, ed.whitespace));
                    text_painter.line_segment([pos2(xb - 5.0, cy + 3.0), pos2(xb - 2.0, cy)], Stroke::new(1.0, ed.whitespace));
                }
            }
        }
    }

    // IME preedit.
    if let Some((pre, _)) = view.ime_preedit.clone() {
        let head = view.sels.primary().head.min(rope.len_chars());
        let (line, row, xa) = locate(ui, &geo, view, doc, style, highlight, head);
        if let Some(y) = row_y(line, row) {
            let x = x0 + xa;
            let g = ui.ctx().fonts_mut(|f| f.layout_no_wrap(pre.clone(), style.font_id.clone(), ed.foreground));
            text_painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(g.rect.width(), lh)), 0.0, ed.current_line);
            text_painter.galley(pos2(x, y + (lh - g.rect.height()) / 2.0), g.clone(), ed.foreground);
            text_painter.hline(x..=x + g.rect.width(), y + lh - 1.0, Stroke::new(1.0, ed.foreground));
        }
    }

    // Carets.
    let blink_on = !style.settings.caret_blink || !is_focused || ((ui.input(|i| i.time) - view.blink_epoch) % 1.0) < 0.55;
    if is_focused && style.settings.caret_blink {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(if blink_on { 550 } else { 450 }));
    }
    let primary = view.sels.primary();
    let mut ime_rect = None;
    if blink_on || !is_focused {
        for s in &sels {
            let head = s.head.min(rope.len_chars());
            let overwrite = view.overwrite;
            let (line, row, xa) = locate(ui, &geo, view, doc, style, highlight, head);
            let Some(y) = row_y(line, row) else { continue };
            let x = x0 + xa;
            let col = head - rope.line_to_char(line);
            let w = if overwrite {
                let l = layout_line(ui, &geo, view, doc, line, style, highlight);
                (l.x_of(col + 1) - l.x_of(col)).max(style.char_width)
            } else {
                style.settings.caret_width
            };
            let colour = if *s == primary { ed.caret } else { ed.caret_secondary };
            let r = Rect::from_min_size(pos2(x, y + 1.0), vec2(w, lh - 2.0));
            if is_focused {
                text_painter.rect_filled(r, 0.0, colour);
            } else {
                text_painter.rect_stroke(r, 0.0, Stroke::new(1.0, colour), egui::StrokeKind::Inside);
            }
            if *s == primary {
                ime_rect = Some(r);
            }
        }
    }
    if is_focused {
        let cursor_rect = ime_rect.unwrap_or(Rect::from_min_size(geo.text.min, vec2(1.0, lh)));
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: geo.text, cursor_rect, should_interrupt_composition: false });
        });
    }

    // Gutter (painted over text so horizontal scroll disappears beneath it).
    let gutter = Rect::from_min_max(rect.min, pos2(geo.text.min.x, rect.max.y));
    painter.rect_filled(gutter, 0.0, ed.gutter_background);
    let current_lines: Vec<usize> = sels.iter().map(|s| rope.char_to_line(s.head.min(rope.len_chars()))).collect();
    let primary_line = rope.char_to_line(primary.head.min(rope.len_chars()));
    let bookmarks = doc.bookmarked_lines();
    let hovered_gutter = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| gutter.contains(p));
    let mut done = std::collections::HashSet::new();
    for vr in &rows {
        if vr.row != 0 || !done.insert(vr.line) {
            continue;
        }
        let line = vr.line;
        let y = vr.y;
        if style.settings.line_numbers {
            let n = if style.settings.relative_line_numbers && line != primary_line { line.abs_diff(primary_line) } else { line + 1 };
            let colour = if current_lines.contains(&line) { ed.line_number_active } else { ed.line_number };
            painter.text(pos2(geo.text.min.x - GUTTER_PAD - FOLD_W, y + lh / 2.0), egui::Align2::RIGHT_CENTER, n.to_string(), style.font_id.clone(), colour);
            // Fold control.
            let folded = folds.iter().any(|(a, _)| *a == line);
            let foldable = folded || style.fold_regions.iter().any(|(a, _)| *a == line);
            if foldable && (folded || hovered_gutter) {
                let c = pos2(geo.text.min.x - FOLD_W / 2.0 - 2.0, y + lh / 2.0);
                let pts = if folded {
                    vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
                } else {
                    vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
                };
                painter.add(egui::Shape::convex_polygon(pts, if folded { ed.fold_marker } else { ed.line_number }, Stroke::NONE));
            }
        }
        if bookmarks.binary_search(&line).is_ok() {
            painter.circle_filled(pos2(rect.min.x + 6.0, y + lh / 2.0), 3.0, theme.ui.accent);
        }
        if let Some((_, kind)) = style.git_marks.iter().find(|(l, _)| *l == line) {
            let colour = match kind {
                0 => ed.git_added,
                1 => ed.git_modified,
                _ => ed.git_deleted,
            };
            let x = geo.text.min.x - 4.0;
            if *kind == 2 {
                painter.rect_filled(Rect::from_min_size(pos2(x - 2.0, y + lh - 2.0), vec2(6.0, 3.0)), 0.0, colour);
            } else {
                let rows_of_line = rows.iter().filter(|r| r.line == line).count() as f32;
                painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, lh * rows_of_line)), 0.0, colour);
            }
        }
    }

    // Minimap.
    if let Some(mm) = geo.minimap {
        paint_minimap(ui, &geo, mm, view, doc, style, highlight, &rows, &sels);
    }

    // Vertical scrollbar.
    let total = geo.total_lines.max(1);
    if style.settings.scrollbars && total > geo.rows_fit() {
        let track = Rect::from_min_max(pos2(rect.max.x - SCROLLBAR_W, rect.min.y), rect.max);
        let frac_visible = (geo.rows_fit() as f32 / total as f32).clamp(0.02, 1.0);
        let thumb_h = (frac_visible * track.height()).max(20.0);
        let frac_pos = (view.top_line as f32 / (total.saturating_sub(geo.rows_fit())).max(1) as f32).clamp(0.0, 1.0);
        let thumb_y = track.min.y + frac_pos * (track.height() - thumb_h);
        let hovered = ui.rect_contains_pointer(track) || view.drag.as_ref().is_some_and(|d| d.origin.anchor == usize::MAX);
        let colour = if hovered { theme.ui.scrollbar_hover } else { theme.ui.scrollbar };
        painter.rect_filled(Rect::from_min_size(pos2(track.min.x + 2.0, thumb_y), vec2(SCROLLBAR_W - 4.0, thumb_h)), 3.0, colour);
    }
    // Horizontal scrollbar (thin, only when content overflows).
    let content_w = max_width + TEXT_PAD * 2.0;
    if style.settings.scrollbars && geo.wrap_width.is_none() && (content_w > geo.text.width() || view.scroll.x > 0.0) {
        let track_w = geo.text.width();
        let total_w = content_w.max(view.scroll.x + geo.text.width());
        let frac_visible = (geo.text.width() / total_w).clamp(0.05, 1.0);
        let thumb_w = (frac_visible * track_w).max(20.0);
        let frac_pos = (view.scroll.x / (total_w - geo.text.width()).max(1.0)).clamp(0.0, 1.0);
        let x = geo.text.min.x + frac_pos * (track_w - thumb_w);
        painter.rect_filled(Rect::from_min_size(pos2(x, rect.max.y - 6.0), vec2(thumb_w, 4.0)), 2.0, theme.ui.scrollbar);
    }
    geo
}

/// Fill the on-screen area of a char range, row by row.
#[allow(clippy::too_many_arguments)]
fn paint_range(
    ui: &Ui,
    geo: &Geometry,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
    painter: &egui::Painter,
    rows: &[VisibleRow],
    start: usize,
    end: usize,
    colour: Color32,
    x0: f32,
    include_newline: bool,
) {
    let rope = doc.rope();
    let n = rope.len_chars();
    let (start, end) = (start.min(n), end.min(n));
    if start >= end || rows.is_empty() {
        return;
    }
    let first_line = rows[0].line;
    let last_line = rows[rows.len() - 1].line;
    let l0 = rope.char_to_line(start).max(first_line);
    let l1 = rope.char_to_line(end).min(last_line);
    if l0 > l1 {
        return;
    }
    let lh = geo.line_height;
    let mut line = l0;
    while line <= l1 {
        if rows.iter().any(|r| r.line == line) {
            let ls = rope.line_to_char(line);
            let le = line_end_char(rope, line);
            let a = start.max(ls);
            let b = end.min(le);
            let layout = layout_line(ui, geo, view, doc, line, style, highlight);
            let galley = layout.galley.clone();
            // Row char boundaries within the galley.
            let mut row_start = 0usize;
            for (k, prow) in galley.rows.iter().enumerate() {
                let count = prow.char_count_including_newline().0;
                let row_end = row_start + count;
                let Some(y) = rows.iter().find(|r| r.line == line && r.row == k).map(|r| r.y) else {
                    row_start = row_end;
                    continue;
                };
                let ga = layout.to_galley_col(a - ls).max(row_start).min(row_end);
                let gb = layout.to_galley_col(b - ls).min(row_end).max(row_start);
                if ga < gb || (ga == gb && include_newline && end > le && k + 1 == galley.rows.len()) {
                    let xa = x0 + galley.pos_from_cursor(CCursor { index: CharIndex(ga), prefer_next_row: true }).min.x;
                    let mut xb = x0 + galley.pos_from_cursor(CCursor { index: CharIndex(gb), prefer_next_row: false }).min.x;
                    if include_newline && end > le && line + 1 < rope.len_lines() && k + 1 == galley.rows.len() {
                        xb += style.char_width * 0.5;
                    }
                    if xb > xa {
                        painter.rect_filled(Rect::from_min_max(pos2(xa, y), pos2(xb, y + lh)), 0.0, colour);
                    }
                }
                row_start = row_end;
            }
        }
        line += 1;
    }
}

/// Draw the minimap: one 2 px row per line, 1 px per character, token colours.
#[allow(clippy::too_many_arguments)]
fn paint_minimap(
    ui: &Ui,
    geo: &Geometry,
    mm: Rect,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
    rows: &[VisibleRow],
    sels: &[Selection],
) {
    let rope = doc.rope();
    let theme = style.theme;
    let painter = ui.painter_at(mm);
    painter.rect_filled(mm, 0.0, theme.editor.gutter_background);
    let total = geo.total_lines.max(1);
    let (start, fit) = minimap_window(geo, view, total);
    let end = (start + fit).min(total);
    let max_cols = ((mm.width() - 6.0) / MINIMAP_CHAR_W) as usize;
    let epoch_key = (view.cache.epoch, start, end, theme.name.len(), style.settings.muted_syntax);
    if view.minimap_cache.as_ref().map(|c| c.0) != Some(epoch_key)
        || view.minimap_cache.as_ref().is_some_and(|c| c.2.elapsed() > std::time::Duration::from_millis(150) && c.3 != view.cache.epoch)
    {
        let mut shapes: Vec<egui::Shape> = Vec::with_capacity((end - start) * 4);
        let fg = theme.editor.foreground.linear_multiply(0.55);
        for line in start..end {
            if view.is_line_hidden(doc, line) {
                continue;
            }
            let y = (line - start) as f32 * MINIMAP_LINE_H;
            let text: String = rope.line(line).chars().take(max_cols).collect();
            let spans = highlight.map(|h| h(line)).unwrap_or_default();
            let mut byte = 0usize;
            let mut col = 0usize;
            let mut run_start: Option<(usize, Color32)> = None;
            let mut flush = |run_start: &mut Option<(usize, Color32)>, col: usize| {
                if let Some((s, c)) = run_start.take()
                    && col > s
                {
                    shapes.push(egui::Shape::rect_filled(
                        Rect::from_min_size(pos2(3.0 + s as f32 * MINIMAP_CHAR_W, y), vec2((col - s) as f32 * MINIMAP_CHAR_W, MINIMAP_LINE_H - 0.5)),
                        0.0,
                        c,
                    ));
                }
            };
            for ch in text.chars() {
                let w = if ch == '\t' { style.settings.tab_width } else { 1 };
                if ch.is_whitespace() || ch == '\n' || ch == '\r' {
                    flush(&mut run_start, col);
                } else {
                    let colour = spans.iter().find(|(a, b, _)| byte >= *a && byte < *b).map(|(_, _, st)| st.fg.linear_multiply(0.7)).unwrap_or(fg);
                    match run_start {
                        Some((_, c)) if c == colour => {}
                        _ => {
                            flush(&mut run_start, col);
                            run_start = Some((col, colour));
                        }
                    }
                }
                col += w;
                byte += ch.len_utf8();
                if col >= max_cols {
                    break;
                }
            }
            flush(&mut run_start, col);
        }
        view.minimap_cache = Some((epoch_key, shapes, std::time::Instant::now(), view.cache.epoch));
    }
    if let Some((_, shapes, _, _)) = &view.minimap_cache {
        let offset = mm.min.to_vec2();
        for s in shapes {
            let mut s = s.clone();
            s.translate(offset);
            painter.add(s);
        }
    }
    // Viewport indicator.
    if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        let y0 = mm.min.y + (first.line.saturating_sub(start)) as f32 * MINIMAP_LINE_H;
        let y1 = mm.min.y + (last.line.saturating_sub(start) + 1) as f32 * MINIMAP_LINE_H;
        painter.rect_filled(Rect::from_min_max(pos2(mm.min.x, y0), pos2(mm.max.x, y1.max(y0 + 4.0))), 0.0, theme.editor.minimap_viewport);
    }
    // Selection, search and git markers at the edges.
    for s in sels {
        let line = rope.char_to_line(s.start().min(rope.len_chars()));
        if line >= start && line < end {
            let y = mm.min.y + (line - start) as f32 * MINIMAP_LINE_H;
            painter.rect_filled(Rect::from_min_size(pos2(mm.min.x, y), vec2(2.0, MINIMAP_LINE_H)), 0.0, theme.editor.caret);
        }
    }
    for m in style.matches {
        let line = rope.char_to_line(m.start.min(rope.len_chars()));
        if line >= start && line < end {
            let y = mm.min.y + (line - start) as f32 * MINIMAP_LINE_H;
            painter.rect_filled(Rect::from_min_size(pos2(mm.max.x - 3.0, y), vec2(3.0, MINIMAP_LINE_H)), 0.0, theme.editor.search_match_current);
        }
    }
    for &(line, kind) in style.git_marks {
        if line >= start && line < end {
            let y = mm.min.y + (line - start) as f32 * MINIMAP_LINE_H;
            let c = match kind {
                0 => theme.editor.git_added,
                1 => theme.editor.git_modified,
                _ => theme.editor.git_deleted,
            };
            painter.rect_filled(Rect::from_min_size(pos2(mm.min.x, y), vec2(2.0, MINIMAP_LINE_H)), 0.0, c);
        }
    }
}

/// Convenience used by the application to compute the font for a view.
pub fn font_for(settings: &Settings, zoom_steps: i32) -> egui::FontId {
    let size = (settings.font_size + zoom_steps as f32).clamp(6.0, 72.0);
    egui::FontId::new(size, egui::FontFamily::Monospace)
}

/// Measure line height and character width for a font.
pub fn metrics(ctx: &egui::Context, font: &egui::FontId, line_height_factor: f32) -> (f32, f32) {
    ctx.fonts_mut(|f| {
        let row = f.row_height(font);
        let cw = f.glyph_width(font, '0');
        ((row * line_height_factor).round(), cw)
    })
}

pub fn selection_summary(doc: &Document, sels: &SelectionSet) -> String {
    let n = sels.len();
    let chars: usize = sels.iter().map(|s| s.len()).sum();
    let lines: usize =
        sels.iter().filter(|s| !s.is_empty()).map(|s| doc.rope().char_to_line(s.end().min(doc.len_chars())) - doc.rope().char_to_line(s.start()) + 1).sum();
    match (n, chars) {
        (1, 0) => String::new(),
        (1, c) if lines <= 1 => format!("{c} characters selected"),
        (1, c) => format!("{lines} lines, {c} characters selected"),
        (n, 0) => format!("{n} selection regions"),
        (n, c) => format!("{n} selection regions, {c} characters"),
    }
}

pub fn ccursor(i: usize) -> CCursor {
    CCursor { index: CharIndex(i), prefer_next_row: false }
}

pub fn arc_galley_text(g: &Arc<egui::Galley>) -> &str {
    g.text()
}
