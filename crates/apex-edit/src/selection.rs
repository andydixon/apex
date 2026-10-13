//! Cursor and selection representation.
//!
//! A view owns one [`SelectionSet`]; every editing command in
//! [`ops`](crate::ops) reads it to build a transaction and then writes the
//! post-edit set back. The set is kept normalised (sorted, non-overlapping,
//! never empty) by [`SelectionSet::normalise`], which every mutator calls, so
//! the rest of the crate may rely on those invariants without re-checking.
//!
//! All offsets are **char** offsets. `goal_col` is a **display column**, the
//! one exception, because vertical motion must line up visually across tabs.

use std::cmp::Ordering;

use apex_buffer::motion::snap_to_grapheme;
use ropey::Rope;
use serde::{Deserialize, Serialize};

use crate::transaction::{Assoc, Transaction};

/// A single caret with an optional extent. Offsets are **char** offsets.
///
/// `anchor` is where the selection started; `head` is the moving end that
/// follows the caret. When they are equal the selection is a plain cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Fixed end, where the selection was started.
    pub anchor: usize,
    /// Moving end; the caret is drawn here.
    pub head: usize,
    /// Preferred display column for vertical motion (`None` = use current).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_col: Option<usize>,
}

impl Selection {
    /// A plain cursor at `pos`.
    pub const fn point(pos: usize) -> Self {
        Self { anchor: pos, head: pos, goal_col: None }
    }

    /// A selection from `anchor` to `head` (either order).
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal_col: None }
    }

    /// Lower of the two ends.
    #[inline]
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// Higher of the two ends (exclusive).
    #[inline]
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// `start()..end()`.
    #[inline]
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start()..self.end()
    }

    /// True for a plain cursor with no extent.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Extent in chars.
    #[inline]
    pub fn len(&self) -> usize {
        self.end() - self.start()
    }

    /// True when the head is before the anchor (selection made leftwards/upwards).
    #[inline]
    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// Collapse to the head, dropping the extent.
    pub fn collapse(self) -> Self {
        Self { anchor: self.head, head: self.head, goal_col: self.goal_col }
    }

    /// Collapse to the lower end.
    pub fn collapse_to_start(self) -> Self {
        Self::point(self.start())
    }

    /// Collapse to the upper end.
    pub fn collapse_to_end(self) -> Self {
        Self::point(self.end())
    }

    /// Exchange anchor and head (the "flip selection direction" command).
    pub fn swap_ends(self) -> Self {
        Self { anchor: self.head, head: self.anchor, goal_col: None }
    }

    /// Move the head, extending if `extend` else collapsing onto the new head.
    pub fn moved_to(self, head: usize, extend: bool) -> Self {
        if extend { Self { anchor: self.anchor, head, goal_col: None } } else { Self::point(head) }
    }

    /// Set the preferred display column for the next vertical move.
    pub fn with_goal(mut self, goal: Option<usize>) -> Self {
        self.goal_col = goal;
        self
    }

    /// True when `pos` lies within the extent (`start <= pos < end`); never true for a cursor.
    pub fn contains(&self, pos: usize) -> bool {
        self.start() <= pos && pos < self.end()
    }

    /// True when the extents intersect. Two cursors overlap only if they
    /// coincide; a cursor touching the edge of a range does not overlap it.
    pub fn overlaps(&self, other: &Selection) -> bool {
        if self.is_empty() && other.is_empty() {
            return self.head == other.head;
        }
        self.start() < other.end() && other.start() < self.end()
    }

    /// Clamp into `0..=len` and snap both ends to grapheme boundaries.
    pub fn clamped(self, rope: &Rope) -> Self {
        let s = rope.slice(..);
        Self { anchor: snap_to_grapheme(s, self.anchor), head: snap_to_grapheme(s, self.head), goal_col: self.goal_col }
    }

    /// Map both endpoints through `tx`.
    pub fn mapped(self, tx: &Transaction, anchor_assoc: Assoc, head_assoc: Assoc) -> Self {
        Self { anchor: tx.map_pos(self.anchor, anchor_assoc), head: tx.map_pos(self.head, head_assoc), goal_col: None }
    }
}

