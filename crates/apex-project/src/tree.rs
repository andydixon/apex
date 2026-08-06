//! Lazily loaded directory tree for the sidebar.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DirEntryNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// `None` until expanded; directories only.
    pub children: Option<Vec<DirEntryNode>>,
    pub expanded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TreeOptions {
    pub show_hidden: bool,
    pub exclude: Vec<String>,
    pub follow_symlinks: bool,
}

impl DirEntryNode {
    pub fn root(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self { path, name, is_dir: true, is_symlink: false, children: None, expanded: true, error: None }
    }

    /// Read the directory's children (dirs first, case-insensitive natural order).
    pub fn load(&mut self, opts: &TreeOptions) {
        if !self.is_dir {
            return;
        }
        let mut out = Vec::new();
        let globs = build_globs(&opts.exclude);
        match std::fs::read_dir(&self.path) {
            Ok(rd) => {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !opts.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    let Ok(ft) = entry.file_type() else { continue };
                    let is_symlink = ft.is_symlink();
                    let is_dir = if is_symlink { opts.follow_symlinks && std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false) } else { ft.is_dir() };
                    if globs.iter().any(|g| g.is_match(&path) || g.is_match(Path::new(&name))) {
                        continue;
                    }
                    out.push(DirEntryNode { path, name, is_dir, is_symlink, children: None, expanded: false, error: None });
                }
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e.to_string());
            }
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
        self.children = Some(out);
    }

    /// Reload this node and keep the expansion state of known children.
    pub fn refresh(&mut self, opts: &TreeOptions, visited: &mut HashSet<PathBuf>) {
        let old: Vec<DirEntryNode> = self.children.take().unwrap_or_default();
        self.load(opts);
        if let Some(children) = &mut self.children {
            for c in children.iter_mut() {
                if let Some(o) = old.iter().find(|o| o.path == c.path && o.is_dir) {
                    c.expanded = o.expanded;
                    if o.expanded && o.children.is_some() {
                        let canon = std::fs::canonicalize(&c.path).unwrap_or(c.path.clone());
                        if visited.insert(canon) {
                            c.children = o.children.clone();
                            c.refresh(opts, visited);
                        }
                    }
                }
            }
        }
    }

    /// Find a node by path (mutable).
    pub fn find_mut(&mut self, path: &Path) -> Option<&mut DirEntryNode> {
        if self.path == path {
            return Some(self);
        }
        if !path.starts_with(&self.path) {
            return None;
        }
        self.children.as_mut()?.iter_mut().find_map(|c| c.find_mut(path))
    }

    /// Expand every directory on the way to `path` (loading as needed).
    pub fn reveal(&mut self, path: &Path, opts: &TreeOptions) -> bool {
        if !path.starts_with(&self.path) {
            return false;
        }
        if self.path == path {
            return true;
        }
        if self.children.is_none() {
            self.load(opts);
        }
        self.expanded = true;
        if let Some(children) = &mut self.children {
            for c in children.iter_mut() {
                if path.starts_with(&c.path) {
                    return if c.is_dir { c.reveal(path, opts) } else { c.path == path };
                }
            }
        }
        false
    }
}

fn build_globs(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns.iter().filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher())).collect()
}

/// Case-insensitive comparison with embedded numbers compared numerically.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = 0u64;
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na = na.saturating_mul(10).saturating_add(c as u64 - '0' as u64);
                    ai.next();
                }
                let mut nb = 0u64;
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb = nb.saturating_mul(10).saturating_add(c as u64 - '0' as u64);
                    bi.next();
                }
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_lowercase().next().unwrap_or(x), y.to_lowercase().next().unwrap_or(y));
                if lx != ly {
                    return lx.cmp(&ly);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_sorted_and_reveals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("b/inner")).unwrap();
        std::fs::write(root.join("b/inner/f.txt"), "").unwrap();
        std::fs::write(root.join("a10.txt"), "").unwrap();
        std::fs::write(root.join("a2.txt"), "").unwrap();
        std::fs::write(root.join(".hidden"), "").unwrap();
        let mut node = DirEntryNode::root(root.to_path_buf());
        node.load(&TreeOptions::default());
        let names: Vec<&str> = node.children.as_ref().unwrap().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a2.txt", "a10.txt"]);
        assert!(node.reveal(&root.join("b/inner/f.txt"), &TreeOptions::default()));
        let b = node.find_mut(&root.join("b")).unwrap();
        assert!(b.expanded);
        assert!(b.find_mut(&root.join("b/inner")).unwrap().expanded);
    }
}
