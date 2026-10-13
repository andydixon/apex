//! Grapheme- and word-aware caret motion over a [`RopeSlice`].
//!
//! All offsets are **char** offsets. Every function clamps its input and never
//! panics on out-of-range values, because cursor positions may be stale after
//! a concurrent edit and the caller must still get a valid answer.
//!
//! Two families live here:
//! * grapheme boundaries, driven by `unicode_segmentation`'s incremental
//!   [`GraphemeCursor`] so that a rope split across chunks is handled without
//!   copying the text out;
//! * word motion, based on a four-way [`CharClass`] split (word / punctuation /
//!   whitespace / newline) in the style of Sublime Text and Vim.
//!
//! Callers are the selection and editing operations in `apex-edit` (arrow
//! keys, Ctrl+arrows, backspace, double-click word selection).

use ropey::RopeSlice;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// Char offset of the next extended grapheme cluster boundary after `char_idx`.
///
/// Returns `text.len_chars()` when there is no later boundary. Chunk
/// boundaries in the rope are crossed transparently; if the segmenter reports
/// an unexpected error the function degrades to advancing by one char.
pub fn next_grapheme_boundary(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let char_idx = char_idx.min(text.len_chars());
    let byte_idx = text.char_to_byte(char_idx);
    // GraphemeCursor works in bytes over &str chunks; track which chunk we are
    // in and its byte/char start so results can be mapped back to char space.
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = text.chunk_at_byte(byte_idx);
    let mut gc = GraphemeCursor::new(byte_idx, text.len_bytes(), true);
    loop {
        match gc.next_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return text.len_chars(),
            Ok(Some(n)) => {
                let tmp = byte_to_char_in_chunk(chunk, n - chunk_byte_idx);
                return chunk_char_idx + tmp;
            }
            Err(GraphemeIncomplete::NextChunk) => {
                // The cluster continues into the following chunk; feed it in.
                chunk_byte_idx += chunk.len();
                let (a, _, c, _) = text.chunk_at_byte(chunk_byte_idx);
                chunk = a;
                chunk_char_idx = c;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                // The segmenter needs to see bytes before `n` to classify
                // (e.g. regional indicator pairs); hand it the chunk ending there.
                let (ctx_chunk, ctx_byte_start, _, _) = text.chunk_at_byte(n - 1);
                gc.provide_context(ctx_chunk, ctx_byte_start);
            }
            Err(_) => return (char_idx + 1).min(text.len_chars()),
        }
    }
}

/// Char offset of the previous extended grapheme cluster boundary before `char_idx`.
///
/// Returns 0 at or before the start. Mirrors [`next_grapheme_boundary`],
/// walking chunks backwards; on an unexpected segmenter error it degrades to
/// stepping back one char.
pub fn prev_grapheme_boundary(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let char_idx = char_idx.min(text.len_chars());
    if char_idx == 0 {
        return 0;
    }
    let byte_idx = text.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = text.chunk_at_byte(byte_idx);
    let mut gc = GraphemeCursor::new(byte_idx, text.len_bytes(), true);
    loop {
        match gc.prev_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return 0,
            Ok(Some(n)) => {
                let tmp = byte_to_char_in_chunk(chunk, n - chunk_byte_idx);
                return chunk_char_idx + tmp;
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                // Step to the chunk that ends just before this one starts.
                let (a, b, c, _) = text.chunk_at_byte(chunk_byte_idx - 1);
                chunk = a;
                chunk_byte_idx = b;
                chunk_char_idx = c;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx_chunk, ctx_byte_start, _, _) = text.chunk_at_byte(n - 1);
                gc.provide_context(ctx_chunk, ctx_byte_start);
            }
            Err(_) => return char_idx - 1,
        }
    }
}

/// Number of chars in the first `byte` bytes of `chunk` (clamped to the chunk).
#[inline]
fn byte_to_char_in_chunk(chunk: &str, byte: usize) -> usize {
    chunk[..byte.min(chunk.len())].chars().count()
}

/// Whether `char_idx` lies on a grapheme boundary.
///
/// The ends of the text always count as boundaries. Internally asks for the
/// previous boundary from one char further on and checks it lands here.
pub fn is_grapheme_boundary(text: RopeSlice<'_>, char_idx: usize) -> bool {
    char_idx == 0 || char_idx >= text.len_chars() || prev_grapheme_boundary(text, char_idx + 1) == char_idx
}

