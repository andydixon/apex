//! The hex editor for binary files.
//!
//! A binary file opens as a [`HexDoc`] in `ApexApp::hex`, keyed by the tab's
//! document. That `Document` stays an empty, read-only placeholder carrying
//! the path, title and dirty flag, so tabs, Save, Save As, close prompts and
//! session restore work as for any file; the bytes live here.
//!
//! Editing overwrites in place. In the hex column two digits make a byte and
//! the caret moves on by itself; in the text column each character typed
//! writes its UTF-8 bytes. Typing past the last byte appends. Tab switches
//! columns. Rows hold 32, 16 or 8 bytes, whichever fits the pane. Find
//! matches the needle both as text and, when it is made of hex digits
//! (`de ad be ef` or `deadbeef`), as bytes.

use std::ops::Range;

use egui::{Event, Key, Rect, Sense, Ui, pos2, vec2};

use crate::app::ApexApp;
use crate::command::Command;
use crate::keymap::{Context, Dispatch};
use crate::view::ViewId;

/// One byte an edit wrote: offset, previous value (`None` if appended), new value.
type Change = (usize, Option<u8>, u8);

/// Width of the scrollbar on the right of the view.
const SCROLLBAR_W: f32 = 10.0;

pub(crate) struct HexDoc {
    pub(crate) bytes: Vec<u8>,
    /// Caret byte; may equal `bytes.len()` (the slot where typing appends).
    cursor: usize,
    /// The selection's other end (inclusive, like `cursor`), if any.
    anchor: Option<usize>,
    /// The next hex digit typed sets the caret byte's low nibble.
    low: bool,
    /// Typing goes to the text column instead of the hex column.
    ascii: bool,
    /// The file cannot be written.
    pub(crate) read_only: bool,
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    /// `undo.len()` at the last save; `None` once that state cannot come back.
    saved: Option<usize>,
    /// Bumped on every change; find matches are keyed on it.
    pub(crate) generation: u64,
    /// Layout from the last paint: bytes per row, rows on screen, first row shown.
    cols: usize,
    page_rows: usize,
    top: usize,
    /// Scroll the caret into view on the next paint.
    reveal: bool,
    /// Wheel movement not yet applied as whole rows.
    scroll_rest: f32,
}

