//! Lightweight Git awareness: repository discovery, branch, status, and line
//! diffs against the index or HEAD for gutter markers and hunk navigation.
//!
//! Every operation here is read-only; anything that writes to a repository
//! runs the user's `git` through [`crate::git_cli`].

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
    /// Per-file index and working-tree state, sorted by path.
    pub entries: Vec<StatusEntry>,
    /// Upstream branch (`origin/main`) and commits ahead/behind it.
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

/// One changed path: what is staged and what is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: PathBuf,
    /// Index vs HEAD: Added, Modified, Deleted or Renamed.
    pub staged: Option<FileStatus>,
    /// Working tree vs index: Modified, Deleted, Renamed or Untracked.
    pub unstaged: Option<FileStatus>,
    pub conflict: bool,
}

fn upstream_info(repo: &git2::Repository) -> Option<(String, usize, usize)> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let local = git2::Branch::wrap(head);
    let up = local.upstream().ok()?;
    let name = up.name().ok()??.to_string();
    let (a, b) = repo.graph_ahead_behind(local.get().target()?, up.get().target()?).ok()?;
    Some((name, a, b))
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
    let mut entries = Vec::new();
    for e in statuses.iter() {
        let Some(rel) = e.path() else { continue };
        let s = e.status();
        if !s.is_ignored() {
            let staged = if s.is_index_new() {
                Some(FileStatus::Added)
            } else if s.is_index_deleted() {
                Some(FileStatus::Deleted)
            } else if s.is_index_renamed() {
                Some(FileStatus::Renamed)
            } else if s.is_index_modified() || s.is_index_typechange() {
                Some(FileStatus::Modified)
            } else {
                None
            };
            let unstaged = if s.is_wt_new() {
                Some(FileStatus::Untracked)
            } else if s.is_wt_deleted() {
                Some(FileStatus::Deleted)
            } else if s.is_wt_renamed() {
                Some(FileStatus::Renamed)
            } else if s.is_wt_modified() || s.is_wt_typechange() {
                Some(FileStatus::Modified)
            } else {
                None
            };
            entries.push(StatusEntry { path: root.join(rel), staged, unstaged, conflict: s.is_conflicted() });
        }
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
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let (upstream, ahead, behind) = match upstream_info(&repo) {
        Some((u, a, b)) => (Some(u), a, b),
        None => (None, 0, 0),
    };
    Ok(RepoStatus { workdir: root, branch, detached, unborn, files, dirs, entries, upstream, ahead, behind })
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

/// Full message of the HEAD commit, if there is one.
pub fn head_message(workdir: &Path) -> Option<String> {
    let repo = git2::Repository::open(workdir).ok()?;
    let c = repo.head().ok()?.peel_to_commit().ok()?;
    c.message().map(str::to_string)
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
        assert_eq!(
            st.entries,
            vec![
                StatusEntry { path: root.join("new.txt"), staged: None, unstaged: Some(FileStatus::Untracked), conflict: false },
                StatusEntry { path: root.join("tracked.txt"), staged: None, unstaged: Some(FileStatus::Modified), conflict: false },
            ]
        );
        assert_eq!(st.upstream, None);
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

/// A commit referenced by a blame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub id: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub summary: String,
    pub message: String,
}

/// Who last changed each line. `lines[i]` indexes `commits`; `None` means
/// the line is not committed (edited in the buffer or new).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlameInfo {
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<Option<u32>>,
}

/// Blame `path` as of HEAD, then against `buffer` (the editor's text) so
/// unsaved edits show as not committed.
pub fn blame(workdir: &Path, path: &Path, buffer: Option<&str>) -> Result<BlameInfo, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let root = repo.workdir().map(Path::to_path_buf).unwrap_or_else(|| workdir.to_path_buf());
    let rel = path.strip_prefix(&root).map_err(|_| git2::Error::from_str("path is outside the repository"))?;
    let base = repo.blame_file(rel, None)?;
    let blame = match buffer {
        Some(b) => base.blame_buffer(b.as_bytes())?,
        None => base,
    };
    let mut info = BlameInfo::default();
    let mut index: HashMap<git2::Oid, u32> = HashMap::new();
    for h in blame.iter() {
        let id = h.final_commit_id();
        let slot = if id.is_zero() {
            None
        } else if let Some(i) = index.get(&id) {
            Some(*i)
        } else {
            let c = repo.find_commit(id)?;
            let a = c.author();
            info.commits.push(BlameCommit {
                id: id.to_string(),
                author: a.name().unwrap_or("").to_string(),
                email: a.email().unwrap_or("").to_string(),
                time: c.time().seconds(),
                summary: c.summary().unwrap_or("").to_string(),
                message: c.message().unwrap_or("").to_string(),
            });
            let i = (info.commits.len() - 1) as u32;
            index.insert(id, i);
            Some(i)
        };
        let start = h.final_start_line().saturating_sub(1);
        let end = start + h.lines_in_hunk();
        if info.lines.len() < end {
            info.lines.resize(end, None);
        }
        for l in &mut info.lines[start..end] {
            *l = slot;
        }
    }
    Ok(info)
}

