//! Atomic multi-range replacements with inverse generation and position mapping.
//!
//! A [`Transaction`] is the only way text changes: [`ops`](crate::ops) build
//! one from the selections, [`Document::apply`](crate::Document::apply)
//! commits it and records the inverse in history, and views use
//! [`Transaction::map_pos`] to carry selections, bookmarks and fold anchors
//! across the edit. All offsets are **char** offsets in *pre-edit*
//! coordinates; the inverse returned by `apply` is in post-edit coordinates,
//! so applying it to the edited rope restores the original.
//!
//! Changes are applied last-to-first so that each change's pre-edit offsets
//! are still valid when it is reached.

use ropey::Rope;
use serde::{Deserialize, Serialize};

/// Which side of an edit a position sticks to when it lies at or inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    /// Position stays before inserted text (left-gravity).
    Before,
    /// Position moves after inserted text (right-gravity).
    After,
}

/// Replace chars `start..end` with `text`. Coordinates are pre-edit char offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// First char replaced (inclusive).
    pub start: usize,
    /// End of the replaced range (exclusive); equals `start` for a pure insert.
    pub end: usize,
    /// Replacement text; empty for a pure delete.
    pub text: String,
}

impl Change {
    /// Insert `text` at char offset `at` without removing anything.
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self { start: at, end: at, text: text.into() }
    }

    /// Remove chars `start..end`.
    pub fn delete(start: usize, end: usize) -> Self {
        Self { start, end, text: String::new() }
    }

    /// Replace chars `start..end` with `text`.
    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        Self { start, end, text: text.into() }
    }

    /// Length of the inserted text in chars (`O(len)`; counts UTF-8 scalars).
    #[inline]
    pub fn new_len(&self) -> usize {
        self.text.chars().count()
    }

    /// Length of the removed range in chars.
    #[inline]
    pub fn old_len(&self) -> usize {
        self.end - self.start
    }

    /// True for an empty insert at a point: removes and adds nothing.
    pub fn is_noop(&self) -> bool {
        self.start == self.end && self.text.is_empty()
    }
}

/// A sorted, disjoint list of [`Change`]s applied atomically.
///
/// Immutable once built; `Serialize` so sessions can persist history-free
/// snapshots of pending edits if needed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    /// Sorted by `start`, pairwise disjoint, no no-ops.
    changes: Vec<Change>,
    /// `prefix[i]` = total length delta of `changes[..i]`, for O(log n) position mapping.
    prefix: Vec<isize>,
}

impl Transaction {
    /// Build a transaction, sorting changes and dropping any that overlap an
    /// earlier one (deterministic: the earlier-starting change wins). Callers
    /// producing changes from a normalised [`crate::SelectionSet`] never hit
    /// the overlap path.
    pub fn new(mut changes: Vec<Change>) -> Self {
        changes.retain(|c| !c.is_noop());
        changes.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        let mut out: Vec<Change> = Vec::with_capacity(changes.len());
        for c in changes {
            if let Some(last) = out.last()
                && c.start < last.end
            {
                tracing::warn!(?c, ?last, "dropping overlapping change");
                continue;
            }
            // Two insertions at the same point are fine; keep both in order.
            out.push(c);
        }
        Self::from_sorted(out)
    }

    /// Wrap already sorted, disjoint changes, computing the prefix deltas.
    fn from_sorted(changes: Vec<Change>) -> Self {
        let mut prefix = Vec::with_capacity(changes.len() + 1);
        let mut acc = 0isize;
        for c in &changes {
            prefix.push(acc);
            acc += c.new_len() as isize - c.old_len() as isize;
        }
        prefix.push(acc);
        Self { changes, prefix }
    }

    /// A transaction with one change.
    pub fn single(change: Change) -> Self {
        Self::new(vec![change])
    }

