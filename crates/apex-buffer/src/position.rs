//! Conversions between the offset spaces described in the crate docs.

use ropey::{Rope, RopeSlice};
use unicode_width::UnicodeWidthChar;

/// A line/column position where `col` is a **char** index within the line.
///
/// Lines are zero-based. `col` counts Unicode scalar values from the start of
/// the line, *not* bytes and *not* display cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }

    /// Convert to a char offset into `rope`, clamping to valid ranges.
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
pub fn line_len_chars(rope: &Rope, line: usize) -> usize {
    if line >= rope.len_lines() {
        return 0;
    }
    let slice = rope.line(line);
    let n = slice.len_chars();
    strip_trailing_newline_len(slice, n)
}

fn strip_trailing_newline_len(slice: RopeSlice<'_>, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    match slice.char(n - 1) {
        '\n' => {
            if n >= 2 && slice.char(n - 2) == '\r' {
                n - 2
            } else {
                n - 1
            }
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
pub fn char_to_utf16_position(rope: &Rope, char_idx: usize) -> (usize, usize) {
    let char_idx = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(char_idx);
    let line_start = rope.line_to_char(line);
    let col = rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start);
    (line, col)
}

/// Convert an LSP (line, UTF-16 column) pair to a char offset, clamping both
/// coordinates so that malformed server responses can never index out of bounds.
pub fn utf16_position_to_char(rope: &Rope, line: usize, utf16_col: usize) -> usize {
    let line = line.min(rope.len_lines().saturating_sub(1));
    let start = rope.line_to_char(line);
    let end = line_end_char(rope, line);
    let start_cu = rope.char_to_utf16_cu(start);
    let end_cu = rope.char_to_utf16_cu(end);
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
    let n = strip_trailing_newline_len(line, line.len_chars());
    for (i, c) in line.chars().take(n).enumerate() {
        let w = char_display_width(c, col, tab_width);
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

    #[test]
    fn line_len_excludes_terminators() {
        let r = Rope::from_str("ab\r\ncd\nef");
        assert_eq!(line_len_chars(&r, 0), 2);
        assert_eq!(line_len_chars(&r, 1), 2);
        assert_eq!(line_len_chars(&r, 2), 2);
        assert_eq!(line_len_chars(&r, 3), 0);
    }

    #[test]
    fn position_round_trips() {
        let r = Rope::from_str("héllo\nwörld\n");
        for i in 0..=r.len_chars() {
            assert_eq!(Position::from_char(&r, i).to_char(&r), i);
        }
        assert_eq!(Position::new(99, 99).to_char(&r), r.len_chars());
    }

    #[test]
    fn utf16_mapping_handles_astral_plane() {
        let r = Rope::from_str("a😀b\nc");
        // '😀' is two UTF-16 code units.
        assert_eq!(char_to_utf16_position(&r, 2), (0, 3));
        assert_eq!(utf16_position_to_char(&r, 0, 3), 2);
        assert_eq!(utf16_position_to_char(&r, 0, 999), 3);
        assert_eq!(utf16_position_to_char(&r, 99, 0), (r.len_chars() - 1));
    }

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
