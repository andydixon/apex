//! Panes, tabs and the split layout tree.
//!
//! This module is pure data: it knows nothing about egui or documents. `ApexApp`
//! owns one `Layout` tree plus a `PaneId → Pane` map, and `workspace_ui.rs`,
//! `app.rs` and the session code manipulate them through the methods here.
//! Each `Pane` holds an ordered list of `ViewId` tabs; a `ViewId` appears in at
//! most one pane. The `Layout` tree only stores pane ids, so the two structures
//! must be kept in step: every leaf must have a `Pane`, and removing a pane
//! means both `Layout::remove_leaf` and dropping the map entry.
//!
//! Fractions in a split are relative weights that always sum to 1; the two
//! mutating methods preserve that invariant.

use std::collections::HashSet;

use crate::view::ViewId;

/// Identifier of a pane (a tab strip plus editor area). Allocated by
/// `ApexApp::new_pane` from a counter and never reused within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u64);

/// Direction along which a split lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side.
    Horizontal,
    /// Children stacked.
    Vertical,
}

/// Recursive split layout. Fractions are relative weights that always sum to 1.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    /// A single pane.
    Leaf(PaneId),
    /// Two or more children along `axis`; `fractions` has one weight per child.
    Split { axis: Axis, children: Vec<Layout>, fractions: Vec<f32> },
}

impl Layout {
    /// Append every pane id in the tree to `out`, in depth-first (visual) order.
    pub fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Leaf(p) => out.push(*p),
            Layout::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    /// All pane ids in visual order (left to right, top to bottom).
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.leaves(&mut v);
        v
    }

    /// Replace the leaf `target` with a split containing it and `new`.
    ///
    /// Returns false if `target` is not in the tree. When `target` already sits in
    /// a split along the same axis, `new` is inserted as its sibling and takes
    /// half of `target`'s weight, so the tree stays flat and the sum stays 1.
    pub fn split_leaf(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            Layout::Leaf(p) if *p == target => {
                *self = Layout::Split { axis, children: vec![Layout::Leaf(target), Layout::Leaf(new)], fractions: vec![0.5, 0.5] };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { axis: a, children, fractions } => {
                // Same axis: insert as a sibling to keep the tree flat.
                if let Some(i) = children.iter().position(|c| *c == Layout::Leaf(target))
                    && *a == axis
                {
                    let f = fractions[i] / 2.0;
                    fractions[i] = f;
                    fractions.insert(i + 1, f);
                    children.insert(i + 1, Layout::Leaf(new));
                    return true;
                }
                // Different axis (or target is deeper): recurse. `any` stops at the
                // first child that reports success.
                children.iter_mut().any(|c| c.split_leaf(target, new, axis))
            }
        }
    }

    /// Remove a leaf, collapsing single-child splits. Returns false if it was
    /// the only leaf.
    ///
    /// The removed child's weight is shared equally among its former siblings so
    /// the fractions still sum to 1. A split left with one child is replaced by
    /// that child, so the tree never contains a one-element split.
    pub fn remove_leaf(&mut self, target: PaneId) -> bool {
        match self {
            // A lone leaf cannot be removed: the caller must keep at least one pane.
            Layout::Leaf(_) => false,
            Layout::Split { children, fractions, .. } => {
                if let Some(i) = children.iter().position(|c| *c == Layout::Leaf(target)) {
                    children.remove(i);
                    let f = fractions.remove(i);
                    let n = fractions.len() as f32;
                    for x in fractions.iter_mut() {
                        *x += f / n;
                    }
                } else if !children.iter_mut().any(|c| c.remove_leaf(target)) {
                    return false;
                }
                // A nested split may have collapsed to a leaf above, or we may have
                // removed the penultimate child here: either way flatten.
                if children.len() == 1 {
                    *self = children.remove(0);
                }
                true
            }
        }
    }
}

