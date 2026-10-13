//! Repository-wide reads for branches, stashes, the commit log graph and
//! merge state (libgit2, read-only), and conflict-marker handling for
//! resolving merges in the editor.
//!
//! Companion to `git.rs`, which covers per-file status and diffs. The git
//! panel in `apex-ui` calls `branches`, `stashes`, `operation` and `log`
//! when it refreshes (each opens the repository afresh through libgit2 and
//! returns plain data, so callers may run them on a worker thread). Any
//! action the user picks from those lists (checkout, stash pop, continue,
//! abort) is performed by `git_cli`, not here. The conflict-marker helpers
//! are pure string functions used by the editor's merge resolution buttons.
//! Refs under `refs/apex-*` (backup refs written by `git_history`) and
//! `refs/stash` are hidden from the log.

use std::collections::HashMap;
use std::path::Path;

/// One local or remote-tracking branch for the branch picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// `main`, or `origin/main` for remote-tracking branches.
    pub name: String,
    /// True for remote-tracking branches.
    pub remote: bool,
    /// True for the checked-out branch.
    pub head: bool,
    /// Upstream name (`origin/main`) for local branches that track one.
    pub upstream: Option<String>,
    /// Subject line of the tip commit.
    pub summary: String,
    /// Commit time of the tip, seconds since the Unix epoch (0 if unknown).
    pub time: i64,
}

/// Local branches (current first, then by last commit), then remote ones.
///
/// The `origin/HEAD` symbolic ref is skipped since it duplicates a branch.
pub fn branches(workdir: &Path) -> Result<Vec<BranchInfo>, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let mut out = Vec::new();
    for b in repo.branches(None)? {
        let (b, kind) = b?;
        let Some(name) = b.name()?.map(str::to_string) else { continue };
        if kind == git2::BranchType::Remote && name.ends_with("/HEAD") {
            continue;
        }
        // An unborn or broken branch has no commit; keep it with empty details.
        let commit = b.get().peel_to_commit().ok();
        out.push(BranchInfo {
            upstream: b.upstream().ok().and_then(|u| u.name().ok().flatten().map(str::to_string)),
            head: b.is_head(),
            remote: kind == git2::BranchType::Remote,
            summary: commit.as_ref().and_then(|c| c.summary().ok().flatten().map(str::to_string)).unwrap_or_default(),
            time: commit.as_ref().map(|c| c.time().seconds()).unwrap_or(0),
            name,
        });
    }
    out.sort_by(|a, b| a.remote.cmp(&b.remote).then(b.head.cmp(&a.head)).then(b.time.cmp(&a.time)));
    Ok(out)
}

/// An operation in progress that needs Continue or Abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Merge,
    /// Any rebase variant (plain, interactive, merge-based, or `am`).
    Rebase,
    CherryPick,
    Revert,
}

impl Operation {
    /// The git subcommand name, for building `git <verb> --continue/--abort`.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
    /// Status-bar text while the operation is pending.
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merging",
            Self::Rebase => "Rebasing",
            Self::CherryPick => "Cherry-picking",
            Self::Revert => "Reverting",
        }
    }
}

/// The pending operation, if any, from libgit2's view of `.git` state files
/// (`MERGE_HEAD`, `rebase-merge/`, `CHERRY_PICK_HEAD`, ...). `None` also
/// when the repository cannot be opened.
pub fn operation(workdir: &Path) -> Option<Operation> {
    let repo = git2::Repository::open(workdir).ok()?;
    use git2::RepositoryState as S;
    match repo.state() {
        S::Merge => Some(Operation::Merge),
        S::Rebase | S::RebaseInteractive | S::RebaseMerge | S::ApplyMailboxOrRebase => Some(Operation::Rebase),
        S::CherryPick | S::CherryPickSequence => Some(Operation::CherryPick),
        S::Revert | S::RevertSequence => Some(Operation::Revert),
        _ => None,
    }
}

/// One stash for the stash list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// Position in the stash stack (`stash@{index}`), 0 being the newest.
    pub index: usize,
    /// The stash message, e.g. `On main: wip`.
    pub message: String,
}

