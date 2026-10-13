//! Conversions between the offset spaces described in the crate docs.
//!
//! Everything here is a pure function over a [`Rope`] or [`RopeSlice`]; nothing
//! is cached, so each call is `O(log n)` for the rope lookups plus `O(line
//! length)` for the display-column walks. Callers are the editing layer
//! (`apex-edit`) for caret arithmetic, the LSP bridge for UTF-16 translation
//! and the UI for display columns.
//!
//! Invariants:
//! * every public function clamps its input instead of panicking, because
//!   positions handed in may be stale after an edit;
//! * `line` is zero-based everywhere and `col` is a **char** index within the
//!   line unless the name says `display_col`, `byte` or `utf16`;
//! * a line's "length" never includes its terminator (`\n`, `\r\n` or `\r`).

use ropey::{Rope, RopeSlice};
use unicode_width::UnicodeWidthChar;

/// A line/column position where `col` is a **char** index within the line.
///
/// Lines are zero-based. `col` counts Unicode scalar values from the start of
/// the line, *not* bytes and *not* display cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    /// Zero-based line index.
    pub line: usize,
    /// Char index from the start of the line (may be past the end; conversions clamp).
    pub col: usize,
}

impl Position {
    /// Construct a position without validation; see [`Position::to_char`] for clamping.
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }

    /// Convert to a char offset into `rope`, clamping to valid ranges.
    ///
    /// A `line` past the end maps onto the last line; a `col` past the end of
    /// the line lands just before that line's terminator.
    pub fn to_char(self, rope: &Rope) -> usize {
        let line = self.line.min(rope.len_lines().saturating_sub(1));
        let start = rope.line_to_char(line);
        let len = line_len_chars(rope, line);
        start + self.col.min(len)
    }

    /// Build from a char offset, clamping to the end of the rope.
    pub fn from_char(rope: &Rope, char_idx: usize) -> Self {
        let char_idx = char_idx.min(rope.len_chars());
        let line = rope.char_to_line(char_idx);
        Self { line, col: char_idx - rope.line_to_char(line) }
    }
}

/// Number of chars on `line` excluding its line terminator.
///
/// Returns 0 for a line index past the end rather than panicking.
pub fn line_len_chars(rope: &Rope, line: usize) -> usize {
    if line >= rope.len_lines() {
        return 0;
    }
    let slice = rope.line(line);
    let n = slice.len_chars();
    strip_trailing_newline_len(slice, n)
}

/// Given a line slice of `n` chars, return `n` minus the length of any
/// trailing terminator (`\r\n` counts as two chars, `\n` or `\r` as one).
fn strip_trailing_newline_len(slice: RopeSlice<'_>, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    match slice.char(n - 1) {
        '\n' => {
            // CRLF: the `\r` belongs to the terminator too.
            if n >= 2 && slice.char(n - 2) == '\r' { n - 2 } else { n - 1 }
        }
        '\r' => n - 1,
        _ => n,
    }
}

/// Char offset of the end of `line`, before its terminator.
pub fn line_end_char(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line) + line_len_chars(rope, line)
}

/// Byte offset → char offset (clamped).
pub fn byte_to_char(rope: &Rope, byte: usize) -> usize {
    rope.byte_to_char(byte.min(rope.len_bytes()))
}

/// Char offset → byte offset (clamped).
pub fn char_to_byte(rope: &Rope, ch: usize) -> usize {
    rope.char_to_byte(ch.min(rope.len_chars()))
}

/// Convert a char offset to a (line, UTF-16 code unit column) pair as used by LSP.
///
/// The column is measured from the start of the line, so astral-plane
/// characters earlier on the same line count as two units each.
pub fn char_to_utf16_position(rope: &Rope, char_idx: usize) -> (usize, usize) {
    let char_idx = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(char_idx);
    let line_start = rope.line_to_char(line);
    let col = rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start);
    (line, col)
}

/// Convert an LSP (line, UTF-16 column) pair to a char offset, clamping both
/// coordinates so that malformed server responses can never index out of bounds.
///
/// A column past the end of the line clamps to just before the terminator.
pub fn utf16_position_to_char(rope: &Rope, line: usize, utf16_col: usize) -> usize {
    let line = line.min(rope.len_lines().saturating_sub(1));
    let start = rope.line_to_char(line);
    let end = line_end_char(rope, line);
    let start_cu = rope.char_to_utf16_cu(start);
    let end_cu = rope.char_to_utf16_cu(end);
    // Clamp in UTF-16 space before converting back, so an over-long column
    // cannot run into the next line.
    let target = (start_cu + utf16_col).min(end_cu);
    rope.utf16_cu_to_char(target)
}

