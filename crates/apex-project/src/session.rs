//! Session persistence and crash recovery.
//!
//! * `session.json` — open files, layout, selections, scroll, recent lists.
//! * `recovery/<id>.json` — snapshots of unsaved buffers, written on a timer
//!   by the application when a document's revision changed since the last
//!   snapshot. Both live in a `0700` data directory and files are `0600`.
//!
//! The UI builds a `Session` from its live state every 30 seconds and on
//! exit, and reads one back at start-up. Recovery entries are written by the
//! same timer loop, removed when a document is saved or closed clean, and
//! listed at start-up to offer restoration. All writes go through
//! `fileio::save_atomic` so a crash mid-write never leaves a torn file.
//! Loading is deliberately forgiving: a session file that does not parse is
//! renamed aside and ignored, because a damaged session must never stop the
//! editor from starting. The structs here are the on-disk schema; add fields
//! with `#[serde(default)]` semantics (the `Session` struct already has it)
//! and bump `SESSION_VERSION` only for incompatible changes.

use std::path::{Path, PathBuf};

use apex_edit::SelectionSet;
use serde::{Deserialize, Serialize};

use crate::fileio;
use crate::paths;

/// One open tab as saved to disk.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TabState {
    /// File on disk; `None` for untitled buffers.
    pub path: Option<PathBuf>,
    /// For untitled buffers: the recovery id holding their text.
    pub recovery_id: Option<String>,
    /// Cursor(s) and selection(s) to restore.
    pub sels: SelectionSet,
    /// Scroll offset in points (x, y).
    pub scroll: (f32, f32),
    pub pinned: bool,
    /// Language override chosen by the user, if any; `None` means detect.
    pub language: Option<String>,
}

/// One editor pane: its tabs and which is active.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PaneState {
    pub tabs: Vec<TabState>,
    /// Index into `tabs`.
    pub active: usize,
}

/// The split tree. Serialised with a `kind` tag (`leaf` / `split`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutState {
    /// A single pane, by index into `WindowState::panes`.
    Leaf { pane: usize },
    /// Children side by side (`horizontal`) or stacked; `fractions` holds
    /// each child's share of the space and has the same length as `children`.
    Split { horizontal: bool, children: Vec<LayoutState>, fractions: Vec<f32> },
}

impl Default for LayoutState {
    /// A single pane (index 0).
    fn default() -> Self {
        Self::Leaf { pane: 0 }
    }
}

/// One top-level window.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WindowState {
    pub panes: Vec<PaneState>,
    pub layout: LayoutState,
    /// Index into `panes`.
    pub active_pane: usize,
    /// Logical size in points, if known.
    pub size: Option<(f32, f32)>,
    /// Logical position in points, if known.
    pub position: Option<(f32, f32)>,
    pub maximised: bool,
    pub sidebar_width: Option<f32>,
}

/// The whole persisted session. Every field defaults when missing so older
/// files keep loading after fields are added.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    /// Schema version; `save_to` always writes `SESSION_VERSION`.
    pub version: u32,
    pub windows: Vec<WindowState>,
    /// Workspace folders.
    pub roots: Vec<PathBuf>,
    /// The `.apex-project` file the roots came from, if any.
    pub project_file: Option<PathBuf>,
    pub recent_files: Vec<PathBuf>,
    pub recent_projects: Vec<PathBuf>,
    pub find_history: Vec<String>,
    pub replace_history: Vec<String>,
    pub theme: Option<String>,
    /// Font size steps relative to the configured size.
    pub font_zoom: i32,
}

/// Current on-disk schema version. Files with a higher version are ignored.
pub const SESSION_VERSION: u32 = 1;

/// `<data_dir>/session.json`.
fn session_path() -> PathBuf {
    paths::data_dir().join("session.json")
}

/// `<data_dir>/recovery`.
fn recovery_dir() -> PathBuf {
    paths::data_dir().join("recovery")
}

