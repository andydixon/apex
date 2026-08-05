//! Fold regions from syntax trees and from indentation.

use apex_buffer::Rope;
use apex_buffer::position::line_len_chars;

use crate::highlight::SyntaxState;

/// A foldable region: lines `start + 1 ..= end` are hidden when folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    pub start: usize,
    pub end: usize,
    /// Nesting depth, 0 for top level.
    pub depth: u16,
}

/// Fold regions from the syntax tree. One region per start line (the outermost).
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
        let is_comment = kind.contains("comment");
        if is_comment && node.is_named() {
            // Merge consecutive single-line comments into one region.
            match comment_run {
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
            regions.push((s, e));
        }
        if end > start && node.is_named() {
            let bracketed = node.child(0).is_some_and(|c| matches!(c.kind(), "{" | "(" | "[" | "<"));
            if bracketed || kinds.contains(&kind) || (is_comment && end > start) {
                regions.push((start, end));
            }
        }
        let _ = &mut depth_stack;
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
pub fn indent_folds(rope: &Rope, tab_width: usize) -> Vec<FoldRegion> {
    let n = rope.len_lines();
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

fn finish(mut regions: Vec<(usize, usize)>, rope: &Rope) -> Vec<FoldRegion> {
    let last = rope.len_lines().saturating_sub(1);
    regions.retain(|(s, e)| e > s && *s <= last);
    for r in &mut regions {
        r.1 = r.1.min(last);
    }
    // Outermost per start line.
    regions.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    regions.dedup_by_key(|r| r.0);
    // Compute nesting depth with a stack of open regions.
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

    #[test]
    fn indentation_folds() {
        let rope = Rope::from_str("a\n  b\n  c\n\n  d\ne\n");
        let folds = indent_folds(&rope, 4);
        assert_eq!(folds, vec![FoldRegion { start: 0, end: 4, depth: 0 }]);
    }
}