    /// The changes, sorted by start.
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// True when applying would change nothing.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Clamp all ranges into `rope` (defence against stale transactions).
    ///
    /// Clamping can make two changes touch but never overlap, so the result
    /// is still a valid sorted, disjoint list.
    pub fn clamped(mut self, rope: &Rope) -> Self {
        let n = rope.len_chars();
        for c in &mut self.changes {
            c.start = c.start.min(n);
            c.end = c.end.clamp(c.start, n);
        }
        Self::from_sorted(self.changes)
    }

    /// Apply to `rope`, returning the inverse transaction (in post-edit
    /// coordinates) and the byte-level edits in the order they were applied
    /// (last-to-first), which is the order a syntax tree must be told about them.
    pub fn apply(&self, rope: &mut Rope) -> (Transaction, Vec<crate::document::ByteEdit>) {
        // Prefix deltas so each inverse change can be expressed in post-edit coordinates.
        let mut prefix = Vec::with_capacity(self.changes.len());
        let mut acc: isize = 0;
        for c in &self.changes {
            prefix.push(acc);
            acc += c.new_len() as isize - c.old_len() as isize;
        }
        let mut inverse = vec![Change::insert(0, ""); self.changes.len()];
        let mut byte_edits = Vec::with_capacity(self.changes.len());
        // Last-to-first: earlier changes' pre-edit offsets are untouched by
        // edits made after them in the text.
        for (i, c) in self.changes.iter().enumerate().rev() {
            let n = rope.len_chars();
            let start = c.start.min(n);
            let end = c.end.clamp(start, n);
            let start_byte = rope.char_to_byte(start);
            let old_end_byte = rope.char_to_byte(end);
            let start_point = point_of(rope, start_byte);
            let old_end_point = point_of(rope, old_end_byte);
            let removed: String = rope.slice(start..end).into();
            rope.remove(start..end);
            rope.insert(start, &c.text);
            let new_end_byte = start_byte + c.text.len();
            let new_end_point = point_of(rope, new_end_byte);
            // Post-edit start = pre-edit start shifted by everything before it.
            let new_start = (start as isize + prefix[i]).max(0) as usize;
            inverse[i] = Change { start: new_start, end: new_start + c.new_len(), text: removed };
            byte_edits.push(crate::document::ByteEdit { start_byte, old_end_byte, new_end_byte, start_point, old_end_point, new_end_point });
        }
        let inverse = Transaction::from_sorted(inverse);
        debug_assert!(inverse.changes.windows(2).all(|w| w[0].end <= w[1].start));
        (inverse, byte_edits)
    }

    /// Map a pre-edit position to its post-edit equivalent.
    ///
    /// `assoc` decides what happens when `pos` sits exactly at an insertion
    /// point or inside a replaced range: `Before` keeps it at the start of
    /// the new text, `After` moves it past the new text. A position at the
    /// end of a replaced range always ends up after the replacement.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        // Changes are sorted and disjoint, so only the last change starting
        // before `pos` can contain it; everything earlier contributes a plain
        // length delta. Binary search keeps 1,000-cursor transactions cheap.
        let first = self.changes.partition_point(|c| c.start < pos).saturating_sub(1);
        let mut delta: isize = self.prefix[first];
        for c in &self.changes[first..] {
            if pos < c.start {
                break;
            }
            let new_len = c.new_len() as isize;
            let old_len = c.old_len() as isize;
            if pos == c.start && c.old_len() == 0 {
                // Insertion exactly at pos.
                match assoc {
                    Assoc::Before => break,
                    Assoc::After => {
                        delta += new_len;
                        continue;
                    }
                }
            }
            if pos < c.end || (pos == c.end && c.old_len() > 0) {
                if pos == c.end {
                    // At the end of a replaced range: after the new text.
                    delta += new_len - old_len;
                    continue;
                }
                // Inside the replaced range.
                let base = (c.start as isize + delta) as usize;
                return match assoc {
                    Assoc::Before => base,
                    Assoc::After => base + c.new_len(),
                };
            }
            delta += new_len - old_len;
        }
        ((pos as isize) + delta).max(0) as usize
    }

    /// Total change in char count after applying.
    pub fn len_delta(&self) -> isize {
        self.changes.iter().map(|c| c.new_len() as isize - c.old_len() as isize).sum()
    }

    /// Byte size of the stored text (for history memory accounting).
    pub fn text_bytes(&self) -> usize {
        self.changes.iter().map(|c| c.text.len()).sum()
    }

    /// Char range touched by the transaction in post-edit coordinates, if any.
    ///
    /// Used to decide which lines need re-layout or where to scroll after an
    /// edit. `None` for an empty transaction.
    pub fn touched_range_after(&self) -> Option<std::ops::Range<usize>> {
        let first = self.changes.first()?;
        let last = self.changes.last()?;
        let end = self.map_pos(last.end, Assoc::After);
        Some(first.start..end.max(first.start))
    }
}

