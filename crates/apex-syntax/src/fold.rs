//! Fold regions from syntax trees and from indentation.
//!
//! The UI asks for a fresh `Vec<FoldRegion>` whenever the document changes:
//! [`syntax_folds`] when a tree is available, [`indent_folds`] otherwise
//! (plain text, or a language without a grammar). Both produce `(start, end)`
//! **line** pairs and hand them to [`finish`], which clamps, keeps only the
//! outermost region per start line and assigns nesting depths.
//!
//! Regions are recomputed from scratch rather than edited incrementally; the
//! cost is one tree walk, which is cheap next to the parse that preceded it.

use apex_buffer::Rope;
use apex_buffer::position::line_len_chars;

use crate::highlight::SyntaxState;

/// A foldable region: lines `start + 1 ..= end` are hidden when folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    /// Zero-based line that carries the fold marker and stays visible.
    pub start: usize,
    /// Zero-based last hidden line (inclusive); always greater than `start`.
    pub end: usize,
    /// Nesting depth, 0 for top level.
    pub depth: u16,
}

/// Fold regions from the syntax tree. One region per start line (the outermost).
///
/// A named node folds when it spans more than one line and either starts
/// with a bracket token, is listed in the language's `fold_kinds`, or is a
/// multi-line comment. Runs of consecutive single-line comments are merged
/// into one region so a doc-comment block folds as a unit. Only the root
/// layer is walked; injected languages do not contribute folds.
pub fn syntax_folds(state: &SyntaxState, rope: &Rope) -> Vec<FoldRegion> {
    let Some(tree) = state.root_tree() else { return Vec::new() };
    let kinds = state.lang.fold_kinds;
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut cursor = tree.walk();
    let mut depth_stack: Vec<usize> = Vec::new();
    let mut comment_run: Option<(usize, usize)> = None;
    // Depth-first traversal without recursion.
    'outer: loop {
        let node = cursor.node();
        let start = node.start_position().row;
        let mut end = node.end_position().row;
        // A node whose end is at column 0 of the next line really ends on the previous line.
        if node.end_position().column == 0 && end > start {
            end -= 1;
        }
        let kind = node.kind();
        // Substring match so `line_comment`, `block_comment`, `Comment` all qualify.
        let is_comment = kind.contains("comment");
        if is_comment && node.is_named() {
            // Merge consecutive single-line comments into one region.
            match comment_run {
                // Directly follows the previous comment: extend the run.
                Some((s, e)) if start == e + 1 => comment_run = Some((s, end)),
                Some((s, e)) => {
                    if e > s {
                        regions.push((s, e));
                    }
                    comment_run = Some((start, end));
                }
                None => comment_run = Some((start, end)),
            }
        } else if node.is_named()
            && !is_comment
            && let Some((s, e)) = comment_run.take()
            && e > s
        {
            // Any other named node breaks the comment run; flush it if it
            // spans more than one line.
            regions.push((s, e));
        }
        if end > start && node.is_named() {
            // Anything opened by a bracket folds regardless of language, which
            // keeps the per-language `fold_kinds` lists short.
            let bracketed = node.child(0).is_some_and(|c| matches!(c.kind(), "{" | "(" | "[" | "<"));
            if bracketed || kinds.contains(&kind) || (is_comment && end > start) {
                regions.push((start, end));
            }
        }
        let _ = &mut depth_stack;
        // Pre-order: descend first, otherwise move to the next sibling,
        // climbing back up as many levels as needed.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'outer;
            }
            if !cursor.goto_parent() {
                break 'outer;
            }
        }
    }
    if let Some((s, e)) = comment_run
        && e > s
    {
        regions.push((s, e));
    }
    finish(regions, rope)
}

/// Fold regions from indentation: a line followed by more-indented lines.
///
/// Blank lines are transparent (they neither extend nor end a region, so a
/// blank line inside a Python block does not split the fold), but a region
/// never ends on one: `end` is the last more-indented non-blank line.
/// Indentation is measured in display columns with tabs expanded to
/// `tab_width`. `O(n^2)` in the worst case (deeply nested, all-indented files);
/// fine in practice because the inner scan stops at the first dedent.
pub fn indent_folds(rope: &Rope, tab_width: usize) -> Vec<FoldRegion> {
    let n = rope.len_lines();
    // Display-column indentation of a line, or `None` for a blank line.
    let indent_of = |line: usize| -> Option<usize> {
        if line_len_chars(rope, line) == 0 {
            return None;
        }
        let mut col = 0;
        for c in rope.line(line).chars() {
            match c {
                ' ' => col += 1,
                '\t' => col += tab_width - col % tab_width,
                '\n' | '\r' => return None,
                _ => return Some(col),
            }
        }
        None
    };
    let indents: Vec<Option<usize>> = (0..n).map(indent_of).collect();
    let mut regions = Vec::new();
    for start in 0..n {
        let Some(base) = indents[start] else { continue };
        let mut end = start;
        let mut j = start + 1;
        while j < n {
            match indents[j] {
                Some(i) if i > base => end = j,
                Some(_) => break,
                None => {}
            }
            j += 1;
        }
        if end > start {
            regions.push((start, end));
        }
    }
    finish(regions, rope)
}

/// Normalise raw `(start, end)` line pairs into the final region list:
/// drop empty or out-of-range pairs, clamp `end` to the last line, keep only
/// the widest region per start line, and compute nesting depth.
///
/// The output is sorted by start line, which the gutter relies on.
fn finish(mut regions: Vec<(usize, usize)>, rope: &Rope) -> Vec<FoldRegion> {
    let last = rope.len_lines().saturating_sub(1);
    regions.retain(|(s, e)| e > s && *s <= last);
    for r in &mut regions {
        r.1 = r.1.min(last);
    }
    // Outermost per start line.
    // Sort by start ascending then end descending, so `dedup_by_key` keeps
    // the longest region for each start line.
    regions.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    regions.dedup_by_key(|r| r.0);
    // Compute nesting depth with a stack of open regions.
    // Regions are sorted by start, so popping every open region that ends
    // before this one starts leaves exactly the enclosing regions on the stack.
    let mut out = Vec::with_capacity(regions.len());
    let mut stack: Vec<usize> = Vec::new();
    for (s, e) in regions {
        while stack.last().is_some_and(|&end| end < s) {
            stack.pop();
        }
        out.push(FoldRegion { start: s, end: e, depth: stack.len() as u16 });
        stack.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::lookup;
    use apex_edit::Document;
    use std::time::Duration;

    /// A three-line comment run folds as one region; nested blocks get increasing depth.
    #[test]
    fn rust_folds_blocks_and_comment_runs() {
        let src = "// a\n// b\n// c\nfn main() {\n    if x {\n        y();\n    }\n}\n";
        let mut doc = Document::from_text(src);
        let s = SyntaxState::new(lookup("rust").unwrap(), &mut doc, Duration::from_secs(5));
        let folds = syntax_folds(&s, doc.rope());
        assert!(folds.contains(&FoldRegion { start: 0, end: 2, depth: 0 }), "{folds:?}");
        assert!(folds.contains(&FoldRegion { start: 3, end: 7, depth: 0 }), "{folds:?}");
        assert!(folds.contains(&FoldRegion { start: 4, end: 6, depth: 1 }), "{folds:?}");
    }

    /// A blank line inside an indented block does not split the fold.
    #[test]
    fn indentation_folds() {
        let rope = Rope::from_str("a\n  b\n  c\n\n  d\ne\n");
        let folds = indent_folds(&rope, 4);
        assert_eq!(folds, vec![FoldRegion { start: 0, end: 4, depth: 0 }]);
    }
}
