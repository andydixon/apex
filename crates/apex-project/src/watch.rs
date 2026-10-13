//! Filesystem watching with coalescing.
//!
//! Wraps a `notify` watcher so the UI thread can ask "what changed?" once per
//! frame without blocking. The watcher's callback runs on notify's own
//! thread and only pushes raw events into an unbounded channel; `FsWatcher::poll`
//! drains that channel on the caller's thread, merges the paths into one
//! `Changes` set, and hands the set over only after the filesystem has been
//! quiet for a debounce interval. That turns a burst of events (an editor
//! save, a `git checkout`, a build) into a single refresh.
//!
//! The app uses this to reload open files changed on disk, to refresh the
//! sidebar tree and the file index, and to re-read git status.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// A coalesced set of changed paths.
#[derive(Debug, Default)]
pub struct Changes {
    /// Every path notify reported since the last delivered set. Paths are
    /// as reported by the OS (may be absolute or relative to the watch root
    /// depending on the backend) and may no longer exist.
    pub paths: HashSet<PathBuf>,
    /// Watcher reported an overflow; callers should refresh everything.
    pub overflow: bool,
}

/// Owner of the platform watcher plus the debounce state.
///
/// Not `Clone`; there is one per app. Dropping it stops watching.
pub struct FsWatcher {
    /// The platform backend (inotify, FSEvents, ReadDirectoryChangesW).
    watcher: RecommendedWatcher,
    /// Raw events from the backend thread, drained by `poll`.
    rx: Receiver<notify::Result<notify::Event>>,
    /// Roots currently registered with the backend, so repeated `watch`
    /// calls for the same path are no-ops.
    watched: HashSet<PathBuf>,
    /// Changes accumulated since the last delivery.
    pending: Changes,
    /// Time of the most recent raw event; `None` once the pending set has
    /// been delivered.
    last_event: Option<Instant>,
}

impl FsWatcher {
    /// Create the backend watcher with no roots. Fails only if the platform
    /// backend cannot be initialised (e.g. inotify instance limit).
    pub fn new() -> notify::Result<Self> {
        let (tx, rx) = unbounded();
        // The callback runs on notify's thread; it must not block, so it just
        // forwards into the channel. A send error means we were dropped.
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        Ok(Self { watcher, rx, watched: HashSet::new(), pending: Changes::default(), last_event: None })
    }

    /// Start watching `path` recursively. Idempotent; failures (missing path,
    /// permission, inotify watch limit) are logged and otherwise ignored, since
    /// a workspace that cannot be watched is still usable.
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

    /// Stop watching `path` if it was registered through `watch`.
    pub fn unwatch(&mut self, path: &Path) {
        if self.watched.remove(path) {
            let _ = self.watcher.unwatch(path);
        }
    }

    /// Pull events and return the coalesced set once things have been quiet
    /// for `debounce`.
    ///
    /// Call this every frame. Returns `None` while there is nothing pending or
    /// events are still arriving; the debounce timer restarts on every event,
    /// so a continuous stream (a long build writing files) delays delivery
    /// until it pauses.
    pub fn poll(&mut self, debounce: Duration) -> Option<Changes> {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ok(ev) => {
                    // The backend's event queue overflowed: individual paths
                    // were lost, so tell the caller to rescan everything.
                    if matches!(ev.kind, notify::EventKind::Other) && ev.need_rescan() {
                        self.pending.overflow = true;
                    }
                    for p in ev.paths {
                        self.pending.paths.insert(p);
                    }
                    self.last_event = Some(Instant::now());
                }
                Err(e) => {
                    // Treat backend errors like overflow: we cannot know what
                    // was missed, so a full refresh is the safe response.
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

    /// True while events have arrived but not yet been delivered. The UI uses
    /// this to keep repainting so the debounce timer is checked promptly.
    pub fn has_pending(&self) -> bool {
        self.last_event.is_some()
    }
}