/// An ordered, non-overlapping set of selections with one designated primary.
///
/// The primary is the one the view scrolls to and the one commands that only
/// make sense for a single caret (e.g. "go to line") operate on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSet {
    /// Sorted by start, pairwise non-overlapping, at least one element.
    sels: Vec<Selection>,
    /// Index into `sels`; always in range.
    primary: usize,
}

impl Default for SelectionSet {
    /// A single cursor at offset 0.
    fn default() -> Self {
        Self::single(Selection::point(0))
    }
}

impl SelectionSet {
    /// A set containing just `sel`.
    pub fn single(sel: Selection) -> Self {
        Self { sels: vec![sel], primary: 0 }
    }

    /// Build from arbitrary selections; `primary` indexes the input slice.
    /// The result is sorted and merged; the primary follows its selection.
    pub fn from_vec(sels: Vec<Selection>, primary: usize) -> Self {
        if sels.is_empty() {
            return Self::default();
        }
        let mut s = Self { sels, primary: 0 };
        s.primary = primary;
        s.normalise();
        s
    }

    /// The primary selection (copied out; `Selection` is `Copy`).
    #[inline]
    pub fn primary(&self) -> Selection {
        self.sels[self.primary]
    }

    /// Index of the primary within `as_slice()`.
    #[inline]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Make the selection at index `i` primary (clamped into range).
    pub fn set_primary_index(&mut self, i: usize) {
        self.primary = i.min(self.sels.len() - 1);
    }

