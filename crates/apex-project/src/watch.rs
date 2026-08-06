//! Filesystem watching with coalescing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// A coalesced set of changed paths.
#[derive(Debug, Default)]
pub struct Changes {
    pub paths: HashSet<PathBuf>,
    /// Watcher reported an overflow; callers should refresh everything.
    pub overflow: bool,
}

pub struct FsWatcher {
    watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    watched: HashSet<PathBuf>,
    pending: Changes,
    last_event: Option<Instant>,
}

impl FsWatcher {
    pub fn new() -> notify::Result<Self> {
        let (tx, rx) = unbounded();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        Ok(Self { watcher, rx, watched: HashSet::new(), pending: Changes::default(), last_event: None })
    }

    pub fn watch(&mut self, path: &Path) {
        if self.watched.contains(path) {
            return;
        }
        match self.watcher.watch(path, RecursiveMode::Recursive) {
            Ok(()) => {
                self.watched.insert(path.to_path_buf());
            }
            Err(e) => tracing::warn!("cannot watch {}: {e}", path.display()),
        }
    }

    pub fn unwatch(&mut self, path: &Path) {
        if self.watched.remove(path) {
            let _ = self.watcher.unwatch(path);
        }
    }

    /// Pull events and return the coalesced set once things have been quiet
    /// for `debounce`.
    pub fn poll(&mut self, debounce: Duration) -> Option<Changes> {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ok(ev) => {
                    if matches!(ev.kind, notify::EventKind::Other) && ev.need_rescan() {
                        self.pending.overflow = true;
                    }
                    for p in ev.paths {
                        self.pending.paths.insert(p);
                    }
                    self.last_event = Some(Instant::now());
                }
                Err(e) => {
                    tracing::debug!("watch error: {e}");
                    self.pending.overflow = true;
                    self.last_event = Some(Instant::now());
                }
            }
        }
        match self.last_event {
            Some(t) if t.elapsed() >= debounce && (!self.pending.paths.is_empty() || self.pending.overflow) => {
                self.last_event = None;
                Some(std::mem::take(&mut self.pending))
            }
            _ => None,
        }
    }

    pub fn has_pending(&self) -> bool {
        self.last_event.is_some()
    }
}
