//! Session persistence and crash recovery.
//!
//! * `session.json` — open files, layout, selections, scroll, recent lists.
//! * `recovery/<id>.json` — snapshots of unsaved buffers, written on a timer
//!   by the application when a document's revision changed since the last
//!   snapshot. Both live in a `0700` data directory and files are `0600`.

use std::path::{Path, PathBuf};

use apex_edit::SelectionSet;
use serde::{Deserialize, Serialize};

use crate::fileio;
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TabState {
    pub path: Option<PathBuf>,
    /// For untitled buffers: the recovery id holding their text.
    pub recovery_id: Option<String>,
    pub sels: SelectionSet,
    pub scroll: (f32, f32),
    pub pinned: bool,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutState {
    Leaf { pane: usize },
    Split { horizontal: bool, children: Vec<LayoutState>, fractions: Vec<f32> },
}

impl Default for LayoutState {
    fn default() -> Self {
        Self::Leaf { pane: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WindowState {
    pub panes: Vec<PaneState>,
    pub layout: LayoutState,
    pub active_pane: usize,
    pub size: Option<(f32, f32)>,
    pub position: Option<(f32, f32)>,
    pub maximised: bool,
    pub sidebar_width: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    pub windows: Vec<WindowState>,
    pub roots: Vec<PathBuf>,
    pub project_file: Option<PathBuf>,
    pub recent_files: Vec<PathBuf>,
    pub recent_projects: Vec<PathBuf>,
    pub find_history: Vec<String>,
    pub replace_history: Vec<String>,
    pub theme: Option<String>,
    pub font_zoom: i32,
}

pub const SESSION_VERSION: u32 = 1;

fn session_path() -> PathBuf {
    paths::data_dir().join("session.json")
}

fn recovery_dir() -> PathBuf {
    paths::data_dir().join("recovery")
}

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
    pub fn load() -> Option<Session> {
        Self::load_from(&session_path())
    }

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

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&session_path())
    }

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
    pub id: String,
    pub path: Option<PathBuf>,
    pub title: String,
    pub text: String,
    /// Identity of the file on disk when the snapshot was taken, to warn if
    /// the file has since changed.
    pub disk_modified_unix: Option<u64>,
    pub saved_at_unix: u64,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl RecoveryEntry {
    fn file(id: &str) -> PathBuf {
        recovery_dir().join(format!("{}.json", id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>()))
    }

    pub fn write(&self) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        write_private(&Self::file(&self.id), &bytes)
    }

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

    pub fn exists_any() -> bool {
        std::fs::read_dir(recovery_dir()).map(|rd| rd.flatten().next().is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