/// One tab strip: the views shown in a pane and which of them is active.
#[derive(Debug, Clone, Default)]
pub struct Pane {
    /// Tabs in display order. A view belongs to at most one pane.
    pub tabs: Vec<ViewId>,
    /// Index into `tabs` of the visible tab; meaningless when `tabs` is empty.
    pub active: usize,
    /// Pinned tabs stay at the left of the strip and are skipped by close-all.
    pub pinned: HashSet<ViewId>,
    /// A preview tab is replaced by the next preview rather than accumulating.
    pub preview: Option<ViewId>,
}

impl Pane {
    /// The view shown in this pane, if it has any tabs.
    pub fn active_view(&self) -> Option<ViewId> {
        self.tabs.get(self.active).copied()
    }

    /// Make `v` the active tab if it is in this pane; otherwise do nothing.
    pub fn activate(&mut self, v: ViewId) {
        if let Some(i) = self.tabs.iter().position(|x| *x == v) {
            self.active = i;
        }
    }

    /// Remove tab `v`, clearing its pinned/preview status. Returns false if the
    /// tab was not in this pane. Keeps `active` pointing at a sensible neighbour.
    pub fn remove(&mut self, v: ViewId) -> bool {
        let Some(i) = self.tabs.iter().position(|x| *x == v) else { return false };
        self.tabs.remove(i);
        self.pinned.remove(&v);
        if self.preview == Some(v) {
            self.preview = None;
        }
        if self.active > i {
            // A tab to the left went away: keep pointing at the same view.
            self.active -= 1;
        } else if self.active == i {
            // Closing the active tab activates its left neighbour.
            self.active = i.saturating_sub(1);
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        true
    }

    /// Insert after the active tab (Sublime behaviour), respecting pinned tabs.
    ///
    /// The new tab becomes active. If the insertion point would land inside the
    /// run of pinned tabs at the front, it is pushed to just after them.
    pub fn insert_after_active(&mut self, v: ViewId) {
        let mut at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        let pinned_end = self.tabs.iter().take_while(|t| self.pinned.contains(t)).count();
        if at < pinned_end {
            at = pinned_end;
        }
        self.tabs.insert(at.min(self.tabs.len()), v);
        self.active = at.min(self.tabs.len() - 1);
    }

    /// Move the active tab `delta` places along the strip, wrapping at the ends.
    /// Swaps with the tab at the target position rather than rotating the strip.
    pub fn move_active(&mut self, delta: isize) {
        let n = self.tabs.len();
        if n < 2 {
            return;
        }
        let target = (self.active as isize + delta).rem_euclid(n as isize) as usize;
        self.tabs.swap(self.active, target);
        self.active = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splitting along one axis stays flat, fractions sum to 1, and removing
    /// leaves collapses back to a single leaf that cannot itself be removed.
    #[test]
    fn split_and_remove() {
        let (a, b, c) = (PaneId(1), PaneId(2), PaneId(3));
        let mut l = Layout::Leaf(a);
        assert!(l.split_leaf(a, b, Axis::Horizontal));
        assert!(l.split_leaf(b, c, Axis::Horizontal));
        assert_eq!(l.pane_ids(), vec![a, b, c]);
        if let Layout::Split { fractions, .. } = &l {
            assert!((fractions.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
        assert!(l.remove_leaf(b));
        assert!(l.remove_leaf(c));
        assert_eq!(l, Layout::Leaf(a));
        assert!(!l.remove_leaf(a));
    }

    /// Insert-after-active ordering, left-neighbour activation on close, and
    /// wrap-around tab moves.
    #[test]
    fn tabs() {
        let mut p = Pane::default();
        let (a, b, c) = (ViewId(1), ViewId(2), ViewId(3));
        p.insert_after_active(a);
        p.insert_after_active(b);
        p.activate(a);
        p.insert_after_active(c);
        assert_eq!(p.tabs, vec![a, c, b]);
        assert_eq!(p.active_view(), Some(c));
        assert!(p.remove(c));
        assert_eq!(p.active_view(), Some(a));
        p.move_active(1);
        assert_eq!(p.tabs, vec![b, a]);
    }
}