impl HexDoc {
    pub(crate) fn new(bytes: Vec<u8>, read_only: bool) -> Self {
        Self {
            bytes,
            cursor: 0,
            anchor: None,
            low: false,
            ascii: false,
            read_only,
            undo: Vec::new(),
            redo: Vec::new(),
            saved: Some(0),
            generation: 0,
            cols: 16,
            page_rows: 20,
            top: 0,
            reveal: false,
            scroll_rest: 0.0,
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    /// Take new contents from disk, dropping the undo history.
    pub(crate) fn reload(&mut self, bytes: Vec<u8>) {
        self.bytes = bytes;
        (self.undo, self.redo, self.saved) = (Vec::new(), Vec::new(), Some(0));
        self.cursor = self.cursor.min(self.bytes.len());
        (self.anchor, self.low) = (None, false);
        self.generation += 1;
    }

    /// The selected bytes (the caret byte's range when nothing is selected).
    fn selection(&self) -> Range<usize> {
        let len = self.bytes.len();
        let (a, b) = match self.anchor {
            Some(a) => (a.min(self.cursor), a.max(self.cursor)),
            None => (self.cursor, self.cursor),
        };
        a.min(len)..(b + 1).min(len)
    }

    /// Select `range` (a find match) and bring it into view.
    pub(crate) fn select(&mut self, range: Range<usize>) {
        self.anchor = Some(range.start);
        self.cursor = range.end.saturating_sub(1).max(range.start);
        (self.low, self.reveal) = (false, true);
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Write `bytes` from `at`, appending past the end, as one undo step.
    fn write(&mut self, at: usize, bytes: &[u8]) -> bool {
        if self.read_only || bytes.is_empty() || at > self.bytes.len() {
            return false;
        }
        let mut group = Vec::with_capacity(bytes.len());
        for (i, &b) in bytes.iter().enumerate() {
            let old = match self.bytes.get_mut(at + i) {
                Some(slot) => Some(std::mem::replace(slot, b)),
                None => {
                    self.bytes.push(b);
                    None
                }
            };
            group.push((at + i, old, b));
        }
        // The saved state was undone and is now overwritten for good.
        if self.saved.is_some_and(|s| s > self.undo.len()) {
            self.saved = None;
        }
        self.undo.push(group);
        self.redo.clear();
        self.generation += 1;
        true
    }

    /// Undo (or redo) the last step; the caret goes to where it happened.
    fn undo(&mut self, redo: bool) -> bool {
        let Some(group) = (if redo { self.redo.pop() } else { self.undo.pop() }) else { return false };
        if redo {
            for &(at, old, new) in &group {
                match old {
                    Some(_) => self.bytes[at] = new,
                    None => self.bytes.push(new),
                }
            }
            self.undo.push(group);
        } else {
            for &(at, old, _) in group.iter().rev() {
                match old {
                    Some(o) => self.bytes[at] = o,
                    None => self.bytes.truncate(at),
                }
            }
            self.redo.push(group);
        }
        let at = if redo { self.undo.last() } else { self.redo.last() }.map_or(0, |g| g[0].0);
        self.cursor = at.min(self.bytes.len());
        (self.anchor, self.low, self.reveal) = (None, false, true);
        self.generation += 1;
        true
    }

    /// One hex digit typed in the hex column: fill a nibble, moving on after the low one.
    fn type_hex(&mut self, digit: u8) -> bool {
        let at = self.selection().start.min(self.cursor);
        let old = self.bytes.get(at).copied().unwrap_or(0);
        let new = if self.low { (old & 0xf0) | digit } else { (digit << 4) | (old & 0x0f) };
        if !self.write(at, &[new]) {
            return false;
        }
        self.cursor = if self.low { at + 1 } else { at };
        self.low = !self.low;
        (self.anchor, self.reveal) = (None, true);
        true
    }

    /// Overwrite from the selection (or caret) with `bytes` and move past them.
    fn put(&mut self, bytes: &[u8]) -> bool {
        let at = if self.anchor.is_some() { self.selection().start } else { self.cursor };
        if !self.write(at, bytes) {
            return false;
        }
        self.cursor = at + bytes.len();
        (self.anchor, self.low, self.reveal) = (None, false, true);
        true
    }

    /// The selection as the active column shows it: `de ad be ef` or text.
    fn copy_text(&self) -> String {
        let s = &self.bytes[self.selection()];
        if self.ascii { String::from_utf8_lossy(s).into_owned() } else { s.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ") }
    }

    /// Text typed (or pasted) into the active column.
    fn type_text(&mut self, t: &str, paste: bool) -> bool {
        if self.ascii {
            return self.put(t.as_bytes());
        }
        if paste && let Some((bytes, None)) = parse_hex(t) {
            return self.put(&bytes);
        }
        let mut changed = false;
        for c in t.chars() {
            if let Some(d) = c.to_digit(16) {
                changed |= self.type_hex(d as u8);
            }
        }
        changed
    }

    /// Caret movement and selection commands. Returns false for anything else.
    fn motion(&mut self, c: Command) -> bool {
        use Command::*;
        let (len, cols, cur) = (self.bytes.len(), self.cols.max(1), self.cursor);
        let page = cols * self.page_rows.max(1);
        let (to, select) = match c {
            MoveLeft | SelectLeft | DeleteBackward => (cur.saturating_sub(1), c == SelectLeft),
            MoveRight | SelectRight => (cur + 1, c == SelectRight),
            MoveUp | SelectUp => (if cur >= cols { cur - cols } else { cur }, c == SelectUp),
            MoveDown | SelectDown => (if cur + cols <= len { cur + cols } else { len }, c == SelectDown),
            MovePageUp | SelectPageUp => (cur.saturating_sub(page), c == SelectPageUp),
            MovePageDown | SelectPageDown => (cur + page, c == SelectPageDown),
            MoveLineStart | SelectLineStart => (cur - cur % cols, c == SelectLineStart),
            MoveLineEnd | SelectLineEnd => (cur - cur % cols + cols - 1, c == SelectLineEnd),
            MoveDocStart | SelectDocStart => (0, c == SelectDocStart),
            MoveDocEnd | SelectDocEnd => (len, c == SelectDocEnd),
            SingleSelection => {
                self.anchor = None;
                return true;
            }
            SelectAll => {
                self.anchor = Some(0);
                self.cursor = len.saturating_sub(1);
                self.low = false;
                return true;
            }
            _ => return false,
        };
        if select {
            self.anchor.get_or_insert(cur);
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(len);
        (self.low, self.reveal) = (false, true);
        true
    }

    /// The status bar's left side: caret offset and selection size.
    pub(crate) fn status(&self) -> String {
        let mut s = format!("Offset 0x{:x} ({})", self.cursor, self.cursor);
        let n = self.selection().len();
        if self.anchor.is_some() && n > 1 {
            s.push_str(&format!("    {n} bytes selected"));
        }
        s.push_str(if self.ascii { "    TEXT" } else { "    HEX" });
        s
    }
}

/// Hex digits (spaces allowed) as bytes plus a trailing odd nibble, or
/// `None` if `s` is not at least two hex digits.
fn parse_hex(s: &str) -> Option<(Vec<u8>, Option<u8>)> {
    let digits: Vec<u8> = s.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    if digits.len() < 2 {
        return None;
    }
    let bytes = digits.as_chunks::<2>().0.iter().map(|p| (p[0] << 4) | p[1]).collect();
    Some((bytes, (digits.len() % 2 == 1).then(|| digits[digits.len() - 1])))
}

/// Byte ranges matching `needle` as text and, when it is hex digits, as
/// bytes (an odd last digit matches the next byte's high nibble). Sorted,
/// at most `limit`.
pub(crate) fn find_all(bytes: &[u8], needle: &str, case_sensitive: bool, limit: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let text = needle.as_bytes();
    if !text.is_empty() {
        for (i, w) in bytes.windows(text.len()).enumerate() {
            if if case_sensitive { w == text } else { w.eq_ignore_ascii_case(text) } {
                out.push(i..i + text.len());
            }
        }
    }
    if let Some((pat, nibble)) = parse_hex(needle) {
        let n = pat.len() + nibble.is_some() as usize;
        for (i, w) in bytes.windows(n).enumerate() {
            if w[..pat.len()] == pat[..] && nibble.is_none_or(|h| w[n - 1] >> 4 == h) {
                out.push(i..i + n);
            }
        }
    }
    out.sort_by_key(|r| (r.start, r.end));
    out.dedup();
    out.truncate(limit);
    out
}

impl ApexApp {
    /// The hex editor of the active tab, if it shows one.
    pub(crate) fn active_hex(&mut self) -> Option<&mut HexDoc> {
        let (_, d) = self.active_ids()?;
        self.hex.get_mut(&d)
    }

    /// Commands the hex editor answers itself while it is the active tab.
    pub(crate) fn hex_command(&mut self, ctx: &egui::Context, cmd: Command) -> bool {
        let Some((_, did)) = self.active_ids() else { return false };
        let Some(h) = self.hex.get_mut(&did) else { return false };
        let changed = match cmd {
            Command::Undo | Command::Redo => h.undo(cmd == Command::Redo),
            Command::Copy | Command::Cut => {
                ctx.copy_text(h.copy_text());
                false
            }
            Command::ReplaceNext | Command::ReplaceAll => {
                self.set_status("Replace is not available in the hex editor".into(), true);
                return true;
            }
            c => return h.motion(c),
        };
        if changed {
            self.hex_sync_dirty(did);
        }
        true
    }

    /// Carry the hex editor's unsaved state over to the tab's document.
    fn hex_sync_dirty(&mut self, did: apex_edit::DocId) {
        let dirty = self.hex.get(&did).is_some_and(HexDoc::dirty);
        if let Some(d) = self.docs.get_mut(&did) {
            d.set_dirty(dirty);
        }
    }

    /// Draw a hex view and handle its mouse and keyboard input.
    pub(crate) fn show_hex(&mut self, ui: &mut Ui, pane_id: crate::workspace::PaneId, vid: ViewId, rect: Rect, focused: bool) {
        let Some(did) = self.views.get(&vid).map(|v| v.doc) else { return };
        let font = crate::editor::font_for(&self.settings, self.font_zoom);
        let (lh, cw) = crate::editor::metrics(ui.ctx(), &font, self.settings.line_height);
        let theme = self.theme.clone();
        let matches: Vec<Range<usize>> =
            if self.find.open && self.find.rev.as_ref().is_some_and(|r| r.0 == did) { self.find.matches.clone() } else { Vec::new() };
        let current = self.find.current.and_then(|i| matches.get(i).cloned());

        let id = vid.egui_id();
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        ui.memory_mut(|m| m.interested_in_focus(id, ui.layer_id()));
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        let has_focus = ui.memory(|m| m.has_focus(id));

        let Some(h) = self.hex.get_mut(&did) else { return };
        // Columns: offset (8) + 2, three per byte plus one between groups of 8, 2, one per byte.
        let need = |n: usize| (10 + n * 3 + n / 8 - 1 + 2 + n) as f32 * cw;
        let avail = rect.width() - 16.0 - SCROLLBAR_W;
        let cols = [32, 16, 8].into_iter().find(|&n| need(n) <= avail).unwrap_or(8);
        let x0 = rect.min.x + 8.0;
        let y0 = rect.min.y + 4.0;
        let hex_x = |i: usize| x0 + (10 + i * 3 + i / 8) as f32 * cw;
        let text_x = |i: usize| x0 + (10 + cols * 3 + cols / 8 - 1 + 2 + i) as f32 * cw;
        let total_rows = h.bytes.len() / cols + 1;
        let page = ((rect.height() - 8.0) / lh).floor().max(1.0) as usize;
        // Keep the caret's row where it was when the column count changes.
        if cols != h.cols {
            h.top = h.top * h.cols / cols;
            h.reveal = true;
        }
        (h.cols, h.page_rows) = (cols, page);

        // Wheel scrolling, in whole rows.
        if resp.hovered() {
            h.scroll_rest -= ui.input(|i| i.smooth_scroll_delta.y);
        }
        let rows = (h.scroll_rest / lh).trunc();
        h.scroll_rest -= rows * lh;
        h.top = (h.top as i64 + rows as i64).clamp(0, total_rows.saturating_sub(1) as i64) as usize;

        // Mouse: click to place the caret (either column), drag or Shift+click to select.
        let track = Rect::from_min_max(pos2(rect.max.x - SCROLLBAR_W, rect.min.y), rect.max);
        let mut focus = false;
        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            let pressed = ui.input(|i| i.pointer.primary_pressed()) && resp.hovered();
            let down = ui.input(|i| i.pointer.primary_down()) && resp.is_pointer_button_down_on();
            if (pressed || down) && track.contains(resp.interact_pointer_pos().unwrap_or(p)) {
                let frac = ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
                h.top = ((total_rows as f32 * frac) as usize).min(total_rows.saturating_sub(1));
            } else if pressed || down {
                let row = h.top as f32 + ((p.y - y0) / lh).floor().max(0.0);
                let in_text = p.x >= text_x(0) - cw;
                let col = if in_text { ((p.x - text_x(0)) / cw).floor() } else { (0..cols).rev().find(|&i| p.x >= hex_x(i) - cw / 2.0).unwrap_or(0) as f32 };
                let at = ((row as usize) * cols + col.clamp(0.0, cols as f32 - 1.0) as usize).min(h.bytes.len());
                if pressed {
                    focus = true;
                    let shift = ui.input(|i| i.modifiers.shift);
                    h.anchor = if shift { Some(h.anchor.unwrap_or(h.cursor)) } else { Some(at) };
                    h.ascii = in_text;
                    h.low = false;
                }
                h.cursor = at;
                if !pressed && p.y < rect.min.y {
                    h.top = h.top.saturating_sub(1);
                } else if !pressed && p.y > rect.max.y {
                    h.top = (h.top + 1).min(total_rows.saturating_sub(1));
                }
            }
            if resp.drag_stopped() && h.anchor == Some(h.cursor) {
                h.anchor = None;
            }
        }
        if resp.clicked() && h.anchor == Some(h.cursor) {
            h.anchor = None;
        }

        // Keyboard.
        let mut commands = Vec::new();
        let mut changed = false;
        if has_focus && focused {
            let events = ui.input(|i| i.events.clone());
            let mut swallow: Option<Key> = None;
            for ev in events {
                match ev {
                    Event::Key { key: Key::Tab, pressed: true, modifiers, .. } if !modifiers.ctrl && !modifiers.command && !modifiers.alt => {
                        h.ascii = !h.ascii;
                        h.low = false;
                        swallow = Some(Key::Tab);
                    }
                    Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                        match self.keymap.dispatch_event(key, physical_key, modifiers, &[Context::Editor, Context::Global]) {
                            Dispatch::Command(c, arg) => {
                                swallow = Some(key);
                                if !h.motion(c) {
                                    commands.push((c, arg));
                                }
                            }
                            Dispatch::Pending => swallow = Some(key),
                            Dispatch::Unbound => {}
                        }
                    }
                    Event::Text(t) => {
                        if swallow.take().is_some_and(|k| k.symbol_or_name().eq_ignore_ascii_case(&t)) {
                            continue;
                        }
                        changed |= h.type_text(&t, false);
                    }
                    Event::Paste(t) => changed |= h.type_text(&t, true),
                    Event::Copy | Event::Cut => ui.ctx().copy_text(h.copy_text()),
                    _ => {}
                }
            }
        }
        if changed && h.read_only {
            changed = false;
        }

        // Keep the caret on screen after keyboard moves and edits.
        if std::mem::take(&mut h.reveal) {
            let row = h.cursor / cols;
            if row < h.top {
                h.top = row;
            } else if row >= h.top + page {
                h.top = row + 1 - page;
            }
        }

        // Paint.
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme.editor.background);
        let sel = h.anchor.map(|_| h.selection());
        let in_match = |at: usize| {
            matches
                .binary_search_by(|m| {
                    if m.end <= at {
                        std::cmp::Ordering::Less
                    } else if m.start > at {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .is_ok()
        };
        let len = h.bytes.len();
        for k in 0..page.min(total_rows - h.top.min(total_rows)) {
            let row = h.top + k;
            let y = y0 + k as f32 * lh;
            painter.text(pos2(x0, y), egui::Align2::LEFT_TOP, format!("{:08x}", row * cols), font.clone(), theme.editor.line_number);
            for i in 0..cols {
                let at = row * cols + i;
                if at > len {
                    break;
                }
                let bg = if sel.as_ref().is_some_and(|s| s.contains(&at)) {
                    Some(theme.editor.selection)
                } else if current.as_ref().is_some_and(|m| m.contains(&at)) {
                    Some(theme.editor.search_match_current)
                } else if in_match(at) {
                    Some(theme.editor.search_match)
                } else {
                    None
                };
                let hex_cell = Rect::from_min_size(pos2(hex_x(i) - 1.0, y), vec2(cw * 2.0 + 2.0, lh));
                let text_cell = Rect::from_min_size(pos2(text_x(i), y), vec2(cw, lh));
                if let Some(c) = bg {
                    painter.rect_filled(hex_cell, 2.0, c);
                    painter.rect_filled(text_cell, 0.0, c);
                }
                if at == h.cursor {
                    let caret = theme.editor.caret;
                    let (active, other) = if h.ascii { (text_cell, hex_cell) } else { (hex_cell, text_cell) };
                    let active = if h.ascii { active } else { Rect::from_min_size(pos2(hex_x(i) + if h.low { cw } else { 0.0 }, y), vec2(cw, lh)) };
                    painter.rect_stroke(other, 1.0, egui::Stroke::new(1.0, theme.editor.line_number), egui::StrokeKind::Inside);
                    if focused {
                        painter.rect_filled(Rect::from_min_size(pos2(active.min.x, active.max.y - 2.0), vec2(active.width(), 2.0)), 0.0, caret);
                    }
                    painter.rect_stroke(active, 1.0, egui::Stroke::new(1.0, caret), egui::StrokeKind::Inside);
                }
                let Some(&b) = h.bytes.get(at) else { continue };
                let fg = if b == 0 { theme.editor.line_number } else { theme.editor.foreground };
                painter.text(pos2(hex_x(i), y), egui::Align2::LEFT_TOP, format!("{b:02x}"), font.clone(), fg);
                let (ch, fg) = if (0x20..0x7f).contains(&b) { (b as char, theme.editor.foreground) } else { ('.', theme.editor.line_number) };
                painter.text(pos2(text_x(i), y), egui::Align2::LEFT_TOP, ch, font.clone(), fg);
            }
        }
        // Scrollbar thumb.
        if total_rows > page {
            let h_thumb = (rect.height() * page as f32 / total_rows as f32).max(20.0);
            let t = rect.min.y + (rect.height() - h_thumb) * h.top as f32 / (total_rows - page).max(1) as f32;
            painter.rect_filled(
                Rect::from_min_size(pos2(track.min.x + 2.0, t.min(rect.max.y - h_thumb)), vec2(SCROLLBAR_W - 4.0, h_thumb)),
                3.0,
                theme.ui.scrollbar,
            );
        }

        if focus {
            ui.memory_mut(|m| m.request_focus(id));
            self.active_pane = pane_id;
        }
        if changed {
            self.hex_sync_dirty(did);
            self.make_permanent(vid);
        }
        for (c, arg) in commands {
            self.execute(ui.ctx(), c, arg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_editing_and_undo() {
        let mut h = HexDoc::new(vec![0x00, 0x11, 0x22], false);
        // Four digits fill two bytes without moving the caret by hand.
        assert!(h.type_text("abcd", false));
        assert_eq!(h.bytes, [0xab, 0xcd, 0x22]);
        assert_eq!(h.cursor, 2);
        assert!(h.dirty());
        // The text column writes bytes, appending past the end.
        h.ascii = true;
        h.type_text("Hi", false);
        assert_eq!(h.bytes, [0xab, 0xcd, b'H', b'i']);
        // Undo walks back, through the append too, to the saved state.
        while h.undo(false) {}
        assert_eq!(h.bytes, [0x00, 0x11, 0x22]);
        assert!(!h.dirty());
        h.undo(true);
        assert_eq!(h.bytes[0], 0xa0);
        // Pasting hex into the hex column writes those bytes.
        h.ascii = false;
        h.cursor = 1;
        h.type_text("de ad", true);
        assert_eq!(h.bytes, [0xa0, 0xde, 0xad]);
        h.anchor = Some(1);
        h.cursor = 2;
        assert_eq!(h.copy_text(), "de ad");
        // Read-only files take no edits.
        let mut r = HexDoc::new(vec![1], true);
        assert!(!r.type_text("ff", false));
        assert_eq!(r.bytes, [1]);
    }

    #[test]
    fn motion_wraps_rows() {
        let mut h = HexDoc::new((0..40).collect(), false);
        h.cols = 16;
        h.motion(Command::MoveDown);
        h.motion(Command::MoveRight);
        assert_eq!(h.cursor, 17);
        h.motion(Command::MoveLineEnd);
        assert_eq!(h.cursor, 31);
        h.motion(Command::SelectDown);
        assert_eq!(h.selection(), 31..40);
        h.motion(Command::MoveDocStart);
        assert_eq!((h.cursor, h.anchor), (0, None));
        h.motion(Command::MoveUp);
        assert_eq!(h.cursor, 0);
    }

    #[test]
    fn finds_hex_and_text() {
        let data = b"\x00\xcc\xde\xad\xbe\xef..DEAD";
        let f = |needle: &str, case: bool| find_all(data, needle, case, 10).into_iter().map(|r| (r.start, r.end)).collect::<Vec<_>>();
        // Spaced, packed, and an odd digit matching the next high nibble.
        assert_eq!(f("de ad be ef", false), [(2, 6)]);
        assert_eq!(f("deadbeef", false), [(2, 6)]);
        assert_eq!(f("00ccdeadbeeef", false), []);
        assert_eq!(f("00ccdeadbeefe", false), []);
        assert_eq!(f("00ccdeadbee", false), [(0, 6)]);
        // Text, case-insensitive unless asked; "dead" also matches as hex.
        assert_eq!(f("dead", false), [(2, 4), (8, 12)]);
        assert_eq!(f("Dead", true), [(2, 4)]);
        assert_eq!(parse_hex("a"), None);
    }
}
