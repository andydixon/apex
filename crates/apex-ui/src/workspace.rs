//! Panes, tabs and the split layout tree.

use std::collections::HashSet;

use crate::view::ViewId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u64);

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
    Leaf(PaneId),
    Split { axis: Axis, children: Vec<Layout>, fractions: Vec<f32> },
}

impl Layout {
    pub fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Leaf(p) => out.push(*p),
            Layout::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.leaves(&mut v);
        v
    }

    /// Replace the leaf `target` with a split containing it and `new`.
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
                children.iter_mut().any(|c| c.split_leaf(target, new, axis))
            }
        }
    }

    /// Remove a leaf, collapsing single-child splits. Returns false if it was
    /// the only leaf.
    pub fn remove_leaf(&mut self, target: PaneId) -> bool {
        match self {
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
                if children.len() == 1 {
                    *self = children.remove(0);
                }
                true
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pane {
    pub tabs: Vec<ViewId>,
    pub active: usize,
    pub pinned: HashSet<ViewId>,
    /// A preview tab is replaced by the next preview rather than accumulating.
    pub preview: Option<ViewId>,
}

impl Pane {
    pub fn active_view(&self) -> Option<ViewId> {
        self.tabs.get(self.active).copied()
    }

    pub fn activate(&mut self, v: ViewId) {
        if let Some(i) = self.tabs.iter().position(|x| *x == v) {
            self.active = i;
        }
    }

    pub fn remove(&mut self, v: ViewId) -> bool {
        let Some(i) = self.tabs.iter().position(|x| *x == v) else { return false };
        self.tabs.remove(i);
        self.pinned.remove(&v);
        if self.preview == Some(v) {
            self.preview = None;
        }
        if self.active > i {
            self.active -= 1;
        } else if self.active == i {
            // Closing the active tab activates its left neighbour.
            self.active = i.saturating_sub(1);
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        true
    }

    /// Insert after the active tab (Sublime behaviour), respecting pinned tabs.
    pub fn insert_after_active(&mut self, v: ViewId) {
        let mut at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        let pinned_end = self.tabs.iter().take_while(|t| self.pinned.contains(t)).count();
        if at < pinned_end {
            at = pinned_end;
        }
        self.tabs.insert(at.min(self.tabs.len()), v);
        self.active = at.min(self.tabs.len() - 1);
    }

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
