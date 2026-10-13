//! Alignment for the side-by-side diff view: the two texts as alternating
//! equal and changed blocks of lines, word-level differences inside changed
//! blocks, and line mapping between the sides for synchronised scrolling.
//!
//! Pure functions over strings; no I/O. The diff view in `apex-ui` calls
//! `blocks` once per pair of texts, `word_spans` per visible changed block,
//! and `map_line` every frame to keep the two scroll positions in step.
//! Line and character indices are 0-based; ranges are half-open. All
//! diffing uses the patience algorithm from the `similar` crate, which gives
//! more readable results than Myers on code with repeated lines.

use std::ops::Range;

use similar::{Algorithm, ChangeTag, DiffTag, TextDiff};

/// A run of lines: equal on both sides, or changed (either side may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Line range on the left (old) side.
    pub left: Range<usize>,
    /// Line range on the right (new) side.
    pub right: Range<usize>,
    /// False for an equal run; true for a replacement, pure insertion
    /// (`left` empty) or pure deletion (`right` empty).
    pub changed: bool,
}

/// Blocks covering both texts completely, in order.
///
/// Consecutive blocks are contiguous on each side (`b[i].left.end ==
/// b[i+1].left.start`, same for `right`), so the result partitions both
/// texts. Two identical texts yield a single unchanged block.
pub fn blocks(left: &str, right: &str) -> Vec<Block> {
    let diff = TextDiff::configure().algorithm(Algorithm::Patience).diff_lines(left, right);
    let mut out: Vec<Block> = Vec::new();
    for op in diff.ops() {
        let (tag, l, r) = op.as_tag_tuple();
        let changed = tag != DiffTag::Equal;
        match out.last_mut() {
            // Merge adjacent changes (delete followed by insert = one replacement).
            Some(last) if last.changed && changed => {
                last.left.end = l.end;
                last.right.end = r.end;
            }
            _ => out.push(Block { left: l, right: r, changed }),
        }
    }
    out
}

/// The changed blocks only.
pub fn changes(blocks: &[Block]) -> impl Iterator<Item = &Block> {
    blocks.iter().filter(|b| b.changed)
}

/// Map a (fractional) line on one side to the other side: proportional
/// inside a block, so both sides stay aligned at block boundaries.
///
/// `from_left` selects the direction. Fractional input lets the caller map
/// a scroll offset rather than a whole line. An empty source range (e.g. a
/// pure insertion seen from the left) maps to the start of its destination
/// range.
pub fn map_line(blocks: &[Block], line: f32, from_left: bool) -> f32 {
    for b in blocks {
        let (src, dst) = if from_left { (&b.left, &b.right) } else { (&b.right, &b.left) };
        if line < src.end as f32 {
            // Blocks are contiguous and start at 0, so a line before this
            // block's start can only be negative; break to the fallback
            // below rather than extrapolating backwards.
            if line < src.start as f32 {
                break;
            }
            // `max(1)` guards the empty-range case so we never divide by zero.
            let span = (src.end - src.start).max(1) as f32;
            let t = ((line - src.start as f32) / span).clamp(0.0, 1.0);
            return dst.start as f32 + t * (dst.end - dst.start) as f32;
        }
    }
    // Past the end: keep the same distance from the last line.
    let (s_end, d_end) = blocks.last().map(|b| if from_left { (b.left.end, b.right.end) } else { (b.right.end, b.left.end) }).unwrap_or((0, 0));
    d_end as f32 + (line - s_end as f32).max(0.0)
}

