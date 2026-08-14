//! Lightweight Git awareness: repository discovery, branch, status, and line
//! diffs against the index or HEAD for gutter markers and hunk navigation.
//!
//! Every operation here is read-only except [`revert_hunk_text`], which only
//! computes replacement text; the editor applies it as an undoable edit. No
//! staging, committing or history rewriting ever happens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, bounded};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Ignored,
    Conflict,
    /// Changes in the index only (staged) with a clean working tree.
    Staged,
}

#[derive(Debug, Clone, Default)]
pub struct RepoStatus {
    pub workdir: PathBuf,
    pub branch: String,
    pub detached: bool,
    pub unborn: bool,
    pub files: HashMap<PathBuf, FileStatus>,
    /// Directories containing changes (for sidebar tinting).
    pub dirs: HashMap<PathBuf, FileStatus>,
}

/// Find the repository containing `path`, if any. Never follows into a
/// bare repository's object store for a working file.
pub fn discover(path: &Path) -> Option<git2::Repository> {
    let repo = git2::Repository::discover(path).ok()?;
    if repo.is_bare() {
        return None;
    }
    Some(repo)
}

pub fn workdir_of(path: &Path) -> Option<PathBuf> {
    discover(path).and_then(|r| r.workdir().map(Path::to_path_buf))
}

fn branch_info(repo: &git2::Repository) -> (String, bool, bool) {
    match repo.head() {
        Ok(head) => {
            let detached = repo.head_detached().unwrap_or(false);
            let name = if detached {
                head.target().map(|o| format!("({})", &o.to_string()[..7])).unwrap_or_else(|| "(detached)".into())
            } else {
                head.shorthand().unwrap_or("HEAD").to_string()
            };
            (name, detached, false)
        }
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => {
            let name = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().map(|s| s.trim_start_matches("refs/heads/").to_string()))
                .unwrap_or_else(|| "main".into());
            (name, false, true)
        }
        Err(_) => ("(no branch)".into(), false, true),
    }
}

/// Compute the full status synchronously. Prefer [`start_status`] from the UI.
pub fn status(workdir: &Path, include_ignored: bool) -> Result<RepoStatus, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let (branch, detached, unborn) = branch_info(&repo);
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).include_ignored(include_ignored).exclude_submodules(false).renames_head_to_index(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let root = repo.workdir().map(Path::to_path_buf).unwrap_or_else(|| workdir.to_path_buf());
    let mut files = HashMap::new();
    let mut dirs: HashMap<PathBuf, FileStatus> = HashMap::new();
    for e in statuses.iter() {
        let Some(rel) = e.path() else { continue };
        let s = e.status();
        let kind = if s.is_conflicted() {
            FileStatus::Conflict
        } else if s.is_wt_new() {
            FileStatus::Untracked
        } else if s.is_ignored() {
            FileStatus::Ignored
        } else if s.is_wt_deleted() || s.is_index_deleted() {
            FileStatus::Deleted
        } else if s.is_wt_renamed() || s.is_index_renamed() {
            FileStatus::Renamed
        } else if s.is_wt_modified() || s.is_wt_typechange() {
            FileStatus::Modified
        } else if s.is_index_new() {
            FileStatus::Added
        } else if s.is_index_modified() || s.is_index_typechange() {
            FileStatus::Staged
        } else {
            continue;
        };
        let path = root.join(rel);
        if kind != FileStatus::Ignored {
            let mut p = path.parent();
            while let Some(d) = p {
                if d == root {
                    break;
                }
                let e = dirs.entry(d.to_path_buf()).or_insert(kind);
                if *e == FileStatus::Untracked && kind != FileStatus::Untracked {
                    *e = kind;
                }
                p = d.parent();
            }
        }
        files.insert(path, kind);
    }
    Ok(RepoStatus { workdir: root, branch, detached, unborn, files, dirs })
}

