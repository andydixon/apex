//! Alignment for the side-by-side diff view: the two texts as alternating
//! equal and changed blocks of lines, word-level differences inside changed
//! blocks, and line mapping between the sides for synchronised scrolling.

use std::ops::Range;

use similar::{Algorithm, ChangeTag, DiffTag, TextDiff};

/// A run of lines: equal on both sides, or changed (either side may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub left: Range<usize>,
    pub right: Range<usize>,
    pub changed: bool,
}

/// Blocks covering both texts completely, in order.
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
pub fn map_line(blocks: &[Block], line: f32, from_left: bool) -> f32 {
    for b in blocks {
        let (src, dst) = if from_left { (&b.left, &b.right) } else { (&b.right, &b.left) };
        if line < src.end as f32 {
            if line < src.start as f32 {
                break;
            }
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
pub fn word_spans(left_block: &str, right_block: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if left_block.is_empty() || right_block.is_empty() || left_block.len() + right_block.len() > 64 * 1024 {
        return (Vec::new(), Vec::new());
    }
    let (lt, rt) = (tokens(left_block), tokens(right_block));
    let diff = TextDiff::configure().algorithm(Algorithm::Patience).diff_slices(&lt, &rt);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    let (mut lc, mut rc) = (0usize, 0usize);
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
fn tokens(s: &str) -> Vec<&str> {
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
