//! Background file index for workspace roots (Goto Anything, Find in Files).
//!
//! The walk respects `.gitignore`, the configured exclude globs and the hidden
//! files setting. Results stream in batches so the UI can show partial
//! results while large trees are still being scanned. Each scan has a
//! generation number; results from an older generation are discarded.
//!
//! Flow: the UI calls `start_scan`, which spawns one `apex-index` thread and
//! returns an `IndexScan` handle. The thread walks every root with the
//! `ignore` crate and sends `IndexEvent`s over a bounded channel. Each frame
//! the UI calls `FileIndex::poll` with the handle to absorb what has arrived.
//! Dropping the handle cancels the thread at its next file. The same
//! `walker` builder is reused by `search.rs` so Find in Files sees exactly
//! the files Goto Anything does.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, bounded};

/// One regular file found by a scan.
#[derive(Debug, Clone)]
pub struct IndexedFile {
    /// Absolute path as produced by the walker.
    pub path: PathBuf,
    /// Path relative to its root, with `/` separators, for display and matching.
    pub rel: Arc<str>,
    /// Index into the `roots` list passed to `start_scan`.
    pub root: usize,
}

/// Filtering options for a scan; mirrors the user's workspace settings.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Glob patterns to exclude (gitignore syntax; a leading `!` re-includes).
    pub exclude: Vec<String>,
    /// Include dot-files and dot-directories.
    pub hidden: bool,
    /// Follow symbolic links while walking.
    pub follow_symlinks: bool,
    /// Honour `.gitignore`, the global gitignore and `.git/info/exclude`.
    pub respect_gitignore: bool,
    /// Stop after this many files to bound memory on pathological trees.
    pub max_files: usize,
}

impl Default for WalkOptions {
    /// Gitignore on, hidden off, no symlinks, two million file cap.
    fn default() -> Self {
        Self { exclude: Vec::new(), hidden: false, follow_symlinks: false, respect_gitignore: true, max_files: 2_000_000 }
    }
}

/// Message from the scan thread to the owner of an `IndexScan`.
pub enum IndexEvent {
    /// Up to 512 newly discovered files.
    Batch(Vec<IndexedFile>),
    /// The walk finished (or hit `max_files`); no more batches follow.
    Done { files: usize, elapsed: std::time::Duration },
}

/// Handle to a running or finished scan.
pub struct IndexScan {
    /// Monotonic id so a `FileIndex` can tell a new scan from the one it is
    /// already absorbing.
    pub generation: u64,
    /// Events from the scan thread.
    pub rx: Receiver<IndexEvent>,
    /// Shared with the thread; set by `cancel` and on drop.
    cancel: Arc<AtomicBool>,
}

