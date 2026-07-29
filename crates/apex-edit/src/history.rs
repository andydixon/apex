//! Undo/redo history.
//!
//! Each entry is a list of (forward, inverse) transaction pairs so that
//! coalesced typing can be undone in one step without composing transactions.
//! Redo is linear: a new edit after an undo discards the redo stack. This is
//! the behaviour of Sublime Text and most editors, and it is documented in the
//! user guide as such.

use std::time::{Duration, Instant};

use crate::document::EditKind;
use crate::selection::SelectionSet;
use crate::transaction::Transaction;

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Steps in application order: (forward, inverse).
    pub steps: Vec<(Transaction, Transaction)>,
    pub sel_before: SelectionSet,
    pub sel_after: SelectionSet,
    pub kind: EditKind,
    pub at: Instant,
    /// Revision before the first step.
    pub rev_before: u64,
    /// Revision after the last step.
    pub rev_after: u64,
}

impl HistoryEntry {
    fn bytes(&self) -> usize {
        self.steps.iter().map(|(a, b)| a.text_bytes() + b.text_bytes()).sum::<usize>() + 64
    }
}

#[derive(Debug)]
pub struct History {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    /// Hard cap on entries.
    max_entries: usize,
    /// Approximate cap on retained transaction text.
    max_bytes: usize,
    /// Typing within this window joins the previous typing entry.
    group_window: Duration,
    /// Explicit group boundary requested (e.g. caret moved).
    boundary: bool,
    bytes: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(10_000, 64 * 1024 * 1024)
    }
}

impl History {
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

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

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
        self.redo.clear();
        let now = Instant::now();
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

    fn trim(&mut self) {
        while self.undo.len() > self.max_entries || (self.bytes > self.max_bytes && self.undo.len() > 1) {
            let e = self.undo.remove(0);
            self.bytes = self.bytes.saturating_sub(e.bytes());
        }
    }

    /// Pop the entry to undo. The caller applies each inverse in reverse order.
    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        let e = self.undo.pop()?;
        self.bytes = self.bytes.saturating_sub(e.bytes());
        self.boundary = true;
        Some(e)
    }

    pub fn push_redo(&mut self, e: HistoryEntry) {
        self.redo.push(e);
    }

    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.boundary = true;
        self.redo.pop()
    }

    pub fn push_undo_entry(&mut self, e: HistoryEntry) {
        self.bytes += e.bytes();
        self.undo.push(e);
        self.trim();
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    pub fn approx_bytes(&self) -> usize {
        self.bytes
    }
}