/// All stashes, newest first (the order `git stash list` uses).
pub fn stashes(workdir: &Path) -> Result<Vec<StashEntry>, git2::Error> {
    // `stash_foreach` needs `&mut` on the repository handle.
    let mut repo = git2::Repository::open(workdir)?;
    let mut out = Vec::new();
    repo.stash_foreach(|i, msg, _| {
        out.push(StashEntry { index: i, message: msg.to_string() });
        true
    })?;
    Ok(out)
}

// ----- log graph -------------------------------------------------------------

/// One row of the log: a commit and how to draw its graph cell. Lanes are
/// columns; `top` segments run from the row's top edge (lane `from`) to the
/// node's centre height (lane `to`), `bottom` from the centre to the bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// Full commit id (hex).
    pub id: String,
    /// Parent count; 2 or more marks a merge.
    pub parents: usize,
    pub author: String,
    /// Commit time, seconds since the Unix epoch.
    pub time: i64,
    /// Subject line.
    pub summary: String,
    /// Decorations: `HEAD -> main`, `origin/main`, `tag: v1`, ...
    pub refs: Vec<String>,
    /// Column of this commit's node.
    pub lane: usize,
    /// `(from, to)` lane pairs for segments in the top half of the row.
    pub top: Vec<(usize, usize)>,
    /// `(from, to)` lane pairs for segments in the bottom half of the row.
    pub bottom: Vec<(usize, usize)>,
}

/// All commits reachable from branches, tags and HEAD, newest first in
/// topological order, with graph lanes. At most `limit` rows.
///
/// Lane assignment is the usual single-pass algorithm: `lanes[i]` holds the
/// commit id expected next in column `i`. A commit takes the first lane
/// waiting for it (or the first free one), its first parent inherits that
/// lane, further parents take free lanes, and columns whose expected commit
/// is unchanged draw a straight pass-through segment. Trailing empty lanes
/// are trimmed so the graph stays narrow.
pub fn log(workdir: &Path, limit: usize) -> Result<Vec<LogRow>, git2::Error> {
    let repo = git2::Repository::open(workdir)?;
    let mut refs: HashMap<git2::Oid, Vec<String>> = HashMap::new();
    let head = repo.head().ok();
    let head_name = head.as_ref().filter(|h| h.is_branch()).and_then(|h| h.shorthand().ok().map(str::to_string));
    for r in repo.references()?.flatten() {
        let Ok(name) = r.shorthand().map(str::to_string) else { continue };
        // Skip `origin/HEAD` (duplicate), stashes and our own backup refs.
        if name.ends_with("/HEAD") || r.name().is_ok_and(|n| n.starts_with("refs/stash") || n.starts_with("refs/apex-")) {
            continue;
        }
        let Ok(c) = r.peel_to_commit() else { continue };
        let label = if r.is_tag() {
            format!("tag: {name}")
        } else if Some(&name) == head_name.as_ref() {
            format!("HEAD -> {name}")
        } else {
            name
        };
        refs.entry(c.id()).or_default().push(label);
    }
    // Detached HEAD: no branch carries the label, so attach a bare "HEAD".
    if head_name.is_none()
        && let Some(id) = head.as_ref().and_then(|h| h.target())
    {
        refs.entry(id).or_default().insert(0, "HEAD".into());
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    // Each push may fail individually (unborn HEAD, no remotes); the walk
    // simply covers whatever did push.
    let _ = walk.push_head();
    let _ = walk.push_glob("refs/heads/*");
    let _ = walk.push_glob("refs/remotes/*");
    let _ = walk.push_glob("refs/tags/*");
    let mut lanes: Vec<Option<git2::Oid>> = Vec::new();
    let mut rows = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        let c = repo.find_commit(oid)?;
        let parents: Vec<git2::Oid> = c.parent_ids().collect();
        // Snapshot of the lanes entering this row, for drawing `top`.
        let before = lanes.clone();
        // This commit's column: a lane waiting for it, else the first free
        // lane, else a new one.
        let lane = lanes.iter().position(|l| *l == Some(oid)).or_else(|| lanes.iter().position(Option::is_none)).unwrap_or_else(|| {
            lanes.push(None);
            lanes.len() - 1
        });
        let mut top = Vec::new();
        for (i, l) in before.iter().enumerate() {
            match l {
                // Every lane waiting for this commit converges on its node.
                Some(x) if *x == oid => top.push((i, lane)),
                // Other lanes pass straight through the top half.
                Some(_) => top.push((i, i)),
                None => {}
            }
        }
        // All lanes that were waiting for this commit are now free.
        for l in lanes.iter_mut() {
            if *l == Some(oid) {
                *l = None;
            }
        }
        let mut bottom = Vec::new();
        for (k, p) in parents.iter().enumerate() {
            if let Some(j) = lanes.iter().position(|l| *l == Some(*p)) {
                // Parent already expected elsewhere: draw a join to that lane.
                bottom.push((lane, j));
            } else {
                // First parent keeps this commit's lane if it is still free;
                // other parents (merge sources) open a new lane.
                let slot = if k == 0 && lanes[lane].is_none() {
                    lane
                } else {
                    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                        lanes.push(None);
                        lanes.len() - 1
                    })
                };
                lanes[slot] = Some(*p);
                bottom.push((lane, slot));
            }
        }
        // Lanes untouched by this commit continue straight down.
        for (i, l) in before.iter().enumerate() {
            if l.is_some() && *l != Some(oid) && lanes.get(i) == Some(l) {
                bottom.push((i, i));
            }
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        let a = c.author();
        rows.push(LogRow {
            id: oid.to_string(),
            parents: parents.len(),
            author: a.name().unwrap_or("").to_string(),
            time: c.time().seconds(),
            summary: c.summary().ok().flatten().unwrap_or("").to_string(),
            refs: refs.remove(&oid).unwrap_or_default(),
            lane,
            top,
            bottom,
        });
    }
    Ok(rows)
}

