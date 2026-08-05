//! The custom editor widget: viewport-only layout, multi-cursor rendering,
//! mouse and keyboard input.
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
use crate::keymap::{Context, Dispatch, KeyPress, Keymap};
use crate::settings::{Settings, WhitespaceMode};
use crate::theme::Theme;
use crate::view::{DragMode, DragState, LineLayout, View};

/// Lines longer than this are laid out only up to the limit; the rest is
/// reachable by keyboard but not drawn. Keeps a 1 MB line from stalling the UI.
pub const MAX_LAYOUT_CHARS: usize = 20_000;

const GUTTER_PAD: f32 = 10.0;
const TEXT_PAD: f32 = 6.0;
const SCROLLBAR_W: f32 = 10.0;

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
    pub line_height: f32,
    pub total_lines: usize,
    pub content_height: f32,
}

pub fn geometry(rect: Rect, doc: &Document, style: &EditorStyle) -> Geometry {
    let total_lines = doc.len_lines();
    let digits = total_lines.max(1).to_string().len().max(2);
    let gutter_w = if style.settings.line_numbers { digits as f32 * style.char_width + GUTTER_PAD * 2.0 + 8.0 } else { GUTTER_PAD + 8.0 };
    let text = Rect::from_min_max(pos2(rect.min.x + gutter_w, rect.min.y), pos2(rect.max.x - SCROLLBAR_W, rect.max.y));
    let past_end = if style.settings.scroll_past_end { (rect.height() - style.line_height).max(0.0) } else { 0.0 };
    Geometry { rect, gutter_w, text, line_height: style.line_height, total_lines, content_height: total_lines as f32 * style.line_height + past_end }
}

impl Geometry {
    pub fn first_visible_line(&self, scroll_y: f32) -> usize {
        (scroll_y / self.line_height).floor().max(0.0) as usize
    }

    pub fn visible_line_count(&self) -> usize {
        (self.text.height() / self.line_height).ceil() as usize + 1
    }

    pub fn line_top(&self, line: usize, scroll_y: f32) -> f32 {
        self.text.min.y + line as f32 * self.line_height - scroll_y
    }

    pub fn line_at_y(&self, y: f32, scroll_y: f32) -> usize {
        let l = ((y - self.text.min.y + scroll_y) / self.line_height).floor();
        (l.max(0.0) as usize).min(self.total_lines.saturating_sub(1))
    }