/// (row, byte column) of a byte offset, as tree-sitter's `Point` wants it.
fn point_of(rope: &Rope, byte: usize) -> (usize, usize) {
    let byte = byte.min(rope.len_bytes());
    let line = rope.byte_to_line(byte);
    (line, byte - rope.line_to_byte(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applying then applying the inverse restores the text; the inverse's inverse is the original shape.
    #[test]
    fn apply_and_invert() {
        let mut r = Rope::from_str("hello world");
        let tx = Transaction::new(vec![Change::replace(0, 5, "HELLO"), Change::insert(11, "!"), Change::delete(5, 6)]);
        let (inv, edits) = tx.apply(&mut r);
        assert_eq!(r.to_string(), "HELLOworld!");
        assert_eq!(edits.len(), 3);
        let (redo, _) = inv.apply(&mut r);
        assert_eq!(r.to_string(), "hello world");
        assert_eq!(redo.changes().len(), 3);
    }

    /// Pins `map_pos` semantics at and around inserts, deletes and replacements.
    #[test]
    fn map_positions() {
        let tx = Transaction::new(vec![Change::insert(2, "XX"), Change::delete(5, 8), Change::replace(10, 12, "Y")]);
        assert_eq!(tx.map_pos(0, Assoc::After), 0);
        assert_eq!(tx.map_pos(2, Assoc::Before), 2);
        assert_eq!(tx.map_pos(2, Assoc::After), 4);
        assert_eq!(tx.map_pos(3, Assoc::After), 5);
        assert_eq!(tx.map_pos(6, Assoc::Before), 7);
        assert_eq!(tx.map_pos(6, Assoc::After), 7);
        assert_eq!(tx.map_pos(8, Assoc::Before), 7);
        assert_eq!(tx.map_pos(9, Assoc::Before), 8);
        assert_eq!(tx.map_pos(11, Assoc::Before), 9);
        assert_eq!(tx.map_pos(11, Assoc::After), 10);
        assert_eq!(tx.map_pos(12, Assoc::Before), 10);
        assert_eq!(tx.map_pos(20, Assoc::Before), 18);
    }

    /// The earlier-starting change wins; a later touching change survives.
    #[test]
    fn overlapping_changes_are_dropped_deterministically() {
        let tx = Transaction::new(vec![Change::delete(0, 5), Change::delete(3, 8), Change::delete(5, 6)]);
        assert_eq!(tx.changes().len(), 2);
        assert_eq!(tx.changes()[1].start, 5);
    }

    /// Byte edits carry correct (row, column) points across an inserted newline.
    #[test]
    fn byte_edits_have_points() {
        let mut r = Rope::from_str("ab\ncd");
        let tx = Transaction::single(Change::insert(4, "X\nY"));
        let (_, e) = tx.apply(&mut r);
        assert_eq!(e[0].start_byte, 4);
        assert_eq!(e[0].start_point, (1, 1));
        assert_eq!(e[0].new_end_byte, 7);
        assert_eq!(e[0].new_end_point, (2, 1));
    }
}