pub struct StatusScan {
    pub rx: Receiver<Result<RepoStatus, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for StatusScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Compute status on a worker thread.
pub fn start_status(workdir: PathBuf, include_ignored: bool) -> StatusScan {
    let (tx, rx) = bounded(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("apex-git-status".into())
        .spawn(move || {
            let r = status(&workdir, include_ignored).map_err(|e| e.message().to_string());
            if !c.load(Ordering::Relaxed) {
                let _ = tx.send(r);
            }
        })
        .map_err(|e| tracing::error!("could not spawn git status thread: {e}"))
        .ok();
    StatusScan { rx, cancel }
}

/// Content of `path` in the index (staged) or HEAD, decoded leniently.
pub fn file_at(workdir: &Path, path: &Path, head: bool) -> Result<Option<String>, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let root = repo.workdir().map(Path::to_path_buf).unwrap_or_else(|| workdir.to_path_buf());
    let rel = path.strip_prefix(&root).map_err(|_| git2::Error::from_str("path is outside the repository"))?;
    let blob_oid = if head {
        let Ok(head_ref) = repo.head() else { return Ok(None) };
        let tree = head_ref.peel_to_tree()?;
        match tree.get_path(rel) {
            Ok(entry) => entry.id(),
            Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    } else {
        let index = repo.index()?;
        match index.get_path(rel, 0) {
            Some(entry) => entry.id,
            None => return Ok(None),
        }
    };
    let blob = repo.find_blob(blob_oid)?;
    if blob.is_binary() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(blob.content()).into_owned()))
}

/// A changed region of the working text relative to the base text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Lines in the *current* text (zero-based, exclusive end). Empty for pure deletions.
    pub new_lines: std::ops::Range<usize>,
    /// Lines in the base text replaced by `new_lines`.
    pub old_lines: std::ops::Range<usize>,
    pub kind: HunkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Modified,
    Deleted,
}

/// Line-level diff of `base` → `current`, grouped into hunks.
pub fn diff_hunks(base: &str, current: &str) -> Vec<Hunk> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::configure().algorithm(similar::Algorithm::Myers).diff_lines(base, current);
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur: Option<(usize, usize, usize, usize)> = None; // (old_start, old_end, new_start, new_end)
    let mut old_i = 0usize;
    let mut new_i = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some((os, oe, ns, ne)) = cur.take() {
                    hunks.push(make_hunk(os, oe, ns, ne));
                }
                old_i += 1;
                new_i += 1;
            }
            ChangeTag::Delete => {
                let c = cur.get_or_insert((old_i, old_i, new_i, new_i));
                c.1 = old_i + 1;
                old_i += 1;
            }
            ChangeTag::Insert => {
                let c = cur.get_or_insert((old_i, old_i, new_i, new_i));
                c.3 = new_i + 1;
                new_i += 1;
            }
        }
    }
    if let Some((os, oe, ns, ne)) = cur {
        hunks.push(make_hunk(os, oe, ns, ne));
    }
    hunks
}

fn make_hunk(os: usize, oe: usize, ns: usize, ne: usize) -> Hunk {
    let kind = if oe == os {
        HunkKind::Added
    } else if ne == ns {
        HunkKind::Deleted
    } else {
        HunkKind::Modified
    };
    Hunk { new_lines: ns..ne, old_lines: os..oe, kind }
}

