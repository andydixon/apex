//! Lazily loaded directory tree for the sidebar.
//!
//! A `DirEntryNode` is one row in the file sidebar. The root is created with
//! `DirEntryNode::root`; directories read their children only when the UI
//! expands them (`load`), so opening a huge workspace costs one `read_dir`.
//! The sidebar calls `refresh` when the watcher reports changes (keeping
//! which folders are open), `reveal` for "show the current file in the
//! sidebar", and `find_mut` to locate a row by path.
//!
//! Filtering here (hidden files, exclude globs, symlink following) is
//! independent of the file index in `index.rs`, which uses the `ignore`
//! crate; the two should be kept in agreement by the settings layer.
//! Symlinked directories are only descended into when `follow_symlinks` is
//! set, and `refresh` tracks canonical paths to avoid looping through links.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One file or directory row in the sidebar tree.
#[derive(Debug, Clone)]
pub struct DirEntryNode {
    /// Full path of the entry (root path joined with each component).
    pub path: PathBuf,
    /// Display name: the final path component, lossily converted.
    pub name: String,
    /// True for directories (and, with `follow_symlinks`, symlinks to them).
    pub is_dir: bool,
    /// True when the entry itself is a symbolic link, whatever it points to.
    pub is_symlink: bool,
    /// `None` until expanded; directories only.
    pub children: Option<Vec<DirEntryNode>>,
    /// Whether the sidebar currently shows this directory open.
    pub expanded: bool,
    /// Error from the last `read_dir` of this directory, shown inline.
    pub error: Option<String>,
}

/// Filtering options applied when a directory is loaded.
#[derive(Debug, Clone, Default)]
pub struct TreeOptions {
    /// Include entries whose name starts with `.`.
    pub show_hidden: bool,
    /// Glob patterns (`globset` syntax) matched against both the full path
    /// and the bare name; a match hides the entry.
    pub exclude: Vec<String>,
    /// Treat symlinks to directories as directories (otherwise they appear
    /// as plain files and are never descended into).
    pub follow_symlinks: bool,
}

impl DirEntryNode {
    /// Make the root node for a workspace folder. It starts expanded but
    /// unloaded; call `load` to read its children.
    pub fn root(path: PathBuf) -> Self {
        // A root like `/` has no file name; fall back to the whole path.
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self { path, name, is_dir: true, is_symlink: false, children: None, expanded: true, error: None }
    }

    /// Read the directory's children (dirs first, case-insensitive natural order).
    ///
    /// Replaces `children` entirely (any previous expansion state of the
    /// children is lost; `refresh` preserves it). A failed `read_dir` leaves
    /// an empty child list and records the error in `self.error`. No-op for
    /// files.
    pub fn load(&mut self, opts: &TreeOptions) {
        if !self.is_dir {
            return;
        }
        let mut out = Vec::new();
        let globs = build_globs(&opts.exclude);
        match std::fs::read_dir(&self.path) {
            Ok(rd) => {
                // `flatten` drops entries that error individually (e.g. a
                // file deleted mid-iteration) rather than failing the whole
                // directory.
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !opts.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    let Ok(ft) = entry.file_type() else { continue };
                    let is_symlink = ft.is_symlink();
                    // `file_type()` does not follow links, so for a symlink we
                    // need a `metadata` call to find out whether the target
                    // is a directory, and only bother when following is on.
                    let is_dir = if is_symlink { opts.follow_symlinks && std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false) } else { ft.is_dir() };
                    // Match both the full path (for patterns like `**/target`)
                    // and the bare name (for patterns like `*.o`).
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
        // Directories before files, then natural order within each group.
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
        self.children = Some(out);
    }

    /// Reload this node and keep the expansion state of known children.
    ///
    /// `visited` holds canonical paths of directories already refreshed in
    /// this pass; it stops a symlink cycle (or the same directory reachable
    /// through two links) from recursing forever. Pass a fresh set per
    /// top-level call.
    pub fn refresh(&mut self, opts: &TreeOptions, visited: &mut HashSet<PathBuf>) {
        let old: Vec<DirEntryNode> = self.children.take().unwrap_or_default();
        self.load(opts);
        if let Some(children) = &mut self.children {
            for c in children.iter_mut() {
                if let Some(o) = old.iter().find(|o| o.path == c.path && o.is_dir) {
                    c.expanded = o.expanded;
                    // Only descend into folders the user had open and loaded;
                    // collapsed ones are reloaded lazily when next expanded.
                    if o.expanded && o.children.is_some() {
                        let canon = std::fs::canonicalize(&c.path).unwrap_or(c.path.clone());
                        if visited.insert(canon) {
                            // Hand the old subtree over so the recursive
                            // refresh can in turn preserve its grandchildren.
                            c.children = o.children.clone();
                            c.refresh(opts, visited);
                        }
                    }
                }
            }
        }
    }

    /// Find a node by path (mutable).
    ///
    /// Only searches loaded children; returns `None` for paths under
    /// directories that have never been expanded.
    pub fn find_mut(&mut self, path: &Path) -> Option<&mut DirEntryNode> {
        if self.path == path {
            return Some(self);
        }
        // Prune subtrees that cannot contain the path.
        if !path.starts_with(&self.path) {
            return None;
        }
        self.children.as_mut()?.iter_mut().find_map(|c| c.find_mut(path))
    }

    /// Expand every directory on the way to `path` (loading as needed).
    ///
    /// Returns true if the path was found (as a file or a directory). Returns
    /// false when it is outside this subtree or is filtered out by `opts`.
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
                    // A file child matches only exactly; `starts_with` would
                    // also accept `foo.txt/bar`.
                    return if c.is_dir { c.reveal(path, opts) } else { c.path == path };
                }
            }
        }
        false
    }
}

/// Compile exclude patterns, silently skipping any that do not parse so one
/// bad setting does not hide the whole tree.
fn build_globs(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns.iter().filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher())).collect()
}

/// Case-insensitive comparison with embedded numbers compared numerically.
///
/// `a2.txt` sorts before `a10.txt`. Digit runs are compared as `u64` (with
/// saturation, so absurdly long runs compare equal rather than overflow);
/// other characters compare by their lower-case form.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            // Both at a digit: consume the whole run on each side and compare
            // numerically.
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
                // Equal numbers (e.g. "7" vs "007"): keep going.
            }
            (Some(x), Some(y)) => {
                // `to_lowercase` can yield several chars; the first is enough
                // for ordering purposes.
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

    /// Hidden files are skipped, directories sort first, numbers sort
    /// naturally, and `reveal` expands the whole chain to a nested file.
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