/// Display width of a single char at the given display column.
///
/// Tabs expand to the next multiple of `tab_width`. Control characters and
/// zero-width/combining marks are 0 wide; East Asian wide characters are 2.
#[inline]
pub fn char_display_width(c: char, col: usize, tab_width: usize) -> usize {
    if c == '\t' {
        // Guard against a zero tab width from a bad config: treat it as 1.
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

/// Display width of a string starting at column zero.
pub fn display_width_of(text: &str, tab_width: usize) -> usize {
    let mut col = 0;
    for c in text.chars() {
        col += char_display_width(c, col, tab_width);
    }
    col
}

/// Display column reached after `char_col` chars of `line`.
///
/// `line` is a whole line slice (terminator included or not; it is never
/// reached if `char_col` is within the text). `O(char_col)`.
pub fn char_col_to_display_col(line: RopeSlice<'_>, char_col: usize, tab_width: usize) -> usize {
    let mut col = 0;
    for c in line.chars().take(char_col) {
        col += char_display_width(c, col, tab_width);
    }
    col
}

/// Char column of the cell containing `display_col` (floor semantics).
///
/// Used by column selection and vertical caret motion so that moving up and
/// down keeps the caret visually aligned across tabs and wide characters.
/// Returns the line length if `display_col` is past the end of the line.
pub fn display_col_to_char_col(line: RopeSlice<'_>, display_col: usize, tab_width: usize) -> usize {
    let mut col = 0;
    // Never return a column inside the terminator; the caret must stay on the line.
    let n = strip_trailing_newline_len(line, line.len_chars());
    for (i, c) in line.chars().take(n).enumerate() {
        let w = char_display_width(c, col, tab_width);
        // Floor semantics: a display column in the middle of a tab or a wide
        // char belongs to that char.
        if col + w > display_col {
            return i;
        }
        col += w;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Line lengths must exclude LF, CRLF and the empty trailing line.
    #[test]
    fn line_len_excludes_terminators() {
        let r = Rope::from_str("ab\r\ncd\nef");
        assert_eq!(line_len_chars(&r, 0), 2);
        assert_eq!(line_len_chars(&r, 1), 2);
        assert_eq!(line_len_chars(&r, 2), 2);
        assert_eq!(line_len_chars(&r, 3), 0);
    }

    /// `from_char` and `to_char` are inverses for every valid offset, and
    /// out-of-range positions clamp to the end.
    #[test]
    fn position_round_trips() {
        let r = Rope::from_str("héllo\nwörld\n");
        for i in 0..=r.len_chars() {
            assert_eq!(Position::from_char(&r, i).to_char(&r), i);
        }
        assert_eq!(Position::new(99, 99).to_char(&r), r.len_chars());
    }

    /// Astral-plane chars occupy two UTF-16 units; bad LSP coordinates clamp.
    #[test]
    fn utf16_mapping_handles_astral_plane() {
        let r = Rope::from_str("a😀b\nc");
        // '😀' is two UTF-16 code units.
        assert_eq!(char_to_utf16_position(&r, 2), (0, 3));
        assert_eq!(utf16_position_to_char(&r, 0, 3), 2);
        assert_eq!(utf16_position_to_char(&r, 0, 999), 3);
        assert_eq!(utf16_position_to_char(&r, 99, 0), (r.len_chars() - 1));
    }

    /// Tabs snap to the next stop and CJK chars are two cells wide, in both directions.
    #[test]
    fn display_columns_expand_tabs_and_wide_chars() {
        let r = Rope::from_str("\ta\t漢x");
        let l = r.line(0);
        assert_eq!(char_col_to_display_col(l, 1, 4), 4);
        assert_eq!(char_col_to_display_col(l, 2, 4), 5);
        assert_eq!(char_col_to_display_col(l, 3, 4), 8);
        assert_eq!(char_col_to_display_col(l, 4, 4), 10);
        assert_eq!(display_col_to_char_col(l, 0, 4), 0);
        assert_eq!(display_col_to_char_col(l, 3, 4), 0);
        assert_eq!(display_col_to_char_col(l, 4, 4), 1);
        assert_eq!(display_col_to_char_col(l, 9, 4), 3);
        assert_eq!(display_col_to_char_col(l, 10, 4), 4);
        assert_eq!(display_col_to_char_col(l, 50, 4), 5);
    }
}