    pub fn max_scroll_y(&self) -> f32 {
        (self.content_height - self.text.height()).max(0.0)
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

/// Lay out one line, consulting the cache.
pub fn layout_line<'a>(ui: &Ui, view: &'a mut View, doc: &Document, line: usize, style: &EditorStyle, highlight: Option<&HighlightFn>) -> &'a LineLayout {
    if view.cache.get(line).is_none() {
        let rope = doc.rope();
        let len = line_len_chars(rope, line);
        let slice = rope.line(line);
        let (text, map) = expand_for_layout(slice, len, style.settings.tab_width);
        let mut job = LayoutJob { wrap: TextWrapping::no_max_width(), break_on_newline: false, round_output_to_gui: true, ..Default::default() };
        let base_colour = style.theme.editor.foreground;
        let font = style.font_id.clone();
        let push = |job: &mut LayoutJob, start: usize, end: usize, st: Option<crate::theme::SyntaxStyle>| {
            if start >= end {
                return;
            }
            let mut fmt = TextFormat { font_id: font.clone(), color: st.map(|s| s.fg).unwrap_or(base_colour), ..Default::default() };
            if let Some(s) = st {
                fmt.italics = s.italic;
                if s.underline {
                    fmt.underline = Stroke::new(1.0, s.fg);
                }
                if s.bold {
                    // egui has no synthetic bold; emulate with slight letter spacing.
                    fmt.extra_letter_spacing = 0.0;
                }
            }
            job.sections.push(LayoutSection { leading_space: 0.0, byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(end), format: fmt });
        };
        let spans = highlight.map(|h| h(line)).unwrap_or_default();
        if spans.is_empty() || map.is_some() || len > MAX_LAYOUT_CHARS {
            // With tabs the byte offsets shift; remap spans through the char map.
            if let (false, Some(m)) = (spans.is_empty(), map.as_ref()) {
                let line_str: String = slice.chars().take(len).collect();
                // byte→char index for the original line
                let mut byte_to_char = vec![0u32; line_str.len() + 1];
                let mut ci = 0u32;
                for (bi, _) in line_str.char_indices() {
                    byte_to_char[bi] = ci;
                    ci += 1;
                }
                byte_to_char[line_str.len()] = ci;
                // galley char index → galley byte index
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
                push(&mut job, 0, text.len(), None);
            }
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
        let epoch = view.cache.epoch;
        view.cache.insert(line, LineLayout { galley, map, epoch });
    }
    view.cache.get(line).expect("just inserted")
}

/// Document position under a screen point.
fn pos_at(ui: &Ui, geo: &Geometry, view: &mut View, doc: &Document, style: &EditorStyle, p: Pos2) -> usize {
    let rope = doc.rope();
    if geo.total_lines == 0 {
        return 0;
    }
    let line = geo.line_at_y(p.y, view.scroll.y);
    let len = line_len_chars(rope, line);
    let gx = p.x - geo.text.min.x - TEXT_PAD + view.scroll.x;
    let layout = layout_line(ui, view, doc, line, style, None);
    let gcol = layout.galley.cursor_from_pos(vec2(gx, 0.0)).index.0;
    let col = layout.to_doc_col(gcol).min(len);
    let pos = rope.line_to_char(line) + col;
    snap_to_grapheme(rope.slice(..), pos)
}

/// Process input for this frame. Mouse selection is applied to `view`
/// directly; everything that edits the document becomes an [`Action`].
pub fn input(ui: &mut Ui, rect: Rect, view: &mut View, doc: &Document, style: &EditorStyle, keymap: &mut Keymap, out: &mut Vec<Action>) {
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

    // --- Scrolling -----------------------------------------------------
    if response.hovered() {
        let (delta, zoom, mods) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
        if zoom != 1.0 {
            let steps = if zoom > 1.0 { 1 } else { -1 };
            out.push(Action::Command(if steps > 0 { Command::ZoomIn } else { Command::ZoomOut }, None));
        } else if delta != Vec2::ZERO {
            let d = if mods.shift && delta.x == 0.0 { vec2(delta.y, 0.0) } else { delta };
            view.scroll -= d;
        }
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
    if let Some(p) = pointer.interact_pos() {
        let in_gutter = rect.contains(p) && p.x < geo.text.min.x;
        let in_scrollbar = rect.contains(p) && p.x >= geo.text.max.x;
        let in_text = geo.text.contains(p);
        if pointer.primary_pressed() && rect.contains(p) && response.hovered() {
            ui.memory_mut(|m| m.request_focus(id));
            out.push(Action::Focus);
            view.blink_epoch = now;
            if in_scrollbar {
                view.drag = Some(DragState { mode: DragMode::Char, base: Vec::new(), origin: Selection::point(usize::MAX), origin_display_col: 0 });
                scrollbar_jump(&geo, view, p.y);
            } else if in_gutter {
                let line = geo.line_at_y(p.y, view.scroll.y);
                let start = rope.line_to_char(line);
                let end = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
                let sel = Selection::new(start, end);
                view.sels.set_single(sel);
                view.drag = Some(DragState { mode: DragMode::Line, base: Vec::new(), origin: sel, origin_display_col: 0 });
            } else if in_text {
                let pos = pos_at(ui, &geo, view, doc, style, p);
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
                    // Ctrl+click on an existing cursor removes it.
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
                    scrollbar_jump(&geo, view, p.y);
                } else {
                    // Auto-scroll when dragging outside the text area.
                    if p.y < geo.text.min.y {
                        view.scroll.y -= geo.line_height * 0.5;
                    } else if p.y > geo.text.max.y {
                        view.scroll.y += geo.line_height * 0.5;
                    }
                    let pos = pos_at(ui, &geo, view, doc, style, pos2(p.x, p.y.clamp(geo.text.min.y, geo.text.max.y - 1.0)));
                    let sel = match drag.mode {
                        DragMode::Char => Selection::new(drag.origin.anchor, pos),
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
                        DragMode::Column => {
                            let l0 = rope.char_to_line(drag.origin.anchor);
                            let l1 = rope.char_to_line(pos);
                            let c1 = char_col_to_display_col(rope.line(l1), pos - rope.line_to_char(l1), tab_width);
                            let c0 = drag.origin_display_col;
                            let (top, bottom) = (l0.min(l1), l0.max(l1));
                            let mut sels: Vec<Selection> = drag.base.clone();
                            for line in top..=bottom {
                                let ls = rope.line_to_char(line);
                                let a = ls + display_col_to_char_col(rope.line(line), c0, tab_width);
                                let h = ls + display_col_to_char_col(rope.line(line), c1, tab_width);
                                if a == h && c0 != c1 && line_len_chars(rope, line) < c0.min(c1) {
                                    continue; // short line outside the column block
                                }
                                sels.push(Selection::new(a, h));
                            }
                            let primary = sels.len().saturating_sub(1);
                            view.sels = SelectionSet::from_vec(sels, primary);
                            Selection::point(usize::MAX)
                        }
                    };
                    if sel.anchor != usize::MAX {
                        let mut all = drag.base.clone();
                        all.push(sel);
                        let primary = all.len() - 1;
                        view.sels = SelectionSet::from_vec(all, primary);
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
                let pos = pos_at(ui, &geo, view, doc, style, p);
                if view.sels.find_containing(pos).is_none() {
                    view.sels.set_single(Selection::point(pos));
                }
            }
            out.push(Action::ContextMenu(p));
        }
        if pointer.button_clicked(egui::PointerButton::Middle) && in_text {
            // Linux primary-selection paste is handled by the application.
            let pos = pos_at(ui, &geo, view, doc, style, p);
            view.sels.set_single(Selection::point(pos));
            out.push(Action::Command(Command::Paste, Some("primary".into())));
        }
        if in_text || in_gutter {
            ui.ctx().set_cursor_icon(if in_text { egui::CursorIcon::Text } else { egui::CursorIcon::Default });
        }
    }

    // --- Keyboard -------------------------------------------------------
    if !has_focus || !style.focused {
        return;
    }
    let events = ui.input(|i| i.events.clone());
    let mut swallow_text: Option<Key> = None;
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
            Event::Key { key, pressed: true, modifiers, .. } => {
                let press = KeyPress::from_egui(key, modifiers);
                match keymap.dispatch(press, &[Context::Editor, Context::Global]) {
                    Dispatch::Command(c, arg) => {
                        out.push(Action::Command(c, arg));
                        swallow_text = Some(key);
                        view.blink_epoch = now;
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

fn scrollbar_jump(geo: &Geometry, view: &mut View, y: f32) {
    let frac = ((y - geo.text.min.y) / geo.text.height()).clamp(0.0, 1.0);
    let visible = geo.text.height();
    view.scroll.y = (frac * geo.content_height - visible * frac).clamp(0.0, geo.max_scroll_y());
}

/// Adjust the scroll offset so the primary caret is visible (or centred).
/// Uses display columns for the x estimate so no text layout is needed.
pub fn scroll_to_cursor(rect: Rect, view: &mut View, doc: &Document, style: &EditorStyle) {
    if !(view.want_cursor_visible || view.want_centre) {
        return;
    }
    let geo = geometry(rect, doc, style);
    let rope = doc.rope();
    let lh = geo.line_height;
    let head = view.sels.primary().head.min(rope.len_chars());
    let line = rope.char_to_line(head);
    let top = line as f32 * lh;
    let margin = if view.want_centre { geo.text.height() / 2.0 - lh / 2.0 } else { lh };
    if view.want_centre {
        view.scroll.y = top - margin;
    } else if top - lh < view.scroll.y {
        view.scroll.y = (top - margin).max(0.0);
    } else if top + lh * 2.0 > view.scroll.y + geo.text.height() {
        view.scroll.y = top + lh * 2.0 - geo.text.height();
    }
    let col = head - rope.line_to_char(line);
    let x = char_col_to_display_col(rope.line(line), col, style.settings.tab_width) as f32 * style.char_width;
    if x < view.scroll.x + style.char_width {
        view.scroll.x = (x - style.char_width * 4.0).max(0.0);
    } else if x > view.scroll.x + geo.text.width() - TEXT_PAD * 2.0 - style.char_width {
        view.scroll.x = x - geo.text.width() + TEXT_PAD * 2.0 + style.char_width * 4.0;
    }
    view.scroll.y = view.scroll.y.clamp(0.0, geo.max_scroll_y());
    view.scroll.x = view.scroll.x.max(0.0);
    view.want_cursor_visible = false;
    view.want_centre = false;
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
    view.lines_per_page = ((geo.text.height() / lh).floor() as usize).max(1);
    view.last_rect = rect;

    scroll_to_cursor(rect, view, doc, style);
    view.scroll.y = view.scroll.y.clamp(0.0, geo.max_scroll_y());
    view.scroll.x = view.scroll.x.max(0.0);

    painter.rect_filled(rect, 0.0, ed.background);
    let first = geo.first_visible_line(view.scroll.y);
    let last = (first + geo.visible_line_count()).min(geo.total_lines);
    let text_painter = ui.painter_at(geo.text);
    let x0 = geo.text.min.x + TEXT_PAD - view.scroll.x;
    let is_focused = style.focused && ui.memory(|m| m.has_focus(view.id.egui_id()));

    // Current line highlight.
    if style.settings.highlight_current_line && !style.settings.low_distraction {
        for s in view.sels.iter().filter(|s| s.is_empty()) {
            let line = rope.char_to_line(s.head.min(rope.len_chars()));
            if line >= first && line < last {
                let y = geo.line_top(line, view.scroll.y);
                painter.rect_filled(Rect::from_min_size(pos2(geo.text.min.x, y), vec2(geo.text.width(), lh)), 0.0, ed.current_line);
            }
        }
    }

    // Rulers.
    for &r in &style.settings.rulers {
        let x = x0 + r as f32 * style.char_width;
        if x > geo.text.min.x && x < geo.text.max.x {
            text_painter.vline(x, geo.text.y_range(), Stroke::new(1.0, ed.ruler));
        }
    }

    // Search matches.
    for (i, m) in style.matches.iter().enumerate() {
        let colour = if style.current_match == Some(i) { ed.search_match_current } else { ed.search_match };
        paint_range(ui, &geo, view, doc, style, highlight, &text_painter, m.start, m.end, first, last, colour, x0, false);
    }

    // Selections.
    let sel_colour = if is_focused { ed.selection } else { ed.selection_inactive };
    let sels: Vec<Selection> = view.sels.iter().copied().collect();
    for s in &sels {
        if !s.is_empty() {
            paint_range(ui, &geo, view, doc, style, highlight, &text_painter, s.start(), s.end(), first, last, sel_colour, x0, true);
        }
    }

    // Bracket match.
    if style.settings.highlight_matching_brackets
        && sels.len() == 1
        && sels[0].is_empty()
        && let Some((o, c)) = apex_edit::brackets::matching_bracket(rope, sels[0].head)
    {
        for p in [o, c] {
            let line = rope.char_to_line(p);
            if line >= first && line < last {
                let col = p - rope.line_to_char(line);
                let layout = layout_line(ui, view, doc, line, style, highlight);
                let xa = x0 + layout.x_of(col);
                let xb = x0 + layout.x_of(col + 1);
                let y = geo.line_top(line, view.scroll.y);
                text_painter.rect_stroke(
                    Rect::from_min_max(pos2(xa, y + 1.0), pos2(xb, y + lh - 1.0)),
                    2.0,
                    Stroke::new(1.0, ed.bracket_match),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    // Indent guides.
    if style.settings.indent_guides && !style.settings.low_distraction {
        let unit = if style.settings.use_tabs { tab_width } else { style.settings.indent_width.max(1) };
        let mut prev_indent = 0usize;
        for line in first..last {
            let l = rope.line(line);
            let n = motion::first_non_blank(l);
            let blank = n >= line_len_chars(rope, line);
            let cols = if blank { prev_indent } else { char_col_to_display_col(l, n, tab_width) };
            if !blank {
                prev_indent = cols;
            }
            let y = geo.line_top(line, view.scroll.y);
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
    for line in first..last {
        let y = geo.line_top(line, view.scroll.y);
        let layout = layout_line(ui, view, doc, line, style, highlight);
        max_width = max_width.max(layout.width());
        let galley = layout.galley.clone();
        let gy = y + (lh - galley.rect.height()) / 2.0;
        text_painter.galley(pos2(x0, gy), galley, ed.foreground);
        if line_len_chars(rope, line) > MAX_LAYOUT_CHARS {
            let x = x0 + layout.width() + 4.0;
            text_painter.text(pos2(x, y + lh / 2.0), egui::Align2::LEFT_CENTER, "… (line truncated for display)", style.font_id.clone(), ed.line_number);
        }
    }

    // Whitespace.
    if style.settings.show_whitespace != WhitespaceMode::None {
        let only_selection = style.settings.show_whitespace == WhitespaceMode::Selection;
        for line in first..last {
            let ls = rope.line_to_char(line);
            let len = line_len_chars(rope, line);
            if len == 0 {
                continue;
            }
            let y = geo.line_top(line, view.scroll.y);
            let layout = layout_line(ui, view, doc, line, style, highlight);
            let xs: Vec<(usize, char)> = rope.line(line).chars().take(len.min(MAX_LAYOUT_CHARS)).enumerate().filter(|(_, c)| *c == ' ' || *c == '\t').collect();
            for (col, c) in xs {
                let pos = ls + col;
                if only_selection && !sels.iter().any(|s| s.contains(pos)) {
                    continue;
                }
                let xa = x0 + layout.x_of(col);
                let xb = x0 + layout.x_of(col + 1);
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
        let line = rope.char_to_line(head);
        if line >= first && line < last {
            let col = head - rope.line_to_char(line);
            let x = x0 + layout_line(ui, view, doc, line, style, highlight).x_of(col);
            let y = geo.line_top(line, view.scroll.y);
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
            let line = rope.char_to_line(head);
            if line < first || line >= last {
                continue;
            }
            let col = head - rope.line_to_char(line);
            let scroll_y = view.scroll.y;
            let overwrite = view.overwrite;
            let layout = layout_line(ui, view, doc, line, style, highlight);
            let x = x0 + layout.x_of(col);
            let y = geo.line_top(line, scroll_y);
            let w = if overwrite { (layout.x_of(col + 1) - layout.x_of(col)).max(style.char_width) } else { style.settings.caret_width };
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
    for line in first..last {
        let y = geo.line_top(line, view.scroll.y);
        if style.settings.line_numbers {
            let n = if style.settings.relative_line_numbers && line != primary_line { line.abs_diff(primary_line) } else { line + 1 };
            let colour = if current_lines.contains(&line) { ed.line_number_active } else { ed.line_number };
            painter.text(pos2(geo.text.min.x - GUTTER_PAD - 4.0, y + lh / 2.0), egui::Align2::RIGHT_CENTER, n.to_string(), style.font_id.clone(), colour);
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
                painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, lh)), 0.0, colour);
            }
        }
    }

    // Vertical scrollbar.
    if style.settings.scrollbars && geo.total_lines as f32 * lh > geo.text.height() {
        let track = Rect::from_min_max(pos2(geo.text.max.x, rect.min.y), rect.max);
        let frac_visible = (geo.text.height() / geo.content_height).clamp(0.02, 1.0);
        let thumb_h = (frac_visible * track.height()).max(20.0);
        let frac_pos = view.scroll.y / geo.max_scroll_y().max(1.0);
        let thumb_y = track.min.y + frac_pos * (track.height() - thumb_h);
        let hovered = ui.rect_contains_pointer(track) || view.drag.as_ref().is_some_and(|d| d.origin.anchor == usize::MAX);
        let colour = if hovered { theme.ui.scrollbar_hover } else { theme.ui.scrollbar };
        painter.rect_filled(Rect::from_min_size(pos2(track.min.x + 2.0, thumb_y), vec2(SCROLLBAR_W - 4.0, thumb_h)), 3.0, colour);
    }
    // Horizontal scrollbar (thin, only when content overflows).
    let content_w = max_width + TEXT_PAD * 2.0;
    if style.settings.scrollbars && (content_w > geo.text.width() || view.scroll.x > 0.0) {
        let track_w = geo.text.width();
        let total = content_w.max(view.scroll.x + geo.text.width());
        let frac_visible = (geo.text.width() / total).clamp(0.05, 1.0);
        let thumb_w = (frac_visible * track_w).max(20.0);
        let frac_pos = (view.scroll.x / (total - geo.text.width()).max(1.0)).clamp(0.0, 1.0);
        let x = geo.text.min.x + frac_pos * (track_w - thumb_w);
        painter.rect_filled(Rect::from_min_size(pos2(x, rect.max.y - 6.0), vec2(thumb_w, 4.0)), 2.0, theme.ui.scrollbar);
    }
    geo
}

/// Fill the on-screen area of a char range, line by line.
#[allow(clippy::too_many_arguments)]
fn paint_range(
    ui: &Ui,
    geo: &Geometry,
    view: &mut View,
    doc: &Document,
    style: &EditorStyle,
    highlight: Option<&HighlightFn>,
    painter: &egui::Painter,
    start: usize,
    end: usize,
    first: usize,
    last: usize,
    colour: Color32,
    x0: f32,
    include_newline: bool,
) {
    let rope = doc.rope();
    let n = rope.len_chars();
    let (start, end) = (start.min(n), end.min(n));
    if start >= end {
        return;
    }
    let l0 = rope.char_to_line(start).max(first);
    let l1 = rope.char_to_line(end).min(last.saturating_sub(1));
    if l0 > l1 {
        return;
    }
    let lh = geo.line_height;
    for line in l0..=l1 {
        let ls = rope.line_to_char(line);
        let le = line_end_char(rope, line);
        let a = start.max(ls);
        let b = end.min(le);
        let y = geo.line_top(line, view.scroll.y);
        let layout = layout_line(ui, view, doc, line, style, highlight);
        let xa = x0 + layout.x_of(a - ls);
        let mut xb = x0 + layout.x_of(b - ls);
        if include_newline && end > le && line + 1 < rope.len_lines() {
            xb += style.char_width * 0.5;
        }
        if xb > xa {
            painter.rect_filled(Rect::from_min_max(pos2(xa, y), pos2(xb, y + lh)), 0.0, colour);
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