/// Character ranges (relative to each block's text) that differ by word
/// inside a changed block with lines on both sides. Large blocks are left
/// line-level only.
///
/// Offsets are in `char`s, not bytes, counted from the start of each block's
/// text. Adjacent differing tokens are merged into one range. Whitespace-only
/// insertions and deletions are not highlighted. Blocks over 64 KiB combined
/// return empty vectors because token diffing large blocks is too slow for a
/// per-frame view.
pub fn word_spans(left_block: &str, right_block: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if left_block.is_empty() || right_block.is_empty() || left_block.len() + right_block.len() > 64 * 1024 {
        return (Vec::new(), Vec::new());
    }
    let (lt, rt) = (tokens(left_block), tokens(right_block));
    let diff = TextDiff::configure().algorithm(Algorithm::Patience).diff_slices(&lt, &rt);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    // Running char offsets on each side.
    let (mut lc, mut rc) = (0usize, 0usize);
    // Extend the previous range when the new one starts where it ended.
    let push = |v: &mut Vec<Range<usize>>, a: usize, b: usize| match v.last_mut() {
        Some(last) if last.end == a => last.end = b,
        _ => v.push(a..b),
    };
    for c in diff.iter_all_changes() {
        let n = c.value().chars().count();
        let blank = c.value().trim().is_empty();
        match c.tag() {
            ChangeTag::Equal => {
                lc += n;
                rc += n;
            }
            ChangeTag::Delete => {
                // Offsets advance even for blank tokens; only the highlight
                // is skipped.
                if !blank {
                    push(&mut l, lc, lc + n);
                }
                lc += n;
            }
            ChangeTag::Insert => {
                if !blank {
                    push(&mut r, rc, rc + n);
                }
                rc += n;
            }
        }
    }
    (l, r)
}

/// Identifier runs, whitespace runs and single other characters.
///
/// Splitting punctuation into single-character tokens means `foo(bar)` and
/// `foo(baz)` differ only in the identifier, not the whole call.
fn tokens(s: &str) -> Vec<&str> {
    // 0 = identifier char, 1 = whitespace, 2 = anything else.
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<u8> = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        // Start a new token on a class change, and after every punctuation
        // character (class 2 never forms runs).
        if let Some(p) = prev
            && (p != k || k == 2)
        {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(k);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks partition both sides with no gaps, and pure insertions produce
    /// an empty left range.
    #[test]
    fn blocks_cover_both_sides() {
        let l = "a\nb\nc\nd\ne\n";
        let r = "a\nB\nc\nd\nx\ny\ne\n";
        let b = blocks(l, r);
        assert_eq!(
            b,
            vec![
                Block { left: 0..1, right: 0..1, changed: false },
                Block { left: 1..2, right: 1..2, changed: true },
                Block { left: 2..4, right: 2..4, changed: false },
                Block { left: 4..4, right: 4..6, changed: true },
                Block { left: 4..5, right: 6..7, changed: false },
            ]
        );
        assert_eq!(changes(&b).count(), 2);
        // Coverage: consecutive and complete.
        for w in b.windows(2) {
            assert_eq!(w[0].left.end, w[1].left.start);
            assert_eq!(w[0].right.end, w[1].right.start);
        }
        assert!(blocks("same\n", "same\n").iter().all(|b| !b.changed));
        assert_eq!(blocks("", "new\n"), vec![Block { left: 0..0, right: 0..1, changed: true }]);
    }

    /// Line mapping is exact at block boundaries, proportional inside blocks
    /// and offset-preserving past the end.
    #[test]
    fn line_mapping() {
        let b = blocks("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nx\ny\ne\n");
        assert_eq!(map_line(&b, 0.0, true), 0.0);
        assert_eq!(map_line(&b, 3.0, true), 3.0);
        // Left line 4 ("e") sits after the insertion: right line 6.
        assert_eq!(map_line(&b, 4.0, true), 6.0);
        assert_eq!(map_line(&b, 5.0, false), 4.0);
        assert_eq!(map_line(&b, 6.5, false), 4.5);
        assert_eq!(map_line(&b, 9.0, true), 11.0);
    }

    /// Word spans are char offsets, skip whitespace-only changes and are
    /// empty when one side is empty.
    #[test]
    fn word_level_spans() {
        let (l, r) = word_spans("let x = old_value;\n", "let x = new_value;\n");
        assert_eq!(l, vec![8..17]);
        assert_eq!(r, vec![8..17]);
        let (l, r) = word_spans("a b\n", "a b c\n");
        assert!(l.is_empty());
        assert_eq!(r, vec![4..5]);
        assert_eq!(word_spans("", "x\n"), (vec![], vec![]));
    }
}