/// Atomic write of a file that may contain unsaved user text: ensures the
/// parent is `0700`, writes via a temp file, then tightens the file to `0600`
/// (Unix only; `save_atomic` copies the mode of any existing file first, so a
/// pre-existing permissive mode would otherwise be kept).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        paths::ensure_private_dir(parent)?;
    }
    fileio::save_atomic(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

impl Session {
    /// Load the default session file, if present and readable.
    pub fn load() -> Option<Session> {
        Self::load_from(&session_path())
    }

    /// Load from `path`. Returns `None` for a missing file, a newer schema
    /// version, or unparseable JSON; in the last case the file is renamed to
    /// `session.json.damaged` so the next save does not overwrite the
    /// evidence.
    pub fn load_from(path: &Path) -> Option<Session> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice::<Session>(&bytes) {
            Ok(s) if s.version <= SESSION_VERSION => Some(s),
            Ok(s) => {
                tracing::warn!("session file version {} is newer than supported; ignoring", s.version);
                None
            }
            Err(e) => {
                // A damaged session must never prevent start-up.
                tracing::warn!("session file unreadable ({e}); starting fresh");
                let _ = std::fs::rename(path, path.with_extension("json.damaged"));
                None
            }
        }
    }

    /// Save to the default session file.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&session_path())
    }

    /// Write pretty-printed JSON to `path` with the current schema version
    /// stamped in, as a private file (see `write_private`).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut s = self.clone();
        s.version = SESSION_VERSION;
        let bytes = serde_json::to_vec_pretty(&s).map_err(std::io::Error::other)?;
        write_private(path, &bytes)
    }
}

/// A snapshot of an unsaved buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEntry {
    /// Stable per-document id; also the file name (after sanitising).
    pub id: String,
    /// The document's file, or `None` for untitled buffers.
    pub path: Option<PathBuf>,
    /// Tab title to show in the recovery dialog.
    pub title: String,
    /// Full buffer text at snapshot time.
    pub text: String,
    /// Identity of the file on disk when the snapshot was taken, to warn if
    /// the file has since changed.
    pub disk_modified_unix: Option<u64>,
    /// When the snapshot was written (seconds since the Unix epoch).
    pub saved_at_unix: u64,
}

/// Seconds since the Unix epoch, or 0 if the clock is before 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl RecoveryEntry {
    /// Path of the snapshot for `id`. Only `[A-Za-z0-9-]` from the id is kept
    /// so an id can never escape the recovery directory or contain
    /// separators.
    fn file(id: &str) -> PathBuf {
        recovery_dir().join(format!("{}.json", id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>()))
    }

    /// Write (or overwrite) this snapshot as a private file.
    pub fn write(&self) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        write_private(&Self::file(&self.id), &bytes)
    }

    /// Delete the snapshot for `id`; a missing file is not an error.
    pub fn remove(id: &str) {
        let _ = std::fs::remove_file(Self::file(id));
    }

    /// All recoverable snapshots, newest first. Unreadable files are skipped.
    pub fn list() -> Vec<RecoveryEntry> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(recovery_dir()) {
            for e in rd.flatten() {
                if let Ok(bytes) = std::fs::read(e.path())
                    && let Ok(entry) = serde_json::from_slice::<RecoveryEntry>(&bytes)
                {
                    out.push(entry);
                }
            }
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.saved_at_unix));
        out
    }

    /// Cheap check for "is there anything to recover?" before paying for
    /// `list`: true if the recovery directory has at least one entry.
    pub fn exists_any() -> bool {
        std::fs::read_dir(recovery_dir()).map(|rd| rd.flatten().next().is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session survives a round trip, and corrupt JSON is moved aside.
    #[test]
    fn session_round_trip_and_damage_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let s = Session {
            windows: vec![WindowState {
                panes: vec![PaneState { tabs: vec![TabState { path: Some("/x/y.rs".into()), ..Default::default() }], active: 0 }],
                ..Default::default()
            }],
            roots: vec!["/x".into()],
            ..Default::default()
        };
        s.save_to(&p).unwrap();
        let back = Session::load_from(&p).unwrap();
        assert_eq!(back.windows[0].panes[0].tabs[0].path, Some(PathBuf::from("/x/y.rs")));
        assert_eq!(back.version, SESSION_VERSION);
        std::fs::write(&p, b"{ not json").unwrap();
        assert!(Session::load_from(&p).is_none());
        assert!(p.with_extension("json.damaged").exists());
    }

    /// The session file is `0600` and its directory `0700`.
    #[cfg(unix)]
    #[test]
    fn session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/session.json");
        Session::default().save_to(&p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(dir.path().join("sub")).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
