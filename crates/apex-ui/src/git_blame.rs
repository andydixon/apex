//! Blame: per-line annotations in the gutter (Toggle Blame), an optional
//! dim note after the caret line (`git_blame_inline`), the current line's
//! author in the status bar, and the commit view opened from them.
//!
//! Blame runs on a worker thread against the buffer text, so unsaved edits
//! show as not committed; it is recomputed shortly after the text changes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use apex_edit::DocId;
use apex_project::git::{self, BlameInfo};
use crossbeam_channel::Receiver;

use crate::app::ApexApp;
use crate::command::Command;
use crate::editor::{BlameCell, BlameColumn};

const DEBOUNCE: Duration = Duration::from_millis(700);

struct DocBlame {
    info: Option<Arc<BlameInfo>>,
    /// Document revision the info was computed for.
    rev: Option<u64>,
    pending: Option<(u64, Receiver<Result<BlameInfo, String>>)>,
    changed_at: Instant,
    seen_rev: u64,
    column: Option<(u64, f32, Arc<BlameColumn>)>,
    error: Option<String>,
}

#[derive(Default)]
pub struct BlameUi {
    /// Documents with the blame column shown.
    pub shown: HashSet<DocId>,
    docs: HashMap<DocId, DocBlame>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Build the gutter column from blame data.
pub fn column(info: &BlameInfo, lines: usize, width: f32, now: i64) -> BlameColumn {
    let (oldest, newest) = info.commits.iter().fold((i64::MAX, i64::MIN), |(a, b), c| (a.min(c.time), b.max(c.time)));
    let span = (newest - oldest).max(1) as f32;
    let mut cells = Vec::with_capacity(lines);
    let mut prev: Option<Option<u32>> = None;
    for l in 0..lines {
        let slot = info.lines.get(l).copied().flatten();
        let first = prev != Some(slot);
        prev = Some(slot);
        cells.push(match slot.and_then(|i| info.commits.get(i as usize)) {
            Some(c) => BlameCell {
                author: c.author.clone(),
                age: git::relative_age(c.time, now, true),
                tint: (c.time - oldest) as f32 / span,
                first,
                commit: Some(c.id.clone()),
                tooltip: format!(
                    "{}  {} <{}>\n{} ({})\n\n{}\n\nClick to show the commit",
                    &c.id[..c.id.len().min(10)],
                    c.author,
                    c.email,
                    git::format_date(c.time, 0),
                    git::relative_age(c.time, now, false),
                    c.message.trim_end()
                ),
            },
            None => BlameCell { author: "Not committed".into(), age: String::new(), tint: 0.0, first, commit: None, tooltip: String::new() },
        });
    }
    BlameColumn { width, cells }
}

impl ApexApp {
    fn blame_wanted(&mut self, d: DocId) -> bool {
        self.git.blame.shown.contains(&d) || (self.settings.git_blame_inline && self.active_ids().is_some_and(|(_, a)| a == d))
    }