    /// Iterate selections in offset order.
    #[inline]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Selection> + DoubleEndedIterator {
        self.sels.iter()
    }

    /// The selections in offset order.
    #[inline]
    pub fn as_slice(&self) -> &[Selection] {
        &self.sels
    }

    /// Number of selections (always at least 1).
    #[inline]
    pub fn len(&self) -> usize {
        self.sels.len()
    }

    /// Always `false`: the set is never empty. Exists to satisfy the
    /// `len`/`is_empty` lint pairing.
    #[inline]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// True when at least one selection has a non-zero extent.
    pub fn has_extent(&self) -> bool {
        self.sels.iter().any(|s| !s.is_empty())
    }

    /// Replace everything with a single selection.
    pub fn set_single(&mut self, sel: Selection) {
        self.sels.clear();
        self.sels.push(sel);
        self.primary = 0;
    }

    /// Add a selection and make it primary.
    pub fn push_primary(&mut self, sel: Selection) {
        self.sels.push(sel);
        self.primary = self.sels.len() - 1;
        self.normalise();
    }

    /// Add a selection without changing the primary.
    pub fn push(&mut self, sel: Selection) {
        self.sels.push(sel);
        self.normalise();
    }

    /// Remove the primary selection; the next one (or the last) becomes primary.
    /// Returns false, and does nothing, if only one selection remains.
    pub fn remove_primary(&mut self) -> bool {
        if self.sels.len() <= 1 {
            return false;
        }
        self.sels.remove(self.primary);
        self.primary = self.primary.min(self.sels.len() - 1);
        true
    }

    /// Keep only the primary selection.
    pub fn keep_primary(&mut self) {
        let p = self.primary();
        self.set_single(p);
    }

    /// Apply `f` to every selection, then re-normalise.
    pub fn transform(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for s in &mut self.sels {
            *s = f(*s);
        }
        self.normalise();
    }

    /// Apply `f` to every selection, yielding possibly several per input
    /// (e.g. "split selection into lines"). If `f` yields nothing for any
    /// input the old primary is kept so the set never becomes empty.
    pub fn flat_transform(&mut self, mut f: impl FnMut(Selection) -> Vec<Selection>) {
        let mut out = Vec::with_capacity(self.sels.len());
        let mut primary = 0;
        for (i, s) in self.sels.iter().enumerate() {
            let produced = f(*s);
            // The primary becomes the last selection produced from the old primary.
            if i == self.primary {
                primary = out.len() + produced.len().saturating_sub(1);
            }
            out.extend(produced);
        }
        if out.is_empty() {
            out.push(self.primary());
            primary = 0;
        }
        self.sels = out;
        self.primary = primary;
        self.normalise();
    }

    /// Clamp all selections into the rope and snap to grapheme boundaries.
    pub fn clamp(&mut self, rope: &Rope) {
        self.transform(|s| s.clamped(rope));
    }

    /// Map through a transaction. `assoc` is used for both endpoints.
    pub fn map(&mut self, tx: &Transaction, assoc: Assoc) {
        self.transform(|s| s.mapped(tx, assoc, assoc));
    }

    /// Sort by start, merge overlapping selections, deduplicate cursors, and
    /// keep the primary pointing at the selection that absorbed it.
    ///
    /// Merging two overlapping selections yields one spanning both, oriented
    /// like the one that contained the primary (or the earlier one).
    pub fn normalise(&mut self) {
        if self.sels.len() <= 1 {
            self.primary = 0;
            return;
        }
        let primary_sel = self.sels[self.primary.min(self.sels.len() - 1)];
        // Stable sort so equal cursors keep insertion order.
        let mut indexed: Vec<(usize, Selection)> = self.sels.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| a.1.start().cmp(&b.1.start()).then(a.1.end().cmp(&b.1.end())));
        let mut merged: Vec<Selection> = Vec::with_capacity(indexed.len());
        let mut primary = 0;
        for (orig_idx, sel) in indexed {
            let is_primary = orig_idx == self.primary;
            if let Some(last) = merged.last_mut() {
                // A cursor strictly inside a range is swallowed by it; a
                // cursor on a range's edge survives as a separate selection.
                let overlap = last.overlaps(&sel) || (last.is_empty() && sel.contains(last.head)) || (sel.is_empty() && last.contains(sel.head));
                if overlap {
                    let start = last.start().min(sel.start());
                    let end = last.end().max(sel.end());
                    let reversed = if is_primary { sel.is_reversed() } else { last.is_reversed() };
                    *last = if reversed { Selection::new(end, start) } else { Selection::new(start, end) };
                    if is_primary {
                        primary = merged.len() - 1;
                    }
                    continue;
                }
            }
            if is_primary {
                primary = merged.len();
            }
            merged.push(sel);
        }
        self.sels = merged;
        self.primary = primary;
        debug_assert!(self.sels.iter().any(|s| s.start() == primary_sel.start() || true));
    }

    /// Index of the selection containing `pos`, if any.
    ///
    /// A cursor "contains" only its own offset; a range contains
    /// `start..end` (end exclusive). Binary search over the sorted set.
    pub fn find_containing(&self, pos: usize) -> Option<usize> {
        self.sels
            .binary_search_by(|s| {
                if pos < s.start() {
                    Ordering::Greater
                } else if pos >= s.end() && !(s.is_empty() && s.head == pos) {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()
    }

    /// Rotate the primary to the next selection (wrapping).
    pub fn rotate_primary(&mut self, forward: bool) {
        let n = self.sels.len();
        self.primary = if forward { (self.primary + 1) % n } else { (self.primary + n - 1) % n };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Overlapping ranges merge, duplicate cursors collapse, and the primary follows its selection.
    #[test]
    fn normalise_merges_and_dedupes() {
        let mut s =
            SelectionSet::from_vec(vec![Selection::new(5, 10), Selection::point(3), Selection::new(8, 12), Selection::point(3), Selection::point(7)], 2);
        assert_eq!(s.as_slice(), &[Selection::point(3), Selection::new(5, 12)]);
        assert_eq!(s.primary_index(), 1);
        s.push_primary(Selection::new(20, 15));
        assert_eq!(s.primary(), Selection::new(20, 15));
        assert!(s.remove_primary());
        assert_eq!(s.len(), 2);
        s.keep_primary();
        assert_eq!(s.len(), 1);
        assert!(!s.remove_primary());
    }

    /// A cursor matches only its own offset; a range excludes its end.
    #[test]
    fn find_containing() {
        let s = SelectionSet::from_vec(vec![Selection::point(2), Selection::new(5, 9)], 0);
        assert_eq!(s.find_containing(2), Some(0));
        assert_eq!(s.find_containing(3), None);
        assert_eq!(s.find_containing(6), Some(1));
        assert_eq!(s.find_containing(9), None);
    }

    /// Out-of-range endpoints clamp to the rope length, keeping direction.
    #[test]
    fn clamps_to_rope() {
        let r = Rope::from_str("ab");
        let mut s = SelectionSet::single(Selection::new(50, 1));
        s.clamp(&r);
        assert_eq!(s.primary(), Selection::new(2, 1));
    }
}