// ----- conflict markers ---------------------------------------------------------

/// A conflict block, as zero-based line numbers of its marker lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    /// `<<<<<<<`
    pub start: usize,
    /// `|||||||` (diff3 style), if present.
    pub base: Option<usize>,
    /// `=======`
    pub mid: usize,
    /// `>>>>>>>`
    pub end: usize,
}

impl Conflict {
    /// Lines of "our" side: after `<<<<<<<` up to the base marker (diff3)
    /// or `=======`.
    pub fn ours(&self) -> std::ops::Range<usize> {
        self.start + 1..self.base.unwrap_or(self.mid)
    }
    /// Lines of "their" side: after `=======` up to `>>>>>>>`.
    pub fn theirs(&self) -> std::ops::Range<usize> {
        self.mid + 1..self.end
    }
}

/// Conflict blocks in `lines` (well-formed ones only).
///
/// A block needs `<<<<<<<`, `=======` and `>>>>>>>` in that order; a new
/// `<<<<<<<` resets any partial block, and a block without a closing marker
/// is dropped. Markers are matched by prefix, so the labels git appends are
/// ignored. Nested markers are not handled (git never produces them).
pub fn conflicts<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Conflict> {
    let mut out = Vec::new();
    let (mut start, mut base, mut mid) = (None, None, None);
    for (i, l) in lines.enumerate() {
        if l.starts_with("<<<<<<<") {
            (start, base, mid) = (Some(i), None, None);
        } else if l.starts_with("|||||||") && start.is_some() && mid.is_none() {
            base = Some(i);
        } else if l.starts_with("=======") && start.is_some() && mid.is_none() {
            mid = Some(i);
        } else if l.starts_with(">>>>>>>")
            && let (Some(s), Some(m)) = (start, mid)
        {
            out.push(Conflict { start: s, base, mid: m, end: i });
            (start, base, mid) = (None, None, None);
        }
    }
    out
}

/// Which side(s) to keep when resolving a conflict block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Ours,
    Theirs,
    /// Ours followed by theirs, markers removed.
    Both,
}

