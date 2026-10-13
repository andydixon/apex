//! Undo/redo history.
//!
//! Each entry is a list of (forward, inverse) transaction pairs so that
//! coalesced typing can be undone in one step without composing transactions.
//! Redo is linear: a new edit after an undo discards the redo stack. This is
//! the behaviour of Sublime Text and most editors, and it is documented in the
//! user guide as such.
//!
//! [`History`] does not touch the rope: [`Document`](crate::Document) applies
//! transactions and hands the (forward, inverse) pair to [`History::record`];
//! on undo it pops an entry and applies the inverses itself. Each entry also
//! stores the selections before and after, so undo restores the caret
//! layout, and the revision span, so views can tell how far back they went.
//!
//! Memory is bounded two ways: a hard entry count and an approximate byte
//! budget for stored replacement text; the oldest entries are dropped first.

use std::time::{Duration, Instant};

use crate::document::EditKind;
use crate::selection::SelectionSet;
use crate::transaction::Transaction;

/// One undoable unit: a single edit, or a run of coalesced typing.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Steps in application order: (forward, inverse).
    pub steps: Vec<(Transaction, Transaction)>,
    /// Selections to restore after undoing this entry.
    pub sel_before: SelectionSet,
    /// Selections to restore after redoing this entry.
    pub sel_after: SelectionSet,
    /// Kind of the edits in `steps`; all steps share it.
    pub kind: EditKind,
    /// When the last step was recorded, for the coalescing window.
    pub at: Instant,
    /// Revision before the first step.
    pub rev_before: u64,
    /// Revision after the last step.
    pub rev_after: u64,
}

impl HistoryEntry {
    /// Approximate memory held: stored text plus a flat allowance for the
    /// struct and selection vectors.
    fn bytes(&self) -> usize {
        self.steps.iter().map(|(a, b)| a.text_bytes() + b.text_bytes()).sum::<usize>() + 64
    }
}

/// Linear undo/redo stacks for one document.
#[derive(Debug)]
pub struct History {
    /// Oldest first; `last()` is the next entry to undo.
    undo: Vec<HistoryEntry>,
    /// Entries undone since the last new edit; `last()` is the next to redo.
    redo: Vec<HistoryEntry>,
    /// Hard cap on entries.
    max_entries: usize,
    /// Approximate cap on retained transaction text.
    max_bytes: usize,
    /// Typing within this window joins the previous typing entry.
    group_window: Duration,
    /// Explicit group boundary requested (e.g. caret moved).
    boundary: bool,
    /// Running total of `HistoryEntry::bytes` over `undo` (redo is not counted).
    bytes: usize,
}

impl Default for History {
    /// 10,000 entries or 64 MiB of text, whichever is hit first.
    fn default() -> Self {
        Self::new(10_000, 64 * 1024 * 1024)
    }
}

impl History {
    /// Create an empty history; limits below 1 entry / 1 KiB are raised to those floors.
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1024),
            group_window: Duration::from_millis(1000),
            boundary: false,
            bytes: 0,
        }
    }

    /// True when there is something to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when there is something to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of undo entries (coalesced runs count once).
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Force the next edit to start a fresh entry even if it would coalesce.
    pub fn mark_boundary(&mut self) {
        self.boundary = true;
    }

    /// Record an applied step. Coalesces with the previous entry when both are
    /// continuous typing/deleting of the same kind within the group window and
    /// the caret count is unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        forward: Transaction,
        inverse: Transaction,
        sel_before: &SelectionSet,
        sel_after: &SelectionSet,
        kind: EditKind,
        rev_before: u64,
        rev_after: u64,
    ) {
        // Linear history: a new edit invalidates the redo stack.
        self.redo.clear();
        let now = Instant::now();
        // Merge only if the previous entry's end state is exactly this
        // edit's start state (same caret positions and count); otherwise an
        // intervening caret move would be hidden inside the group.
        let can_merge = !self.boundary
            && kind.coalesces()
            && self.undo.last().is_some_and(|e| {
                e.kind == kind && now.duration_since(e.at) < self.group_window && e.sel_after == *sel_before && e.sel_after.len() == sel_after.len()
            });
        self.boundary = false;
        let step_bytes = forward.text_bytes() + inverse.text_bytes();
        if can_merge {
            let e = self.undo.last_mut().expect("checked");
            e.steps.push((forward, inverse));
            e.sel_after = sel_after.clone();
            e.rev_after = rev_after;
            e.at = now;
            self.bytes += step_bytes;
        } else {
            let e = HistoryEntry {
                steps: vec![(forward, inverse)],
                sel_before: sel_before.clone(),
                sel_after: sel_after.clone(),
                kind,
                at: now,
                rev_before,
                rev_after,
            };
            self.bytes += e.bytes();
            self.undo.push(e);
        }
        self.trim();
    }

    /// Drop oldest entries until both limits hold. Always keeps at least one
    /// entry even if it alone exceeds the byte budget, so a huge paste is
    /// still undoable.
    fn trim(&mut self) {
        while self.undo.len() > self.max_entries || (self.bytes > self.max_bytes && self.undo.len() > 1) {
            let e = self.undo.remove(0);
            self.bytes = self.bytes.saturating_sub(e.bytes());
        }
    }

    /// Pop the entry to undo. The caller applies each inverse in reverse order.
    ///
    /// Sets a boundary so typing after an undo starts a new entry.
    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        let e = self.undo.pop()?;
        self.bytes = self.bytes.saturating_sub(e.bytes());
        self.boundary = true;
        Some(e)
    }

    /// Park an undone entry so it can be redone.
    pub fn push_redo(&mut self, e: HistoryEntry) {
        self.redo.push(e);
    }

    /// Pop the entry to redo. The caller applies each forward step in order.
    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.boundary = true;
        self.redo.pop()
    }

    /// Put a redone entry back on the undo stack (never coalesces).
    pub fn push_undo_entry(&mut self, e: HistoryEntry) {
        self.bytes += e.bytes();
        self.undo.push(e);
        self.trim();
    }

    /// Forget everything (used when a document is reloaded or converted).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    /// Approximate bytes retained by the undo stack, for diagnostics.
    pub fn approx_bytes(&self) -> usize {
        self.bytes
    }
}