    /// Poll and schedule blame computations (frame loop). True while busy.
    pub(crate) fn git_blame_poll(&mut self) -> bool {
        let mut busy = false;
        let docs: Vec<DocId> = self.docs.keys().copied().collect();
        self.git.blame.docs.retain(|d, _| docs.contains(d));
        self.git.blame.shown.retain(|d| docs.contains(d));
        for d in docs {
            if !self.blame_wanted(d) {
                continue;
            }
            let Some(doc) = self.docs.get(&d) else { continue };
            if doc.large_file {
                continue;
            }
            let rev = doc.revision();
            let entry = self.git.blame.docs.entry(d).or_insert_with(|| DocBlame {
                info: None,
                rev: None,
                pending: None,
                changed_at: Instant::now() - DEBOUNCE,
                seen_rev: rev,
                column: None,
                error: None,
            });
            if entry.seen_rev != rev {
                entry.seen_rev = rev;
                entry.changed_at = Instant::now();
            }
            if let Some((r, rx)) = &entry.pending {
                busy = true;
                match rx.try_recv() {
                    Ok(Ok(info)) => {
                        entry.info = Some(Arc::new(info));
                        entry.rev = Some(*r);
                        entry.error = None;
                        entry.pending = None;
                        entry.column = None;
                    }
                    Ok(Err(e)) => {
                        entry.error = Some(e);
                        entry.rev = Some(*r);
                        entry.pending = None;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                    Err(_) => entry.pending = None,
                }
                continue;
            }
            if entry.rev == Some(rev) {
                continue;
            }
            busy = true;
            if entry.changed_at.elapsed() < DEBOUNCE && entry.info.is_some() {
                continue;
            }
            let (Some(path), Some(workdir)) = (doc.path.clone(), apex_project::git::workdir_of(doc.path.as_deref().unwrap_or(std::path::Path::new("")))) else {
                entry.rev = Some(rev);
                entry.error = Some("not in a Git repository".into());
                continue;
            };
            entry.pending = Some((rev, git::start_blame(workdir, path, doc.text())));
        }
        busy
    }

    /// Gutter column for a document, if blame is shown for it.
    pub(crate) fn git_blame_column(&mut self, d: DocId, char_width: f32) -> Option<Arc<BlameColumn>> {
        if !self.git.blame.shown.contains(&d) {
            return None;
        }
        let lines = self.docs.get(&d)?.len_lines();
        let width = (char_width * 24.0).round();
        let e = self.git.blame.docs.get_mut(&d)?;
        let info = e.info.clone()?;
        let key = Arc::as_ptr(&info) as u64 ^ lines as u64;
        if let Some((k, w, c)) = &e.column
            && *k == key
            && *w == width
        {
            return Some(c.clone());
        }
        let c = Arc::new(column(&info, lines, width, now_unix()));
        e.column = Some((key, width, c.clone()));
        Some(c)
    }

    /// "Author, 3 months ago • summary" for a line.
    pub(crate) fn git_blame_line(&self, d: DocId, line: usize) -> Option<String> {
        let info = self.git.blame.docs.get(&d)?.info.as_ref()?;
        Some(match info.lines.get(line).copied().flatten().and_then(|i| info.commits.get(i as usize)) {
            Some(c) => format!("{}, {} • {}", c.author, git::relative_age(c.time, now_unix(), false), c.summary),
            None => "Not committed yet".into(),
        })
    }

    pub(crate) fn git_blame_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::GitToggleBlame => {
                let Some((_, d)) = self.active_ids() else { return true };
                if !self.git.blame.shown.remove(&d) {
                    if self.docs.get(&d).is_some_and(|doc| doc.path.as_deref().and_then(apex_project::git::workdir_of).is_none()) {
                        self.set_status("Not in a Git repository".into(), false);
                        return true;
                    }
                    self.git.blame.shown.insert(d);
                    if let Some(e) = self.git.blame.docs.get(&d)
                        && let Some(err) = &e.error
                    {
                        self.set_status(format!("Blame unavailable: {err}"), false);
                    }
                }
            }
            Command::GitShowCommit => {
                let id = match arg {
                    Some(a) => a,
                    None => {
                        // The commit of the caret line.
                        let Some((v, d)) = self.active_ids() else { return true };
                        let line = self.views[&v].primary_line(&self.docs[&d]);
                        let found = self
                            .git
                            .blame
                            .docs
                            .get(&d)
                            .and_then(|e| e.info.as_ref())
                            .and_then(|i| i.lines.get(line).copied().flatten().and_then(|c| i.commits.get(c as usize)))
                            .map(|c| c.id.clone());
                        match found {
                            Some(id) => id,
                            None => {
                                self.set_status("Toggle Blame first, then show the commit of a committed line".into(), false);
                                return true;
                            }
                        }
                    }
                };
                let Some((workdir, _)) = self.git_target(None) else { return true };
                match git::commit_text(&workdir, &id) {
                    Ok(text) => {
                        let short = &id[..id.len().min(7)];
                        self.open_scratch_doc(&format!("commit {short}.diff"), &text, Some("diff"), true, ctx);
                    }
                    Err(e) => self.set_status(format!("Could not show {id}: {}", e.message()), true),
                }
            }
            _ => return false,
        }
        true
    }

    /// Status bar and inline note text for the active view's caret line.
    pub(crate) fn git_blame_caret_note(&mut self) -> Option<(usize, String)> {
        let (v, d) = self.active_ids()?;
        if !self.blame_wanted(d) {
            return None;
        }
        let line = self.views.get(&v)?.primary_line(&self.docs[&d]);
        self.git_blame_line(d, line).map(|s| (line, s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_project::git::BlameCommit;

    #[test]
    fn column_groups_runs_and_tints_by_age() {
        let c = |id: &str, t| BlameCommit { id: id.into(), author: "A".into(), email: "a@x".into(), time: t, summary: "s".into(), message: "s\n".into() };
        let info = BlameInfo { commits: vec![c("old", 0), c("new", 100)], lines: vec![Some(0), Some(0), Some(1), None] };
        let col = column(&info, 5, 200.0, 86_400);
        assert_eq!(col.cells.len(), 5);
        assert!(col.cells[0].first && !col.cells[1].first && col.cells[2].first && col.cells[3].first && !col.cells[4].first);
        assert_eq!(col.cells[0].tint, 0.0);
        assert_eq!(col.cells[2].tint, 1.0);
        assert_eq!(col.cells[3].commit, None);
        assert_eq!(col.cells[2].age, "23 h");
        assert!(col.cells[0].tooltip.contains("Click to show the commit"));
    }
}