/// The text that replaces a conflict block (marker lines included) for a
/// resolution; `lines` are the block's lines with their terminators.
///
/// `lines` is the whole document split with `split_inclusive('\n')`, so the
/// `Conflict` line numbers index it directly. Ranges are clamped to the
/// slice so a stale `Conflict` cannot panic.
pub fn resolve(lines: &[&str], c: &Conflict, how: Resolution) -> String {
    let take = |r: std::ops::Range<usize>| lines[r.start.min(lines.len())..r.end.min(lines.len())].concat();
    match how {
        Resolution::Ours => take(c.ours()),
        Resolution::Theirs => take(c.theirs()),
        Resolution::Both => take(c.ours()) + &take(c.theirs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `git` in `dir` and panic with stderr on failure.
    fn git(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// diff3-style blocks parse with their base section, each resolution
    /// yields the right lines, and an unterminated block is ignored.
    #[test]
    fn conflict_parsing_and_resolution() {
        let text = "a\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> feat\nz\n<<<<<<< x\n";
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let cs = conflicts(text.lines());
        assert_eq!(cs, vec![Conflict { start: 1, base: Some(3), mid: 5, end: 7 }]);
        assert_eq!(resolve(&lines, &cs[0], Resolution::Ours), "ours\n");
        assert_eq!(resolve(&lines, &cs[0], Resolution::Theirs), "theirs\n");
        assert_eq!(resolve(&lines, &cs[0], Resolution::Both), "ours\ntheirs\n");
        assert!(conflicts("<<<<<<< a\nx\n".lines()).is_empty());
    }

    /// End-to-end against a real `git` binary: branch ordering, merge state
    /// detection, the two-lane graph of a merge, and stash listing. Skipped
    /// when `git` is not installed.
    #[test]
    fn branches_state_stash_and_graph_with_real_git() {
        if !crate::git_cli::available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "user.email", "t@x"]);
        git(p, &["config", "commit.gpgsign", "false"]);
        std::fs::write(p.join("f"), "base\n").unwrap();
        git(p, &["add", "f"]);
        git(p, &["commit", "-q", "-m", "root"]);
        git(p, &["switch", "-q", "-c", "feat"]);
        std::fs::write(p.join("f"), "feat\n").unwrap();
        git(p, &["commit", "-q", "-am", "on feat"]);
        git(p, &["switch", "-q", "main"]);
        std::fs::write(p.join("f"), "main\n").unwrap();
        git(p, &["commit", "-q", "-am", "on main"]);

        let b = branches(p).unwrap();
        assert_eq!(b.iter().map(|x| (x.name.as_str(), x.head)).collect::<Vec<_>>(), vec![("main", true), ("feat", false)]);

        // Conflicting merge leaves the repository mid-merge.
        let o = std::process::Command::new("git").current_dir(p).args(["merge", "feat"]).output().unwrap();
        assert!(!o.status.success());
        assert_eq!(operation(p), Some(Operation::Merge));
        let text = std::fs::read_to_string(p.join("f")).unwrap();
        assert_eq!(conflicts(text.lines()).len(), 1);
        git(p, &["merge", "--abort"]);
        assert_eq!(operation(p), None);

        git(p, &["merge", "-q", "-s", "ours", "-m", "merge feat", "feat"]);
        let rows = log(p, 100).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].summary, "merge feat");
        assert_eq!(rows[0].parents, 2);
        assert!(rows[0].refs.contains(&"HEAD -> main".to_string()));
        // The merge opens a second lane that rejoins at the root.
        assert_eq!(rows[0].bottom.len(), 2);
        assert!(rows.iter().any(|r| r.lane == 1));
        let root = rows.last().unwrap();
        assert_eq!(root.summary, "root");
        // The feature lane joins the main lane below its last commit.
        assert!(rows.iter().any(|r| r.bottom.contains(&(1, 0))), "{rows:?}");
        assert_eq!(root.top, vec![(0, 0)]);
        assert!(root.bottom.is_empty());

        std::fs::write(p.join("f"), "dirty\n").unwrap();
        git(p, &["stash", "push", "-q", "-m", "wip"]);
        let s = stashes(p).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].message.contains("wip"));
    }
}
