//! Pure editing operations.
//!
//! Every function takes the document and the current selections and returns
//! an [`EditResult`] describing the transaction and the selections after it.
//! Nothing here mutates the document; the caller applies the result through
//! [`Document::apply`], which keeps multi-cursor edits atomic and undoable.

use apex_buffer::motion::{self, first_non_blank, next_grapheme_boundary, prev_grapheme_boundary};
use apex_buffer::position::{line_end_char, line_len_chars};
use apex_buffer::{LineEnding, Rope};

use crate::document::{Document, EditKind};
use crate::indent::IndentSettings;
use crate::selection::{Selection, SelectionSet};
use crate::transaction::{Assoc, Change, Transaction};

/// Outcome of an editing operation, ready for `Document::apply`.
#[derive(Debug, Clone)]
pub struct EditResult {
    /// Changes to apply, in char indices of the document before the edit. May be
    /// empty when only the selections move (for example stepping over a closer).
    pub tx: Transaction,
    /// Selections after the edit, in char indices of the edited document.
    pub sels: SelectionSet,
    /// Undo grouping hint: `Typing` and the delete kinds may coalesce with the
    /// previous undo entry.
    pub kind: EditKind,
}

impl EditResult {
    /// Wrap the parts, returning `None` when `tx` changes nothing so callers can
    /// treat no-op commands uniformly.
    fn new(tx: Transaction, sels: SelectionSet, kind: EditKind) -> Option<Self> {
        if tx.is_empty() { None } else { Some(Self { tx, sels, kind }) }
    }
}

/// Options influencing plain text insertion.
#[derive(Debug, Clone, Copy)]
pub struct InsertOptions {
    /// Insert the closing partner of a typed bracket or quote, wrap selections
    /// in it, and step over an existing closer.
    pub auto_pair: bool,
    /// Not read by `insert_text`; auto-indent on Enter is the `auto_indent`
    /// argument of `newline`.
    pub auto_indent: bool,
    /// Overtype mode: a single typed char replaces the grapheme after each caret.
    pub overwrite: bool,
}

impl Default for InsertOptions {
    fn default() -> Self {
        Self { auto_pair: true, auto_indent: true, overwrite: false }
    }
}

/// Auto-pair table: `(opener, closer)`. Quotes pair with themselves.
const PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\''), ('`', '`')];

/// Closer for opener `c`, or `None` if `c` does not open a pair.
fn closing_of(c: char) -> Option<char> {
    PAIRS.iter().find(|(o, _)| *o == c).map(|(_, c)| *c)
}

/// Whether `c` is the closing half of any `PAIRS` entry (quotes included).
fn is_closer(c: char) -> bool {
    PAIRS.iter().any(|(_, x)| *x == c)
}

/// Build a result where each selection's range is replaced by the text
/// yielded by `f` and the cursor collapses after the inserted text.
///
/// Selections are visited in ascending order; `delta` accumulates the length
/// change so far so each new caret is placed in post-edit char indices.
fn replace_each(doc: &Document, sels: &SelectionSet, kind: EditKind, mut f: impl FnMut(&Selection) -> String) -> Option<EditResult> {
    // `doc` is unused; kept so every operation has the same shape.
    let _ = doc;
    let mut changes = Vec::with_capacity(sels.len());
    let mut new_sels = Vec::with_capacity(sels.len());
    let mut delta: isize = 0;
    for s in sels.iter() {
        let text = f(s);
        let new_len = text.chars().count();
        let pos = (s.start() as isize + delta) as usize + new_len;
        new_sels.push(Selection::point(pos));
        delta += new_len as isize - s.len() as isize;
        changes.push(Change::replace(s.start(), s.end(), text));
    }
    EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), kind)
}

/// Insert typed text at every caret, with auto-pairing and overtype.
///
/// `text` may be several chars (IME commit, paste-like input); pairing,
/// step-over and overwrite only apply when it is a single char. Returns
/// `Some` with an empty transaction when the only effect is stepping over
/// a closer. A single non-newline char is tagged `EditKind::Typing` so
/// consecutive keystrokes share an undo entry.
pub fn insert_text(doc: &Document, sels: &SelectionSet, text: &str, opts: InsertOptions) -> Option<EditResult> {
    let rope = doc.rope();
    let mut chars = text.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    };

    // Auto-pair / step-over only applies to single-char input.
    if let (Some(c), true) = (single, opts.auto_pair) {
        if let Some(close) = closing_of(c) {
            // Wrap non-empty selections.
            if sels.has_extent() && sels.iter().all(|s| !s.is_empty()) {
                return surround(doc, sels, &c.to_string(), &close.to_string());
            }
            // Quotes: do not pair after a word character (apostrophes) or before one.
            let should_pair = |s: &Selection| {
                let next = rope.get_char(s.head);
                let prev = if s.head > 0 { rope.get_char(s.head - 1) } else { None };
                let next_ok = next.is_none_or(|n| n.is_whitespace() || is_closer(n) || n == ';' || n == ',');
                if close == c {
                    let prev_ok = prev.is_none_or(|p| !(p.is_alphanumeric() || p == '_' || p == close));
                    next_ok && prev_ok
                } else {
                    next_ok
                }
            };
            if sels.iter().all(|s| s.is_empty()) {
                // Step over an existing closer.
                if is_closer(c) && sels.iter().all(|s| rope.get_char(s.head) == Some(c)) {
                    let mut ns = sels.clone();
                    ns.transform(|s| Selection::point(s.head + 1));
                    return Some(EditResult { tx: Transaction::default(), sels: ns, kind: EditKind::Other });
                }
                if sels.iter().all(should_pair) {
                    let mut changes = Vec::with_capacity(sels.len());
                    let mut new_sels = Vec::with_capacity(sels.len());
                    for (i, s) in sels.iter().enumerate() {
                        changes.push(Change::insert(s.head, format!("{c}{close}")));
                        // Each earlier caret has gained two chars, hence `2 * i`.
                        new_sels.push(Selection::point(s.head + 1 + 2 * i));
                    }
                    return EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), EditKind::Typing);
                }
            }
        // Typing a closer that is not also an opener (`)`, `]`, `}`) steps over
        // an identical char at every caret.
        } else if is_closer(c) && sels.iter().all(|s| s.is_empty() && rope.get_char(s.head) == Some(c)) {
            let mut ns = sels.clone();
            ns.transform(|s| Selection::point(s.head + 1));
            return Some(EditResult { tx: Transaction::default(), sels: ns, kind: EditKind::Other });
        }
    }

    if opts.overwrite && single.is_some() && sels.iter().all(|s| s.is_empty()) {
        let mut changes = Vec::with_capacity(sels.len());
        let mut new_sels = Vec::with_capacity(sels.len());
        let mut delta: isize = 0;
        for s in sels.iter() {
            let next = rope.get_char(s.head);
            // Never overtype a line terminator: at end of line this is a plain insert.
            let end = if next.is_some_and(|n| n != '\n' && n != '\r') { next_grapheme_boundary(rope.slice(..), s.head) } else { s.head };
            changes.push(Change::replace(s.head, end, text));
            let pos = (s.head as isize + delta) as usize + 1;
            new_sels.push(Selection::point(pos));
            delta += 1 - (end - s.head) as isize;
        }
        return EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), EditKind::Typing);
    }

    let kind = if single.is_some_and(|c| c != '\n' && c != '\r') { EditKind::Typing } else { EditKind::Other };
    replace_each(doc, sels, kind, |_| text.to_owned())
}