/// Snap `char_idx` to the nearest grapheme boundary at or before it.
///
/// Used after edits so a caret never ends up inside a combining sequence.
pub fn snap_to_grapheme(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let c = char_idx.min(text.len_chars());
    if is_grapheme_boundary(text, c) { c } else { prev_grapheme_boundary(text, c) }
}

/// Character class used for word motion. Runs of the same class form a "word".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    /// Spaces, tabs and other Unicode whitespace, excluding line breaks.
    Whitespace,
    /// Alphanumerics and `_` (identifier characters).
    Word,
    /// Anything else that is not whitespace.
    Punctuation,
    /// `\n` or `\r`; kept separate so motions stop at line boundaries.
    Newline,
}

/// Classify a char for word motion. `_` is a word char so identifiers stay whole.
#[inline]
pub fn char_class(c: char) -> CharClass {
    if c == '\n' || c == '\r' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Char offset after the end of the word at or after `char_idx` (Ctrl+Right).
///
/// Skips leading whitespace, then one run of a single class. Newlines are
/// their own class so the caret stops at line boundaries like Sublime does.
pub fn next_word_end(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let len = text.len_chars();
    let mut i = char_idx.min(len);
    let mut chars = text.chars_at(i);
    // Skip whitespace (not newlines).
    let mut first = None;
    for c in chars.by_ref() {
        if char_class(c) == CharClass::Whitespace {
            i += 1;
        } else {
            first = Some(c);
            break;
        }
    }
    let Some(first) = first else { return len };
    let class = char_class(first);
    if class == CharClass::Newline {
        // Consume a single line break (treat CRLF as one unit).
        i += 1;
        if first == '\r' && i < len && text.char(i) == '\n' {
            i += 1;
        }
        return i;
    }
    // `first` has already been consumed from the iterator; count it, then
    // extend over the rest of the run.
    i += 1;
    for c in chars {
        if char_class(c) == class {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Char offset of the start of the word at or before `char_idx` (Ctrl+Left).
///
/// The mirror of [`next_word_end`]: walks back over whitespace, then back over
/// one run of a single class. A line break is consumed as one unit (CRLF
/// included) so the caret lands at the end of the previous line.
pub fn prev_word_start(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let mut i = char_idx.min(text.len_chars());
    if i == 0 {
        return 0;
    }
    let mut chars = text.chars_at(i);
    // Walk backwards over whitespace.
    let mut last = None;
    while let Some(c) = chars.prev() {
        if char_class(c) == CharClass::Whitespace {
            i -= 1;
        } else {
            last = Some(c);
            break;
        }
    }
    let Some(last) = last else { return 0 };
    let class = char_class(last);
    // `last` has been consumed; account for it before scanning its run.
    i -= 1;
    if class == CharClass::Newline {
        // Treat `\r\n` as one unit when stepping back over it.
        if last == '\n' && i > 0 && text.char(i - 1) == '\r' {
            i -= 1;
        }
        return i;
    }
    while let Some(c) = chars.prev() {
        if char_class(c) == class {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

/// Char offset of the start of the next word (Vim `w` semantics: stops at the
/// start of the following run, skipping whitespace and newlines).
///
/// Unlike [`next_word_end`] this crosses line breaks, so it is suited to
/// "jump to next word" rather than Ctrl+Right.
pub fn next_word_start(text: RopeSlice<'_>, char_idx: usize) -> usize {
    let len = text.len_chars();
    let mut i = char_idx.min(len);
    if i >= len {
        return len;
    }
    let class = char_class(text.char(i));
    let mut chars = text.chars_at(i);
    // If we start inside a word or punctuation run, first leave that run.
    if class != CharClass::Whitespace && class != CharClass::Newline {
        for c in chars.by_ref() {
            if char_class(c) == class {
                i += 1;
            } else {
                break;
            }
        }
        // The loop above consumed one char past the run; restart from `i`.
        chars = text.chars_at(i);
    }
    // Then skip any whitespace and line breaks to the next run's first char.
    for c in chars {
        if matches!(char_class(c), CharClass::Whitespace | CharClass::Newline) {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Range (char offsets) of the word containing or adjacent to `char_idx`.
///
/// Prefers the word to the right of the caret, then the word to the left,
/// mirroring the behaviour of double-click and "expand selection to word".
/// Returns an empty range `(i, i)` when neither neighbour is a word or
/// punctuation char (e.g. the caret sits between two spaces).
pub fn word_at(text: RopeSlice<'_>, char_idx: usize) -> (usize, usize) {
    let len = text.len_chars();
    let i = char_idx.min(len);
    let right = if i < len { Some(char_class(text.char(i))) } else { None };
    let left = if i > 0 { Some(char_class(text.char(i - 1))) } else { None };
    // Word chars beat punctuation on either side, so `foo|.bar` picks `foo`
    // rather than `.`.
    let class = match (right, left) {
        (Some(CharClass::Word), _) => CharClass::Word,
        (_, Some(CharClass::Word)) => CharClass::Word,
        (Some(CharClass::Punctuation), _) => CharClass::Punctuation,
        (_, Some(CharClass::Punctuation)) => CharClass::Punctuation,
        _ => return (i, i),
    };
    let mut start = i;
    let mut chars = text.chars_at(i);
    while let Some(c) = chars.prev() {
        if char_class(c) == class {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = i;
    for c in text.chars_at(i) {
        if char_class(c) == class {
            end += 1;
        } else {
            break;
        }
    }
    (start, end)
}

/// Char offset (relative to the start of `line`) of the first char that is not
/// a space or tab; equals the line length if the line is all blanks.
///
/// Only ASCII blanks count, matching what indentation is made of.
pub fn first_non_blank(line: RopeSlice<'_>) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    /// Combining marks, ZWJ sequences and flag pairs each move as one unit.
    #[test]
    fn graphemes_treat_combining_marks_and_emoji_as_units() {
        // e + combining acute, family emoji (ZWJ sequence), flag.
        let r = Rope::from_str("e\u{301}x👨‍👩‍👧🇬🇧y");
        let s = r.slice(..);
        assert_eq!(next_grapheme_boundary(s, 0), 2);
        assert_eq!(next_grapheme_boundary(s, 2), 3);
        assert_eq!(next_grapheme_boundary(s, 3), 8); // 👨 ZWJ 👩 ZWJ 👧 = 5 scalars
        assert_eq!(next_grapheme_boundary(s, 8), 10); // flag = 2 regional indicators
        assert_eq!(prev_grapheme_boundary(s, 10), 8);
        assert_eq!(prev_grapheme_boundary(s, 8), 3);
        assert_eq!(prev_grapheme_boundary(s, 2), 0);
        assert_eq!(next_grapheme_boundary(s, 11), 11);
        assert!(is_grapheme_boundary(s, 3));
        assert!(!is_grapheme_boundary(s, 1));
        assert_eq!(snap_to_grapheme(s, 1), 0);
    }

    /// Unicode segmentation treats `\r\n` as a single grapheme; the caret must not split it.
    #[test]
    fn crlf_is_one_grapheme() {
        let r = Rope::from_str("a\r\nb");
        let s = r.slice(..);
        assert_eq!(next_grapheme_boundary(s, 1), 3);
        assert_eq!(prev_grapheme_boundary(s, 3), 1);
    }

    /// Pins the exact stop positions of each word motion on a mixed line.
    #[test]
    fn word_motion() {
        let r = Rope::from_str("foo_bar  baz->qux\nnext");
        let s = r.slice(..);
        assert_eq!(next_word_end(s, 0), 7);
        assert_eq!(next_word_end(s, 7), 12);
        assert_eq!(next_word_end(s, 12), 14);
        assert_eq!(next_word_end(s, 14), 17);
        assert_eq!(next_word_end(s, 17), 18); // over newline
        assert_eq!(prev_word_start(s, 18), 17);
        assert_eq!(prev_word_start(s, 17), 14);
        assert_eq!(prev_word_start(s, 12), 9);
        assert_eq!(prev_word_start(s, 9), 0);
        assert_eq!(prev_word_start(s, 0), 0);
        assert_eq!(next_word_start(s, 0), 9);
        assert_eq!(next_word_start(s, 9), 12);
        assert_eq!(word_at(s, 1), (0, 7));
        assert_eq!(word_at(s, 7), (0, 7));
        assert_eq!(word_at(s, 8), (8, 8));
        assert_eq!(word_at(s, 13), (12, 14));
    }

    /// Grapheme walking stays consistent across ropey chunk boundaries.
    #[test]
    fn large_rope_crosses_chunks() {
        let big: String = "ab\u{301}c ".repeat(20_000);
        let r = Rope::from_str(&big);
        let s = r.slice(..);
        let mut i = 0;
        let mut count = 0;
        while i < s.len_chars() {
            let n = next_grapheme_boundary(s, i);
            assert!(n > i);
            assert_eq!(prev_grapheme_boundary(s, n), i);
            i = n;
            count += 1;
        }
        assert_eq!(count, 20_000 * 4);
    }
}