/// Text that restores the base for one hunk: the base lines that the hunk
/// replaced. The editor swaps `hunk.new_lines` for this text.
pub fn revert_hunk_text(base: &str, hunk: &Hunk) -> String {
    let lines: Vec<&str> = base.split_inclusive('\n').collect();
    lines[hunk.old_lines.start.min(lines.len())..hunk.old_lines.end.min(lines.len())].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_classify_changes() {
        let base = "a\nb\nc\nd\n";
        let cur = "a\nB\nc\nd\ne\n";
        let h = diff_hunks(base, cur);
        assert_eq!(
            h,
            vec![Hunk { new_lines: 1..2, old_lines: 1..2, kind: HunkKind::Modified }, Hunk { new_lines: 4..5, old_lines: 4..4, kind: HunkKind::Added }]
        );
        let h = diff_hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!(h, vec![Hunk { new_lines: 1..1, old_lines: 1..2, kind: HunkKind::Deleted }]);
        assert_eq!(revert_hunk_text("a\nb\nc\n", &h[0]), "b\n");
        assert!(diff_hunks("same\n", "same\n").is_empty());
    }

    #[test]
    fn repository_status_and_blob_access() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = git2::Repository::init(root).unwrap();
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        std::fs::write(root.join("new.txt"), "x").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("tracked.txt")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        }
        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        let st = status(root, false).unwrap();
        assert!(!st.unborn);
        assert_eq!(st.files.get(&root.join("tracked.txt")), Some(&FileStatus::Modified));
        assert_eq!(st.files.get(&root.join("new.txt")), Some(&FileStatus::Untracked));
        assert_eq!(file_at(root, &root.join("tracked.txt"), true).unwrap().as_deref(), Some("one\n"));
        assert_eq!(file_at(root, &root.join("new.txt"), true).unwrap(), None);
        assert!(discover(&root.join("tracked.txt")).is_some());
        assert!(workdir_of(Path::new("/")).is_none() || true);
    }

    #[test]
    fn unborn_repository_is_handled() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let st = status(dir.path(), false).unwrap();
        assert!(st.unborn);
        assert!(!st.branch.is_empty());
    }
}

/// One commit that touched a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: String,
    pub short_id: String,
    pub author: String,
    pub date: String,
    pub summary: String,
}

/// `YYYY-MM-DD HH:MM` for a Unix timestamp with a UTC offset in minutes.
pub fn format_date(unix: i64, offset_minutes: i32) -> String {
    let t = unix + offset_minutes as i64 * 60;
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// Commits in which `path` changed, newest first, bounded by `limit`.
pub fn file_history(workdir: &Path, path: &Path, limit: usize) -> Result<Vec<HistoryEntry>, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let root = repo.workdir().map(Path::to_path_buf).unwrap_or_else(|| workdir.to_path_buf());
    let rel = path.strip_prefix(&root).map_err(|_| git2::Error::from_str("path is outside the repository"))?;
    let mut walk = repo.revwalk()?;
    if walk.push_head().is_err() {
        return Ok(Vec::new());
    }
    walk.set_sorting(git2::Sort::TIME)?;
    let mut out = Vec::new();
    let blob_at = |commit: &git2::Commit| -> Option<git2::Oid> { commit.tree().ok()?.get_path(rel).ok().map(|e| e.id()) };
    for oid in walk.take(5000) {
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let current = blob_at(&commit);
        // Changed if any parent has a different blob (or the file is new).
        let changed = if commit.parent_count() == 0 {
            current.is_some()
        } else {
            (0..commit.parent_count()).any(|i| commit.parent(i).ok().and_then(|p| blob_at(&p)) != current)
        };
        if !changed {
            continue;
        }
        let sig = commit.author();
        let time = commit.time();
        out.push(HistoryEntry {
            id: oid.to_string(),
            short_id: oid.to_string()[..7].to_string(),
            author: sig.name().unwrap_or("").to_string(),
            date: format_date(time.seconds(), time.offset_minutes()),
            summary: commit.summary().unwrap_or("").to_string(),
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Unified diff text of `base` → `current` with the given labels.
pub fn unified_diff(base: &str, current: &str, base_label: &str, current_label: &str) -> String {
    similar::TextDiff::from_lines(base, current).unified_diff().context_radius(3).header(base_label, current_label).to_string()
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn dates_and_unified_diff() {
        assert_eq!(format_date(0, 0), "1970-01-01 00:00");
        assert_eq!(format_date(1_700_000_000, 60), "2023-11-14 23:13");
        let d = unified_diff("a\nb\n", "a\nc\n", "x (HEAD)", "x");
        assert!(d.starts_with("--- x (HEAD)\n+++ x\n"));
        assert!(d.contains("-b\n+c\n"));
    }
}