/// Paste: when the clipboard holds exactly as many lines as there are carets,
/// distribute one line per caret; otherwise insert the whole text everywhere.
///
/// Line endings in `text` are normalised to LF first; the document's
/// own line ending is applied elsewhere. Distribution requires more than one
/// caret and no trailing newline on the clipboard text.
pub fn paste(doc: &Document, sels: &SelectionSet, text: &str) -> Option<EditResult> {
    let text = apex_buffer::line_ending::normalise(text, LineEnding::Lf);
    let lines: Vec<&str> = text.lines().collect();
    if sels.len() > 1 && lines.len() == sels.len() && !text.ends_with('\n') {
        let mut i = 0;
        return replace_each(doc, sels, EditKind::Other, |_| {
            let l = lines[i].to_owned();
            i += 1;
            l
        });
    }
    // Mechanically nothing here special-cases whole-line pastes (see the
    // next comment): the text replaces each selection as-is.
    // Whole-line paste (copied with no selection) goes above the caret's line.
    replace_each(doc, sels, EditKind::Other, |_| text.clone())
}

/// Leading whitespace of the line containing `pos`.
///
/// `pos` is a char index, clamped to the document length.
fn line_indent_at(rope: &Rope, pos: usize) -> String {
    let line = rope.char_to_line(pos.min(rope.len_chars()));
    let l = rope.line(line);
    let n = first_non_blank(l);
    l.slice(..n).into()
}

/// Insert a line break at each caret with automatic indentation.
///
/// With `auto_indent`, the new line copies the current line's leading
/// whitespace, adds one `indent.unit()` after an opening bracket, and when
/// the caret sits between a bracket pair also pushes the closer onto its own
/// line at the base indent, leaving the caret on the middle line.
pub fn newline(doc: &Document, sels: &SelectionSet, indent: IndentSettings, auto_indent: bool) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::with_capacity(sels.len());
    let mut new_sels = Vec::with_capacity(sels.len());
    let mut delta: isize = 0;
    for s in sels.iter() {
        let mut text = String::from("\n");
        let mut caret_offset_in_text = 1;
        if auto_indent {
            let base = line_indent_at(rope, s.start());
            // Last non-whitespace char before the caret on this line; stopping at
            // `\n` means a bracket on an earlier line does not count.
            let prev = rope.slice(..s.start()).chars_at(s.start()).reversed().find(|c| !c.is_whitespace() || *c == '\n');
            let next = rope.get_char(s.end());
            let opens = matches!(prev, Some('{' | '(' | '['));
            let closes = matches!((prev, next), (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']')));
            text.push_str(&base);
            if opens {
                text.push_str(&indent.unit());
            }
            caret_offset_in_text = text.chars().count();
            if closes {
                text.push('\n');
                text.push_str(&base);
            }
        }
        let new_len = text.chars().count();
        let pos = (s.start() as isize + delta) as usize + caret_offset_in_text;
        new_sels.push(Selection::point(pos));
        delta += new_len as isize - s.len() as isize;
        changes.push(Change::replace(s.start(), s.end(), text));
    }
    EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), EditKind::Other)
}

/// Delete the selection, or one grapheme (or one indent level of leading
/// whitespace) before each caret.
///
/// With `smart` and spaces-mode indentation, a caret preceded only by spaces
/// on its line deletes back to the previous indent stop. Otherwise, if the
/// caret sits between an opener and its closer (for example `(|)`), both
/// are removed. Carets at char index 0 are left as they are.
pub fn delete_backward(doc: &Document, sels: &SelectionSet, indent: IndentSettings, smart: bool) -> Option<EditResult> {
    let rope = doc.rope();
    let text = rope.slice(..);
    let mut changes = Vec::with_capacity(sels.len());
    let mut new_sels = Vec::with_capacity(sels.len());
    let mut delta: isize = 0;
    for s in sels.iter() {
        let (start, end) = if !s.is_empty() {
            (s.start(), s.end())
        } else if s.head == 0 {
            new_sels.push(Selection::point(((s.head as isize) + delta) as usize));
            continue;
        } else {
            let line = rope.char_to_line(s.head);
            let line_start = rope.line_to_char(line);
            let before: String = rope.slice(line_start..s.head).into();
            let mut start = prev_grapheme_boundary(text, s.head);
            if smart && !before.is_empty() && before.chars().all(|c| c == ' ') && indent.style == crate::indent::IndentStyle::Spaces {
                let w = indent.indent_width.max(1);
                // `before` is all ASCII spaces, so its byte length is its column count.
                let back = before.len() % w;
                let back = if back == 0 { w } else { back };
                start = s.head - back;
            } else if let Some(prev) = rope.get_char(s.head - 1) {
                // Delete an auto-inserted pair together.
                if let Some(close) = closing_of(prev)
                    && rope.get_char(s.head) == Some(close)
                {
                    let end = s.head + 1;
                    changes.push(Change::delete(start, end));
                    new_sels.push(Selection::point((start as isize + delta) as usize));
                    delta -= (end - start) as isize;
                    continue;
                }
            }
            (start, s.head)
        };
        changes.push(Change::delete(start, end));
        new_sels.push(Selection::point((start as isize + delta) as usize));
        delta -= (end - start) as isize;
    }
    EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), EditKind::DeleteBackward)
}

/// Delete the selection or one grapheme after each caret.
///
/// Tagged `EditKind::DeleteForward` so repeated presses coalesce in undo.
pub fn delete_forward(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let text = doc.slice();
    delete_ranges(sels, EditKind::DeleteForward, |s| if s.is_empty() { (s.head, next_grapheme_boundary(text, s.head)) } else { (s.start(), s.end()) })
}

/// Delete each selection, or from each caret back to the previous word start.
pub fn delete_word_backward(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let text = doc.slice();
    delete_ranges(sels, EditKind::Other, |s| if s.is_empty() { (motion::prev_word_start(text, s.head), s.head) } else { (s.start(), s.end()) })
}

/// Delete each selection, or from each caret forward to the next word end.
pub fn delete_word_forward(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let text = doc.slice();
    delete_ranges(sels, EditKind::Other, |s| if s.is_empty() { (s.head, motion::next_word_end(text, s.head)) } else { (s.start(), s.end()) })
}

/// Delete from the start of each caret's line up to the caret (the
/// selection's head; its extent is ignored).
pub fn delete_to_line_start(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    delete_ranges(sels, EditKind::Other, |s| (rope.line_to_char(rope.char_to_line(s.head)), s.head))
}

/// Delete from each caret (the selection's head) to the end of its line,
/// leaving the terminator.
pub fn delete_to_line_end(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    delete_ranges(sels, EditKind::Other, |s| (s.head, line_end_char(rope, rope.char_to_line(s.head))))
}