impl IndexScan {
    /// Ask the scan thread to stop. It exits at the next directory entry.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for IndexScan {
    /// Dropping the handle cancels the scan so a replaced scan does not keep
    /// walking (and blocking on a full channel nobody reads).
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Source of `IndexScan::generation`; starts at 1 so a default `FileIndex`
/// (generation 0) always resets on its first poll.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Build an `ignore` walker for one root.
///
/// Shared with `search.rs`. Always honours `.ignore` files and parent
/// directories' ignore rules; git rules are controlled by
/// `respect_gitignore`.
pub fn walker(root: &Path, opts: &WalkOptions) -> ignore::WalkBuilder {
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(!opts.hidden)
        .follow_links(opts.follow_symlinks)
        .git_ignore(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .ignore(true)
        .parents(true);
    let mut ov = ignore::overrides::OverrideBuilder::new(root);
    for g in &opts.exclude {
        // In override syntax a leading `!` means "ignore".
        let pattern = if let Some(stripped) = g.strip_prefix('!') { stripped.to_string() } else { format!("!{g}") };
        if let Err(e) = ov.add(&pattern) {
            tracing::warn!("bad exclude glob {g:?}: {e}");
        }
    }
    if let Ok(o) = ov.build() {
        b.overrides(o);
    }
    b
}

/// Start scanning `roots` on a worker thread.
///
/// Returns immediately. If the thread cannot be spawned the error is logged
/// and the returned handle's channel simply closes, so pollers see no events.
pub fn start_scan(roots: Vec<PathBuf>, opts: WalkOptions) -> IndexScan {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    // Bounded so a fast walker cannot pile up unbounded memory while the UI
    // is busy; the thread blocks on `send` instead.
    let (tx, rx) = bounded::<IndexEvent>(64);
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("apex-index".into())
        .spawn(move || scan_thread(roots, opts, tx, c))
        .map_err(|e| tracing::error!("could not spawn index thread: {e}"))
        .ok();
    IndexScan { generation, rx, cancel }
}

/// Body of the `apex-index` thread: walk each root in order, batching files
/// into 512-entry chunks. Exits early on cancel, on a closed channel (the
/// handle was dropped) or when `max_files` is reached.
fn scan_thread(roots: Vec<PathBuf>, opts: WalkOptions, tx: Sender<IndexEvent>, cancel: Arc<AtomicBool>) {
    let start = Instant::now();
    let mut batch = Vec::with_capacity(512);
    let mut total = 0usize;
    'roots: for (ri, root) in roots.iter().enumerate() {
        let root = root.clone();
        for entry in walker(&root, &opts).build() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Ok(entry) = entry else { continue };
            // Depth 0 is the root itself; directories and other non-files are
            // not indexed.
            if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            // Normalise to `/` so matching and display are the same on Windows.
            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path()).to_string_lossy().replace('\\', "/");
            batch.push(IndexedFile { path: entry.into_path(), rel: rel.into(), root: ri });
            total += 1;
            if batch.len() >= 512 {
                if tx.send(IndexEvent::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                batch = Vec::with_capacity(512);
            }
            if total >= opts.max_files {
                tracing::warn!("file index capped at {} entries", opts.max_files);
                break 'roots;
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(IndexEvent::Batch(batch));
    }
    let _ = tx.send(IndexEvent::Done { files: total, elapsed: start.elapsed() });
}

/// In-memory index that absorbs scan events.
#[derive(Default)]
pub struct FileIndex {
    /// Files seen so far in the current generation, in walk order.
    pub files: Vec<IndexedFile>,
    /// True once `IndexEvent::Done` has been received for this generation.
    pub complete: bool,
    /// Generation of the scan whose results `files` holds.
    pub generation: u64,
    /// Wall time of the last completed scan, for the status bar.
    pub last_scan: Option<std::time::Duration>,
}

impl FileIndex {
    /// Drain available events. Returns true if the index changed.
    ///
    /// When `scan` belongs to a newer generation than the index, the old
    /// contents are discarded first so stale files never mix with new ones.
    pub fn poll(&mut self, scan: &IndexScan) -> bool {
        if scan.generation != self.generation {
            self.files.clear();
            self.complete = false;
            self.generation = scan.generation;
        }
        let mut changed = false;
        while let Ok(ev) = scan.rx.try_recv() {
            match ev {
                IndexEvent::Batch(b) => {
                    self.files.extend(b);
                    changed = true;
                }
                IndexEvent::Done { elapsed, .. } => {
                    self.complete = true;
                    self.last_scan = Some(elapsed);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Drop one file (e.g. after a delete in the sidebar) without rescanning.
    pub fn remove_path(&mut self, path: &Path) {
        self.files.retain(|f| f.path != path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gitignored, excluded and hidden entries are all absent from a scan.
    #[test]
    fn scans_respecting_gitignore_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(root.join("target/debug/out"), "x").unwrap();
        std::fs::write(root.join("node_modules/x/i.js"), "x").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join(".hidden"), "x").unwrap();
        // A .gitignore only applies inside a git repository.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let scan = start_scan(vec![root.to_path_buf()], WalkOptions { exclude: vec!["**/node_modules".into()], ..Default::default() });
        let mut idx = FileIndex::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !idx.complete && Instant::now() < deadline {
            idx.poll(&scan);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut rels: Vec<&str> = idx.files.iter().map(|f| &*f.rel).collect();
        rels.sort();
        // `.gitignore` itself is hidden and hidden files are off.
        assert_eq!(rels, vec!["src/main.rs"]);
    }
}