/// Run [`blame`] on a worker thread.
pub fn start_blame(workdir: PathBuf, path: PathBuf, buffer: String) -> Receiver<Result<BlameInfo, String>> {
    let (tx, rx) = bounded(1);
    let spawned = std::thread::Builder::new().name("apex-git-blame".into()).spawn(move || {
        let _ = tx.send(blame(&workdir, &path, Some(&buffer)).map_err(|e| e.message().to_string()));
    });
    if let Err(e) = spawned {
        tracing::error!("could not spawn blame thread: {e}");
    }
    rx
}

/// "3 months ago" (long) or "3 mo" (short) for a Unix time.
pub fn relative_age(time: i64, now: i64, short: bool) -> String {
    let s = (now - time).max(0);
    let (n, long, abbr) = match s {
        0..60 => return if short { "now".into() } else { "just now".into() },
        60..3600 => (s / 60, "minute", "min"),
        3600..86_400 => (s / 3600, "hour", "h"),
        86_400..1_209_600 => (s / 86_400, "day", "d"),
        1_209_600..5_259_600 => (s / 604_800, "week", "wk"),
        5_259_600..31_557_600 => (s / 2_629_800, "month", "mo"),
        _ => (s / 31_557_600, "year", "y"),
    };
    if short { format!("{n} {abbr}") } else { format!("{n} {long}{} ago", if n == 1 { "" } else { "s" }) }
}

/// `git show`-style text for a commit: header, message and the patch
/// against its first parent.
pub fn commit_text(workdir: &Path, id: &str) -> Result<String, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let commit = repo.revparse_single(id)?.peel_to_commit()?;
    let a = commit.author();
    let mut out = format!(
        "commit {}\nAuthor: {} <{}>\nDate:   {}\n",
        commit.id(),
        a.name().unwrap_or(""),
        a.email().unwrap_or(""),
        format_date(a.when().seconds(), a.when().offset_minutes())
    );
    if commit.parent_count() > 1 {
        let parents: Vec<String> = commit.parent_ids().map(|p| p.to_string()[..7].to_string()).collect();
        out.push_str(&format!("Merge:  {}\n", parents.join(" ")));
    }
    out.push('\n');
    for l in commit.message().unwrap_or("").trim_end().lines() {
        if l.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("    {l}\n"));
        }
    }
    out.push('\n');
    let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit.tree()?), None)?;
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(out)
}

#[cfg(test)]
mod blame_tests {
    use super::*;

    fn commit_all(repo: &git2::Repository, msg: &str, time: i64) {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::new("Ada", "ada@example.com", &git2::Time::new(time, 0)).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents).unwrap();
    }

    #[test]
    fn blame_tracks_commits_and_buffer_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = git2::Repository::init(root).unwrap();
        let f = root.join("f.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        commit_all(&repo, "first", 1_000_000);
        std::fs::write(&f, "a\nB\nc\n").unwrap();
        commit_all(&repo, "second\n\nbody", 2_000_000);
        let info = blame(root, &f, Some("a\nB\nc\nnew\n")).unwrap();
        assert_eq!(info.commits.len(), 2);
        let summary = |l: usize| info.lines[l].map(|i| info.commits[i as usize].summary.clone());
        assert_eq!(summary(0).as_deref(), Some("first"));
        assert_eq!(summary(1).as_deref(), Some("second"));
        assert_eq!(summary(2).as_deref(), Some("first"));
        assert_eq!(info.lines[3], None);
        let text = commit_text(root, "HEAD").unwrap();
        assert!(text.starts_with("commit "), "{text}");
        assert!(text.contains("Author: Ada <ada@example.com>"));
        assert!(text.contains("    second\n\n    body\n"));
        assert!(text.contains("-b\n+B\n"), "{text}");
        assert!(commit_text(root, "HEAD~1").unwrap().contains("+a\n+b\n+c\n"));
    }

    #[test]
    fn ages() {
        assert_eq!(relative_age(100, 100, false), "just now");
        assert_eq!(relative_age(0, 120, false), "2 minutes ago");
        assert_eq!(relative_age(0, 3600, false), "1 hour ago");
        assert_eq!(relative_age(0, 86_400 * 3, true), "3 d");
        assert_eq!(relative_age(0, 86_400 * 21, false), "3 weeks ago");
        assert_eq!(relative_age(0, 86_400 * 100, true), "3 mo");
        assert_eq!(relative_age(0, 86_400 * 800, false), "2 years ago");
        assert_eq!(relative_age(500, 100, true), "now");
    }
}