/// Delete the char range `range_of` yields for each selection (either order
/// accepted) and collapse each selection to the start of its deleted range,
/// in post-edit char indices. Empty ranges produce no change.
fn delete_ranges(sels: &SelectionSet, kind: EditKind, mut range_of: impl FnMut(&Selection) -> (usize, usize)) -> Option<EditResult> {
    let mut changes = Vec::with_capacity(sels.len());
    let mut new_sels = Vec::with_capacity(sels.len());
    let mut delta: isize = 0;
    for s in sels.iter() {
        let (start, end) = range_of(s);
        let (start, end) = (start.min(end), start.max(end));
        new_sels.push(Selection::point((start as isize + delta) as usize));
        if start < end {
            changes.push(Change::delete(start, end));
            delta -= (end - start) as isize;
        }
    }
    EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), kind)
}

/// Wrap each selection in `open`…`close`, keeping the inner text selected.
///
/// `open` and `close` may be multi-char strings. Each selection keeps its
/// direction.
pub fn surround(doc: &Document, sels: &SelectionSet, open: &str, close: &str) -> Option<EditResult> {
    // `doc` is unused; kept so every operation has the same shape.
    let _ = doc;
    let mut changes = Vec::with_capacity(sels.len() * 2);
    for s in sels.iter() {
        changes.push(Change::insert(s.start(), open));
        changes.push(Change::insert(s.end(), close));
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| {
        let start = tx.map_pos(s.start(), Assoc::After);
        // The close insertion sits exactly at s.end(); map Before so we stay inside it.
        let end = tx.map_pos(s.end(), Assoc::Before);
        if s.is_reversed() { Selection::new(end, start) } else { Selection::new(start, end) }
    });
    EditResult::new(tx, ns, EditKind::Other)
}

/// Inclusive line range spanned by a selection. A selection ending at column
/// 0 of a later line does not include that line (Sublime behaviour).
///
/// Returns zero-based `(first, last)` line indices.
pub fn lines_of(rope: &Rope, s: &Selection) -> (usize, usize) {
    let first = rope.char_to_line(s.start());
    let mut last = rope.char_to_line(s.end());
    if last > first && rope.line_to_char(last) == s.end() {
        last -= 1;
    }
    (first, last)
}

/// Merge the line ranges of all selections into disjoint sorted ranges.
///
/// Ranges are inclusive line indices; overlapping or adjacent ranges are
/// joined so each line is visited once.
fn merged_line_ranges(rope: &Rope, sels: &SelectionSet) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = sels.iter().map(|s| lines_of(rope, s)).collect();
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for r in ranges {
        if let Some(last) = out.last_mut()
            && r.0 <= last.1 + 1
        {
            last.1 = last.1.max(r.1);
            continue;
        }
        out.push(r);
    }
    out
}

/// Indent every line touched by a selection by one level; selections are
/// preserved (mapped) so repeated presses keep working.
///
/// Inserts `indent.unit()` at the start of each line. Blank lines are skipped
/// when a range spans several lines.
pub fn indent_lines(doc: &Document, sels: &SelectionSet, indent: IndentSettings) -> Option<EditResult> {
    let rope = doc.rope();
    let unit = indent.unit();
    let mut changes = Vec::new();
    for (a, b) in merged_line_ranges(rope, sels) {
        for line in a..=b {
            // `sels.is_empty()` is always false, so this reduces to "blank line in a
            // multi-line range".
            if line_len_chars(rope, line) == 0 && !sels.is_empty() && (a != b) {
                continue; // keep blank lines blank in block indents
            }
            changes.push(Change::insert(rope.line_to_char(line), unit.clone()));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| {
        // `After` keeps a selection that starts at column 0 after the inserted
        // indent rather than before it.
        let a = tx.map_pos(s.anchor, Assoc::After);
        let h = tx.map_pos(s.head, Assoc::After);
        Selection::new(a, h)
    });
    EditResult::new(tx, ns, EditKind::Other)
}

/// Remove one level of indentation from every touched line.
///
/// Per line removes either one leading tab or up to `indent_width` leading
/// spaces, whichever comes first.
pub fn outdent_lines(doc: &Document, sels: &SelectionSet, indent: IndentSettings) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    for (a, b) in merged_line_ranges(rope, sels) {
        for line in a..=b {
            let start = rope.line_to_char(line);
            let l = rope.line(line);
            let mut remove = 0usize;
            let mut cols = 0usize;
            for c in l.chars() {
                match c {
                    '\t' => {
                        remove += 1;
                        break;
                    }
                    ' ' if cols < indent.indent_width.max(1) => {
                        remove += 1;
                        cols += 1;
                    }
                    _ => break,
                }
            }
            if remove > 0 {
                changes.push(Change::delete(start, start + remove));
            }
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Insert `unit` at the caret, or indent lines if the selection spans lines.
///
/// In spaces mode the inserted run reaches the next multiple of
/// `indent_width` display columns (tabs expanded at `tab_width`), so it
/// aligns rather than always inserting a full unit.
pub fn tab(doc: &Document, sels: &SelectionSet, indent: IndentSettings) -> Option<EditResult> {
    let rope = doc.rope();
    let multiline = sels.iter().any(|s| !s.is_empty() && lines_of(rope, s).0 != lines_of(rope, s).1);
    if multiline {
        return indent_lines(doc, sels, indent);
    }
    replace_each(doc, sels, EditKind::Other, |s| match indent.style {
        crate::indent::IndentStyle::Tabs => "\t".into(),
        crate::indent::IndentStyle::Spaces => {
            let line = rope.char_to_line(s.start());
            let col = apex_buffer::position::char_col_to_display_col(rope.line(line), s.start() - rope.line_to_char(line), indent.tab_width);
            let w = indent.indent_width.max(1);
            " ".repeat(w - col % w)
        }
    })
}

/// Duplicate the selected text, or the whole line for empty selections.
///
/// A duplicated line is inserted above the original with that line's own
/// terminator (or the document default for the last line), and the caret
/// moves down with the original. A duplicated selection is inserted right
/// after itself and the copy becomes selected.
pub fn duplicate(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    let mut new_sels = Vec::new();
    let mut delta: isize = 0;
    for s in sels.iter() {
        if s.is_empty() {
            let line = rope.char_to_line(s.head);
            let start = rope.line_to_char(line);
            let end = line_end_char(rope, line);
            let mut text: String = rope.slice(start..end).into();
            let term = apex_buffer::line_ending::terminator_of_line(rope, line).unwrap_or(doc.line_ending);
            text.push_str(term.as_str());
            let n = text.chars().count();
            changes.push(Change::insert(start, text));
            new_sels.push(Selection::point((s.head as isize + delta) as usize + n));
            delta += n as isize;
        } else {
            let text: String = rope.slice(s.range()).into();
            let n = text.chars().count();
            changes.push(Change::insert(s.end(), text));
            let a = (s.end() as isize + delta) as usize;
            new_sels.push(Selection::new(a, a + n));
            delta += n as isize;
        }
    }
    EditResult::new(Transaction::new(changes), SelectionSet::from_vec(new_sels, sels.primary_index()), EditKind::Other)
}

/// Delete whole lines touched by the selections.
///
/// Each selection collapses to the mapped start of its range.
pub fn delete_lines(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    for (a, b) in merged_line_ranges(rope, sels) {
        let start = rope.line_to_char(a);
        let end = if b + 1 < rope.len_lines() { rope.line_to_char(b + 1) } else { rope.len_chars() };
        // Deleting the last line: also remove the preceding terminator.
        let start = if end == rope.len_chars() && a > 0 && b + 1 >= rope.len_lines() { line_end_char(rope, a - 1) } else { start };
        changes.push(Change::delete(start, end));
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| Selection::point(tx.map_pos(s.start(), Assoc::Before)));
    EditResult::new(tx, ns, EditKind::Other)
}

/// Move the lines touched by each selection up or down by one.
///
/// `down` picks the direction. Ranges already at the top (or bottom) are
/// skipped; returns `None` if every range was. Selections move with their
/// lines and keep their extent.
pub fn move_lines(doc: &Document, sels: &SelectionSet, down: bool) -> Option<EditResult> {
    let rope = doc.rope();
    let ranges = merged_line_ranges(rope, sels);
    let nlines = rope.len_lines();
    // Treat a trailing empty "line" after the final newline as not a real line.
    let last_real = if nlines > 0 && line_len_chars(rope, nlines - 1) == 0 && rope.len_chars() > 0 && rope.char(rope.len_chars() - 1) == '\n' {
        nlines - 1
    } else {
        nlines
    };
    let mut changes = Vec::new();
    // Selections are moved by hand rather than mapped through `tx`, because
    // mapping a replaced range cannot tell which part of it a position followed.
    let mut shift: Vec<(usize, usize, isize)> = Vec::new(); // (first_char, last_char, delta)
    for (a, b) in ranges {
        if down {
            if b + 1 >= last_real {
                continue;
            }
            let block_start = rope.line_to_char(a);
            let block_end = rope.line_to_char(b + 1);
            let next_end = if b + 2 < nlines { rope.line_to_char(b + 2) } else { rope.len_chars() };
            let block: String = rope.slice(block_start..block_end).into();
            let mut next: String = rope.slice(block_end..next_end).into();
            let mut block = block;
            // Ensure both chunks end with a terminator so the swap is clean.
            let term = doc.line_ending.as_str();
            if !next.ends_with('\n') && !next.ends_with('\r') {
                next.push_str(term);
                // block loses its final terminator so the file still ends without one
                while block.ends_with('\n') || block.ends_with('\r') {
                    block.pop();
                }
            }
            let next_len = next.chars().count();
            changes.push(Change::replace(block_start, next_end, format!("{next}{block}")));
            shift.push((block_start, block_end, next_len as isize));
        } else {
            if a == 0 {
                continue;
            }
            let prev_start = rope.line_to_char(a - 1);
            let block_start = rope.line_to_char(a);
            let block_end = if b + 1 < nlines { rope.line_to_char(b + 1) } else { rope.len_chars() };
            let prev: String = rope.slice(prev_start..block_start).into();
            let mut block: String = rope.slice(block_start..block_end).into();
            let prev_len = prev.chars().count();
            let mut prev = prev;
            let term = doc.line_ending.as_str();
            if !block.ends_with('\n') && !block.ends_with('\r') {
                block.push_str(term);
                while prev.ends_with('\n') || prev.ends_with('\r') {
                    prev.pop();
                }
            }
            changes.push(Change::replace(prev_start, block_end, format!("{block}{prev}")));
            shift.push((block_start, block_end, -(prev_len as isize)));
        }
    }
    if changes.is_empty() {
        return None;
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| {
        // A selection moves with the block that contains its start; selections
        // elsewhere stay put.
        let d = shift.iter().find(|(a, b, _)| s.start() >= *a && s.start() <= *b).map(|x| x.2).unwrap_or(0);
        let f = |p: usize| ((p as isize) + d).max(0) as usize;
        Selection::new(f(s.anchor), f(s.head))
    });
    // Guard against the hand-computed positions running past the new end.
    ns.clamp(&{
        let mut r = rope.clone();
        tx.apply(&mut r);
        r
    });
    EditResult::new(tx, ns, EditKind::Other)
}

/// Join each selected line range (or the caret line with the next) with a single space.
///
/// The next line's leading whitespace is removed. No space is inserted when
/// either side is blank.
pub fn join_lines(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    for (a, b) in merged_line_ranges(rope, sels) {
        let b = if a == b { a + 1 } else { b };
        for line in a..b {
            if line + 1 >= rope.len_lines() {
                break;
            }
            let end = line_end_char(rope, line);
            let next_start = rope.line_to_char(line + 1);
            let next_content = next_start + first_non_blank(rope.line(line + 1));
            let sep = if end == rope.line_to_char(line) || next_content == line_end_char(rope, line + 1) { "" } else { " " };
            changes.push(Change::replace(end, next_content, sep));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Swap the grapheme before the caret with the one after (`ab|` → `ba|`-style).
///
/// At the end of a line (or the document) the two graphemes before the caret
/// are swapped instead. Selections with an extent, and carets at index 0,
/// are left alone.
pub fn transpose_chars(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let text = doc.slice();
    let mut changes = Vec::new();
    let mut new_sels = Vec::new();
    for s in sels.iter() {
        if !s.is_empty() || s.head == 0 {
            new_sels.push(*s);
            continue;
        }
        let (a, b) = if s.head >= rope.len_chars() || matches!(rope.get_char(s.head), Some('\n' | '\r')) {
            let b = s.head;
            let a = prev_grapheme_boundary(text, b);
            (prev_grapheme_boundary(text, a), a)
        } else {
            (prev_grapheme_boundary(text, s.head), s.head)
        };
        let c = next_grapheme_boundary(text, b);
        let left: String = rope.slice(a..b).into();
        let right: String = rope.slice(b..c).into();
        changes.push(Change::replace(a, c, format!("{right}{left}")));
        new_sels.push(Selection::point(c));
    }
    let tx = Transaction::new(changes);
    let ns = SelectionSet::from_vec(new_sels.into_iter().map(|s| Selection::point(tx.map_pos(s.head, Assoc::Before))).collect(), sels.primary_index());
    EditResult::new(tx, ns, EditKind::Other)
}

/// Apply `f` to the lines in each selection (whole document if no extent).
///
/// `f` receives the lines without terminators and returns replacement
/// lines, which are joined with the first line's terminator. Lines are
/// whole-line ranges from `merged_line_ranges`. Without an extent the whole
/// document is used; when it ends with a terminator, the empty last line is
/// included in what `f` receives.
fn transform_line_block(doc: &Document, sels: &SelectionSet, mut f: impl FnMut(Vec<String>) -> Vec<String>) -> Option<EditResult> {
    let rope = doc.rope();
    let ranges = if sels.has_extent() {
        merged_line_ranges(rope, sels)
    } else {
        // Whole document. A final newline leaves an empty last "line" that is
        // not content: leave it out so sorting cannot move it to the top.
        let mut last = rope.len_lines().saturating_sub(1);
        if last > 0 && rope.line(last).len_chars() == 0 {
            last -= 1;
        }
        vec![(0, last)]
    };
    let mut changes = Vec::new();
    for (a, b) in ranges {
        let start = rope.line_to_char(a);
        let end = line_end_char(rope, b);
        let block: String = rope.slice(start..end).into();
        let term = apex_buffer::line_ending::terminator_of_line(rope, a).unwrap_or(doc.line_ending).as_str();
        let lines: Vec<String> = block.split('\n').map(|l| l.trim_end_matches('\r').to_owned()).collect();
        let out = f(lines).join(term);
        if out != block {
            changes.push(Change::replace(start, end, out));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Sort the line block (see `transform_line_block`) by `Ord` on strings, or
/// by lower-cased text when not `case_sensitive`; `reverse` reverses the
/// result. The sort is stable.
pub fn sort_lines(doc: &Document, sels: &SelectionSet, case_sensitive: bool, reverse: bool) -> Option<EditResult> {
    transform_line_block(doc, sels, |mut v| {
        if case_sensitive {
            v.sort();
        } else {
            v.sort_by_key(|a| a.to_lowercase());
        }
        if reverse {
            v.reverse();
        }
        v
    })
}

/// Remove repeated lines from the line block, keeping the first occurrence of
/// each.
pub fn unique_lines(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    transform_line_block(doc, sels, |v| {
        let mut seen = std::collections::HashSet::new();
        v.into_iter().filter(|l| seen.insert(l.clone())).collect()
    })
}

/// Reverse the order of the lines in the line block.
pub fn reverse_lines(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    transform_line_block(doc, sels, |mut v| {
        v.reverse();
        v
    })
}

/// Shuffle the line block with a Fisher–Yates pass driven by `seed`; the same
/// seed and input give the same order.
pub fn shuffle_lines(doc: &Document, sels: &SelectionSet, seed: u64) -> Option<EditResult> {
    transform_line_block(doc, sels, |mut v| {
        // Small xorshift so we need no RNG dependency.
        // xorshift64 must not start at zero, hence `| 1`. Modulo bias is
        // irrelevant here.
        let mut x = seed | 1;
        for i in (1..v.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.swap(i, (x % (i as u64 + 1)) as usize);
        }
        v
    })
}

/// Remove trailing whitespace on every line (or the selected lines).
///
/// Only spaces and tabs are trimmed; line terminators are left alone.
pub fn trim_trailing_whitespace(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    let ranges = if sels.has_extent() {
        merged_line_ranges(rope, sels)
    } else {
        // Whole document. A final newline leaves an empty last "line" that is
        // not content: leave it out so sorting cannot move it to the top.
        let mut last = rope.len_lines().saturating_sub(1);
        if last > 0 && rope.line(last).len_chars() == 0 {
            last -= 1;
        }
        vec![(0, last)]
    };
    for (a, b) in ranges {
        for line in a..=b {
            let end = line_end_char(rope, line);
            let l = rope.slice(rope.line_to_char(line)..end);
            let mut it = l.chars_at(l.len_chars());
            let mut trimmed = 0;
            while let Some(c) = it.prev() {
                if c == ' ' || c == '\t' {
                    trimmed += 1;
                } else {
                    break;
                }
            }
            if trimmed > 0 {
                changes.push(Change::delete(end - trimmed, end));
            }
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Make sure the document ends with exactly one line terminator.
///
/// Mechanically: appends the document's line ending when the last char is
/// not `\n` or `\r`. Extra trailing blank lines are not removed. Returns
/// `None` for an empty document or one that already ends with a terminator.
pub fn ensure_final_newline(doc: &Document, sels: &SelectionSet) -> Option<EditResult> {
    let rope = doc.rope();
    let n = rope.len_chars();
    if n == 0 || matches!(rope.char(n - 1), '\n' | '\r') {
        return None;
    }
    let tx = Transaction::single(Change::insert(n, doc.line_ending.as_str()));
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Convert leading indentation between tabs and spaces across the document.
///
/// Each line's leading whitespace is measured with `to.tab_width` and
/// rewritten with `to`'s style. The whole document is processed whatever
/// `sels` covers; `sels` is only mapped through the edit.
pub fn convert_indentation(doc: &Document, sels: &SelectionSet, to: IndentSettings) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    for line in 0..rope.len_lines() {
        let l = rope.line(line);
        let n = first_non_blank(l);
        if n == 0 {
            continue;
        }
        let ws: String = l.slice(..n).into();
        let cols = to.width_of(&ws);
        let replacement = to.whitespace_for(cols);
        if replacement != ws {
            let start = rope.line_to_char(line);
            changes.push(Change::replace(start, start + n, replacement));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.map(&tx, Assoc::Before);
    EditResult::new(tx, ns, EditKind::Other)
}

/// Case conversion applied by `change_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOp {
    /// `UPPER CASE`.
    Upper,
    /// `lower case`.
    Lower,
    /// First alphanumeric of each run upper-cased, the rest lower-cased.
    Title,
    /// Upper-case letters lowered and everything else upper-cased.
    Swap,
    /// `snake_case`, splitting on separators and lower-to-upper transitions.
    Snake,
    /// `camelCase`, using the same word split as `Snake`.
    Camel,
    /// `kebab-case`, using the same word split as `Snake`.
    Kebab,
}

/// Return `s` converted by `op`.
fn apply_case(s: &str, op: CaseOp) -> String {
    match op {
        CaseOp::Upper => s.to_uppercase(),
        CaseOp::Lower => s.to_lowercase(),
        CaseOp::Swap => s.chars().map(|c| if c.is_uppercase() { c.to_lowercase().collect::<String>() } else { c.to_uppercase().collect() }).collect(),
        CaseOp::Title => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    start = false;
                } else {
                    out.push(c);
                    start = true;
                }
            }
            out
        }
        CaseOp::Snake | CaseOp::Kebab | CaseOp::Camel => {
            let words = split_words(s);
            match op {
                CaseOp::Snake => words.join("_").to_lowercase(),
                CaseOp::Kebab => words.join("-").to_lowercase(),
                _ => words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let lw = w.to_lowercase();
                        if i == 0 { lw } else { apply_case(&lw, CaseOp::Title) }
                    })
                    .collect(),
            }
        }
    }
}

/// Split an identifier into words at `_`, `-`, whitespace, and where an
/// upper-case char follows a lower-case char or digit. Runs of capitals are
/// not split (`HTTPServer` stays one word).
fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == '-' || c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        } else {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase() || c.is_numeric();
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Change the case of each selection (or the word at each caret).
///
/// Selections keep their direction; a caret stays in place, clamped to the
/// end of the converted word. Carets not on a word are left alone.
pub fn change_case(doc: &Document, sels: &SelectionSet, op: CaseOp) -> Option<EditResult> {
    let rope = doc.rope();
    let text = doc.slice();
    let mut changes = Vec::new();
    let mut ranges = Vec::new();
    for s in sels.iter() {
        let (a, b) = if s.is_empty() { motion::word_at(text, s.head) } else { (s.start(), s.end()) };
        if a == b {
            continue;
        }
        let src: String = rope.slice(a..b).into();
        let out = apply_case(&src, op);
        ranges.push((a, b, out.chars().count(), s.is_reversed()));
        changes.push(Change::replace(a, b, out));
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    // `transform` visits selections in the same order as the loop above, so
    // `ranges[i]` lines up as long as the same carets are skipped.
    let mut i = 0;
    ns.transform(|s| {
        let r = if s.is_empty() { motion::word_at(text, s.head) } else { (s.start(), s.end()) };
        if r.0 == r.1 {
            return Selection::point(tx.map_pos(s.head, Assoc::Before));
        }
        let (a, _, n, rev) = ranges[i];
        i += 1;
        let a = tx.map_pos(a, Assoc::Before);
        if s.is_empty() {
            Selection::point(tx.map_pos(s.head, Assoc::Before).min(a + n))
        } else if rev {
            Selection::new(a + n, a)
        } else {
            Selection::new(a, a + n)
        }
    });
    EditResult::new(tx, ns, EditKind::Other)
}

/// Toggle a line comment prefix on the lines touched by every selection.
/// If every non-blank line is commented, uncomment; otherwise comment all.
///
/// `prefix` is the comment marker including any trailing space (for example
/// `// `). Matching ignores that trailing space so `//x` counts as
/// commented. Blank lines are not commented unless the block is entirely
/// blank.
pub fn toggle_line_comment(doc: &Document, sels: &SelectionSet, prefix: &str) -> Option<EditResult> {
    let rope = doc.rope();
    let prefix_trim = prefix.trim_end();
    let mut all_lines = Vec::new();
    for (a, b) in merged_line_ranges(rope, sels) {
        all_lines.extend(a..=b);
    }
    let is_commented = |line: usize| {
        let l = rope.line(line);
        let n = first_non_blank(l);
        let rest: String = l.slice(n..).chars().take(prefix_trim.chars().count()).collect();
        rest == prefix_trim
    };
    let non_blank: Vec<usize> = all_lines.iter().copied().filter(|&l| line_len_chars(rope, l) > first_non_blank(rope.line(l))).collect();
    let all_commented = !non_blank.is_empty() && non_blank.iter().all(|&l| is_commented(l));
    let mut changes = Vec::new();
    if all_commented {
        for &line in &non_blank {
            let start = rope.line_to_char(line);
            let n = first_non_blank(rope.line(line));
            let mut end = start + n + prefix_trim.chars().count();
            if rope.get_char(end) == Some(' ') && prefix.ends_with(' ') {
                end += 1;
            }
            changes.push(Change::delete(start + n, end));
        }
    } else {
        // Insert at the minimum indentation of the block so comments line up.
        let min_indent = non_blank.iter().map(|&l| first_non_blank(rope.line(l))).min().unwrap_or(0);
        let targets: Vec<usize> = if non_blank.is_empty() { all_lines.clone() } else { non_blank.clone() };
        for line in targets {
            let start = rope.line_to_char(line);
            changes.push(Change::insert(start + min_indent.min(line_len_chars(rope, line)), prefix));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| Selection::new(tx.map_pos(s.anchor, Assoc::After), tx.map_pos(s.head, Assoc::After)));
    EditResult::new(tx, ns, EditKind::Other)
}

/// Toggle block comments around each selection.
///
/// The range is the selection, or the caret line from its first non-blank
/// char. If the trimmed range is itself wrapped in `open`/`close`, the markers
/// (and one inner space each side) are removed; if `open`/`close` sit just
/// outside the range they are removed; otherwise `open ` and ` close` are
/// added.
pub fn toggle_block_comment(doc: &Document, sels: &SelectionSet, open: &str, close: &str) -> Option<EditResult> {
    let rope = doc.rope();
    let mut changes = Vec::new();
    for s in sels.iter() {
        let (a, b) = if s.is_empty() {
            let line = rope.char_to_line(s.head);
            (rope.line_to_char(line) + first_non_blank(rope.line(line)), line_end_char(rope, line))
        } else {
            (s.start(), s.end())
        };
        let text: String = rope.slice(a..b).into();
        let inner = text.trim();
        if inner.starts_with(open) && inner.ends_with(close) && inner.len() >= open.len() + close.len() {
            let lead = text.len() - text.trim_start().len();
            let trail = text.len() - text.trim_end().len();
            let a_chars = text[..lead].chars().count();
            let t_chars = text[text.len() - trail..].chars().count();
            let open_c = open.chars().count();
            let close_c = close.chars().count();
            let s0 = a + a_chars;
            let e0 = b - t_chars;
            let mut open_end = s0 + open_c;
            // Only eat the padding space if it does not overlap the close marker.
            if rope.get_char(open_end) == Some(' ') && e0 - close_c > open_end {
                open_end += 1;
            }
            let mut close_start = e0 - close_c;
            if close_start > open_end && rope.get_char(close_start - 1) == Some(' ') {
                close_start -= 1;
            }
            changes.push(Change::delete(s0, open_end));
            changes.push(Change::delete(close_start, e0));
        } else if let Some((o0, o1, c0, c1)) = surrounding_markers(rope, a, b, open, close) {
            // Selection is the inner text of an existing block comment.
            changes.push(Change::delete(o0, o1));
            changes.push(Change::delete(c0, c1));
        } else {
            changes.push(Change::insert(a, format!("{open} ")));
            changes.push(Change::insert(b, format!(" {close}")));
        }
    }
    let tx = Transaction::new(changes);
    let mut ns = sels.clone();
    ns.transform(|s| Selection::new(tx.map_pos(s.anchor, Assoc::After), tx.map_pos(s.head, Assoc::Before)));
    EditResult::new(tx, ns, EditKind::Other)
}

/// If `a..b` is directly wrapped by `open`/`close` (with optional single
/// spaces), return the char ranges of both markers.
///
/// Returns `(open_start, open_end, close_start, close_end)` as char indices;
/// the open range ends at `a` and the close range starts at `b`, so the
/// optional spaces are included in the removed ranges.
fn surrounding_markers(rope: &Rope, a: usize, b: usize, open: &str, close: &str) -> Option<(usize, usize, usize, usize)> {
    let oc = open.chars().count();
    let cc = close.chars().count();
    let mut o1 = a;
    if o1 > 0 && rope.get_char(o1 - 1) == Some(' ') {
        o1 -= 1;
    }
    if o1 < oc {
        return None;
    }
    let o0 = o1 - oc;
    if rope.slice(o0..o1) != open {
        return None;
    }
    let mut c0 = b;
    if rope.get_char(c0) == Some(' ') {
        c0 += 1;
    }
    let c1 = c0 + cc;
    if c1 > rope.len_chars() || rope.slice(c0..c1) != close {
        return None;
    }
    Some((o0, a, b, c1))
}

/// Replace each selection with `start + i*step` (number cursors like Sublime's
/// "Arithmetic" or TextPastry).
///
/// `i` is the selection's position in document order, starting at 0.
pub fn insert_sequence(doc: &Document, sels: &SelectionSet, start: i64, step: i64) -> Option<EditResult> {
    let mut i = 0i64;
    replace_each(doc, sels, EditKind::Other, |_| {
        let v = start + i * step;
        i += 1;
        v.to_string()
    })
}

/// Split every multi-line selection into one selection per line.
///
/// Each part covers the selection's text on that line, excluding the
/// terminator. A part that would start exactly at the selection's end on a
/// later line is dropped. Carets are kept. This changes only selections, not
/// the document.
pub fn split_into_lines(doc: &Document, sels: &SelectionSet) -> SelectionSet {
    let rope = doc.rope();
    let mut ns = sels.clone();
    ns.flat_transform(|s| {
        if s.is_empty() {
            return vec![s];
        }
        let (a, b) = (rope.char_to_line(s.start()), rope.char_to_line(s.end()));
        let mut out = Vec::new();
        for line in a..=b {
            let ls = rope.line_to_char(line).max(s.start());
            let le = line_end_char(rope, line).min(s.end());
            if ls <= le && (line == a || ls < s.end()) {
                out.push(Selection::new(ls, le));
            }
        }
        out
    });
    ns
}

/// Hard-wrap selected paragraphs (or the caret paragraph) at `width` columns.
///
/// Mechanically: works on the line block from `transform_line_block`, so
/// with no extent the whole document is wrapped. Paragraphs are runs of
/// non-blank lines; each is refilled using the first line's indentation.
/// `width` is in display columns with a floor of 10, and tabs count as 4.
/// Words longer than the width are kept whole.
pub fn hard_wrap(doc: &Document, sels: &SelectionSet, width: usize) -> Option<EditResult> {
    transform_line_block(doc, sels, |lines| {
        let mut out = Vec::new();
        let mut para: Vec<String> = Vec::new();
        let flush = |para: &mut Vec<String>, out: &mut Vec<String>| {
            if para.is_empty() {
                return;
            }
            let indent: String = para[0].chars().take_while(|c| c.is_whitespace()).collect();
            let words: Vec<&str> = para.iter().flat_map(|l| l.split_whitespace()).collect();
            let mut cur = indent.clone();
            let mut cur_w = apex_buffer::display_width_of(&cur, 4);
            for w in words {
                let ww = apex_buffer::display_width_of(w, 4);
                if cur.trim().is_empty() {
                    cur.push_str(w);
                    cur_w += ww;
                } else if cur_w + 1 + ww > width.max(10) {
                    out.push(std::mem::replace(&mut cur, indent.clone()));
                    cur.push_str(w);
                    cur_w = apex_buffer::display_width_of(&indent, 4) + ww;
                } else {
                    cur.push(' ');
                    cur.push_str(w);
                    cur_w += 1 + ww;
                }
            }
            out.push(cur);
            para.clear();
        };
        for l in lines {
            if l.trim().is_empty() {
                flush(&mut para, &mut out);
                out.push(l);
            } else {
                para.push(l);
            }
        }
        flush(&mut para, &mut out);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a test document from `s`.
    fn doc(s: &str) -> Document {
        Document::from_text(s)
    }

    /// Run `f` on `d`, apply its result and return the new selections. Panics if
    /// `f` produced no edit.
    fn run(d: &mut Document, f: impl FnOnce(&Document) -> Option<EditResult>) -> SelectionSet {
        let r = f(d).expect("edit produced nothing");
        let before = SelectionSet::default();
        d.apply(r.tx, &before, &r.sels, r.kind);
        r.sels
    }

    /// Pins down post-edit caret positions and primary index for multi-caret insert.
    #[test]
    fn multi_cursor_insert_keeps_offsets_straight() {
        let mut d = doc("a\nb\nc");
        let sels = SelectionSet::from_vec(vec![Selection::point(0), Selection::point(2), Selection::point(4)], 2);
        let s = run(&mut d, |d| insert_text(d, &sels, "xy", InsertOptions::default()));
        assert_eq!(d.text(), "xya\nxyb\nxyc");
        assert_eq!(s.as_slice(), &[Selection::point(2), Selection::point(6), Selection::point(10)]);
        assert_eq!(s.primary_index(), 2);
    }

    /// Pins down bracket auto-pairing, stepping over the closer, and pair-aware
    /// backspace.
    #[test]
    fn auto_pair_and_step_over() {
        let mut d = doc("");
        let sels = SelectionSet::default();
        let s = run(&mut d, |d| insert_text(d, &sels, "(", InsertOptions::default()));
        assert_eq!(d.text(), "()");
        assert_eq!(s.primary(), Selection::point(1));
        let r = insert_text(&d, &s, ")", InsertOptions::default()).unwrap();
        assert!(r.tx.is_empty());
        assert_eq!(r.sels.primary(), Selection::point(2));
        // Backspace inside an empty pair removes both.
        let s = SelectionSet::single(Selection::point(1));
        run(&mut d, |d| delete_backward(d, &s, IndentSettings::default(), true));
        assert_eq!(d.text(), "");
    }

    /// Pins down that typing a quote over a selection wraps it and keeps the
    /// inner text selected.
    #[test]
    fn wrap_selection_with_quotes() {
        let mut d = doc("abc");
        let sels = SelectionSet::single(Selection::new(0, 3));
        let s = run(&mut d, |d| insert_text(d, &sels, "\"", InsertOptions::default()));
        assert_eq!(d.text(), "\"abc\"");
        assert_eq!(s.primary(), Selection::new(1, 4));
    }

    /// Pins down base indent, extra level after `{`, and closer on its own line.
    #[test]
    fn newline_auto_indents_and_expands_braces() {
        let mut d = doc("  foo {}");
        let sels = SelectionSet::single(Selection::point(7));
        let s = run(&mut d, |d| newline(d, &sels, IndentSettings::default(), true));
        assert_eq!(d.text(), "  foo {\n      \n  }");
        assert_eq!(s.primary(), Selection::point(14));
    }

    /// Pins down backspace to the previous indent stop, including partial levels.
    #[test]
    fn smart_backspace_removes_indent_level() {
        let mut d = doc("      x");
        let sels = SelectionSet::single(Selection::point(6));
        run(&mut d, |d| delete_backward(d, &sels, IndentSettings::default(), true));
        assert_eq!(d.text(), "    x");
        let sels = SelectionSet::single(Selection::point(4));
        run(&mut d, |d| delete_backward(d, &sels, IndentSettings::default(), true));
        assert_eq!(d.text(), "x");
    }

    /// Pins down that backspace removes a whole grapheme (base plus combining mark).
    #[test]
    fn delete_backward_grapheme() {
        let mut d = doc("ae\u{301}");
        let sels = SelectionSet::single(Selection::point(3));
        run(&mut d, |d| delete_backward(d, &sels, IndentSettings::default(), true));
        assert_eq!(d.text(), "a");
    }

    /// Pins down block indent skipping blank lines, selection mapping, and that
    /// outdent reverses it.
    #[test]
    fn indent_outdent_round_trip() {
        let mut d = doc("a\nb\n\nc");
        let sels = SelectionSet::single(Selection::new(0, 6));
        let s = run(&mut d, |d| indent_lines(d, &sels, IndentSettings::default()));
        assert_eq!(d.text(), "    a\n    b\n\n    c");
        assert_eq!(s.primary(), Selection::new(4, 18));
        let s = run(&mut d, |d| outdent_lines(d, &s, IndentSettings::default()));
        assert_eq!(d.text(), "a\nb\n\nc");
        assert_eq!(s.primary(), Selection::new(0, 6));
    }

    /// Pins down line duplication, caret placement, and deleting the last line
    /// with its preceding terminator.
    #[test]
    fn duplicate_and_delete_lines() {
        let mut d = doc("one\ntwo\nthree");
        let sels = SelectionSet::single(Selection::point(5));
        let s = run(&mut d, |d| duplicate(d, &sels));
        assert_eq!(d.text(), "one\ntwo\ntwo\nthree");
        assert_eq!(s.primary(), Selection::point(9));
        run(&mut d, |d| delete_lines(d, &s));
        assert_eq!(d.text(), "one\ntwo\nthree");
        let s = SelectionSet::single(Selection::point(12));
        run(&mut d, |d| delete_lines(d, &s));
        assert_eq!(d.text(), "one\ntwo");
    }

    /// Pins down moving a line past the end without a trailing newline, the
    /// bottom limit, and moving back up.
    #[test]
    fn move_lines_up_and_down() {
        let mut d = doc("a\nb\nc");
        let sels = SelectionSet::single(Selection::point(0));
        let s = run(&mut d, |d| move_lines(d, &sels, true));
        assert_eq!(d.text(), "b\na\nc");
        assert_eq!(s.primary(), Selection::point(2));
        let s = run(&mut d, |d| move_lines(d, &s, true));
        assert_eq!(d.text(), "b\nc\na");
        assert_eq!(s.primary(), Selection::point(4));
        assert!(move_lines(&d, &s, true).is_none());
        let s = run(&mut d, |d| move_lines(d, &s, false));
        assert_eq!(d.text(), "b\na\nc");
        assert_eq!(s.primary(), Selection::point(2));
    }

    /// Pins down join (indent stripped), sort, de-duplication and reversal.
    #[test]
    fn join_sort_unique_reverse() {
        let mut d = doc("b\n  a\nb");
        let sels = SelectionSet::single(Selection::point(0));
        run(&mut d, |d| join_lines(d, &sels));
        assert_eq!(d.text(), "b a\nb");
        let mut d = doc("b\na\nb\nc");
        let all = SelectionSet::single(Selection::point(0));
        run(&mut d, |d| sort_lines(d, &all, true, false));
        assert_eq!(d.text(), "a\nb\nb\nc");
        run(&mut d, |d| unique_lines(d, &all));
        assert_eq!(d.text(), "a\nb\nc");
        run(&mut d, |d| reverse_lines(d, &all));
        assert_eq!(d.text(), "c\nb\na");
    }

    /// A whole-document sort keeps the final newline at the end instead of
    /// sorting the empty last line to the top.
    #[test]
    fn sort_keeps_trailing_newline() {
        let mut d = doc("b\na\n");
        let all = SelectionSet::single(Selection::point(0));
        run(&mut d, |d| sort_lines(d, &all, true, false));
        assert_eq!(d.text(), "a\nb\n");
    }

    /// Pins down line comments aligned at minimum indent (blank lines skipped)
    /// and block comment add/remove round trips.
    #[test]
    fn comments_toggle() {
        let mut d = doc("  a\n\n  b");
        let sels = SelectionSet::single(Selection::new(0, 8));
        let s = run(&mut d, |d| toggle_line_comment(d, &sels, "// "));
        assert_eq!(d.text(), "  // a\n\n  // b");
        run(&mut d, |d| toggle_line_comment(d, &s, "// "));
        assert_eq!(d.text(), "  a\n\n  b");
        let mut d = doc("x = 1");
        let sels = SelectionSet::single(Selection::new(0, 5));
        let s = run(&mut d, |d| toggle_block_comment(d, &sels, "/*", "*/"));
        assert_eq!(d.text(), "/* x = 1 */");
        run(&mut d, |d| toggle_block_comment(d, &s, "/*", "*/"));
        assert_eq!(d.text(), "x = 1");
    }

    /// Pins down title, swap, snake, camel and kebab conversions.
    #[test]
    fn case_changes() {
        let mut d = doc("hello world");
        let sels = SelectionSet::single(Selection::new(0, 11));
        run(&mut d, |d| change_case(d, &sels, CaseOp::Title));
        assert_eq!(d.text(), "Hello World");
        run(&mut d, |d| change_case(d, &sels, CaseOp::Swap));
        assert_eq!(d.text(), "hELLO wORLD");
        assert_eq!(apply_case("someVarName", CaseOp::Snake), "some_var_name");
        assert_eq!(apply_case("some_var_name", CaseOp::Camel), "someVarName");
        assert_eq!(apply_case("SomeVar", CaseOp::Kebab), "some-var");
    }

    /// Pins down grapheme transposition and numbered multi-caret insertion.
    #[test]
    fn transpose_and_sequence() {
        let mut d = doc("ab");
        let sels = SelectionSet::single(Selection::point(1));
        let s = run(&mut d, |d| transpose_chars(d, &sels));
        assert_eq!(d.text(), "ba");
        assert_eq!(s.primary(), Selection::point(2));
        let mut d = doc("x\nx\nx");
        let sels = SelectionSet::from_vec(vec![Selection::new(0, 1), Selection::new(2, 3), Selection::new(4, 5)], 0);
        run(&mut d, |d| insert_sequence(d, &sels, 1, 1));
        assert_eq!(d.text(), "1\n2\n3");
    }

    /// Pins down one clipboard line per caret when the counts match.
    #[test]
    fn paste_distributes_lines_across_cursors() {
        let mut d = doc("\n\n");
        let sels = SelectionSet::from_vec(vec![Selection::point(0), Selection::point(1), Selection::point(2)], 0);
        run(&mut d, |d| paste(d, &sels, "a\nb\nc"));
        assert_eq!(d.text(), "a\nb\nc");
    }

    /// Pins down trailing-whitespace removal and that a final newline is added once.
    #[test]
    fn trim_and_final_newline() {
        let mut d = doc("a  \nb\t");
        let s = SelectionSet::default();
        run(&mut d, |d| trim_trailing_whitespace(d, &s));
        assert_eq!(d.text(), "a\nb");
        run(&mut d, |d| ensure_final_newline(d, &s));
        assert_eq!(d.text(), "a\nb\n");
        assert!(ensure_final_newline(&d, &s).is_none());
    }

    /// Pins down per-line selection splitting and paragraph refill at a width.
    #[test]
    fn split_lines_and_hard_wrap() {
        let d = doc("ab\ncd\nef");
        let s = split_into_lines(&d, &SelectionSet::single(Selection::new(1, 7)));
        assert_eq!(s.as_slice(), &[Selection::new(1, 2), Selection::new(3, 5), Selection::new(6, 7)]);
        let mut d = doc("one two three four five six");
        run(&mut d, |d| hard_wrap(d, &SelectionSet::default(), 12));
        assert_eq!(d.text(), "one two\nthree four\nfive six");
    }

    /// Measures (and bounds at 200 ms) a 1,000-caret insert, checking offsets.
    #[test]
    fn thousand_cursors_insert() {
        let text = "x\n".repeat(1000);
        let mut d = doc(&text);
        let sels = SelectionSet::from_vec((0..1000).map(|i| Selection::point(i * 2)).collect(), 0);
        let t = std::time::Instant::now();
        let s = run(&mut d, |d| insert_text(d, &sels, "ab", InsertOptions::default()));
        assert!(t.elapsed().as_millis() < 200, "took {:?}", t.elapsed());
        assert_eq!(s.len(), 1000);
        assert!(d.text().starts_with("abx\nabx\n"));
        assert_eq!(s.as_slice()[999], Selection::point(999 * 4 + 2));
    }
}
