//! Git awareness in the application: status polling, sidebar marks, gutter
//! hunks, hunk navigation and revert, compare views and file history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apex_edit::{DocId, Selection, SelectionSet};
use apex_project::git::{self, FileStatus, Hunk, HunkKind, RepoStatus, StatusScan};

use crate::app::{ApexApp, Dialog};
use crate::command::Command;
use crate::palette::{PaletteItem, PaletteMode, Payload};
use crate::sidebar::GitMark;

pub(crate) struct RepoEntry {
    pub(crate) status: RepoStatus,
    scan: Option<StatusScan>,
    refreshed: Instant,
    dirty: bool,
    error: Option<String>,
}

struct BaseText {
    text: Option<String>,
    loaded: Instant,
    head: bool,
}

#[derive(Default)]
pub struct GitUi {
    pub(crate) repos: HashMap<PathBuf, RepoEntry>,
    root_workdir: HashMap<PathBuf, Option<PathBuf>>,
    doc_workdir: HashMap<DocId, Option<PathBuf>>,
    base: HashMap<DocId, BaseText>,
    hunks: HashMap<DocId, (u64, Vec<Hunk>)>,
    /// Compare the buffer with HEAD instead of the index.
    pub compare_head: bool,
    pub last_hunk_message: Option<String>,
    /// Command-line jobs (`git_ops.rs`).
    pub(crate) ops: crate::git_ops::GitOps,
    /// Source Control panel state (`git_panel.rs`).
    pub(crate) sc: crate::git_panel::SourceControl,
}

impl GitUi {
    pub fn has_repos(&self) -> bool {
        !self.repos.is_empty()
    }
}

const STATUS_REFRESH: Duration = Duration::from_secs(4);
const BASE_REFRESH: Duration = Duration::from_secs(10);

fn mark_of(s: FileStatus) -> GitMark {
    match s {
        FileStatus::Modified | FileStatus::Renamed | FileStatus::Staged => GitMark::Modified,
        FileStatus::Added => GitMark::Added,
        FileStatus::Untracked => GitMark::Untracked,
        FileStatus::Deleted => GitMark::Deleted,
        FileStatus::Ignored => GitMark::Ignored,
        FileStatus::Conflict => GitMark::Conflict,
    }
}

impl ApexApp {
    fn git_workdir_for_path(&mut self, path: &Path) -> Option<PathBuf> {
        if !self.settings.git_gutter {
            return None;
        }
        // Cache per root to avoid repeated discovery on every frame.
        let root = self.roots.iter().find(|r| path.starts_with(r)).cloned();
        if let Some(r) = &root
            && let Some(w) = self.git.root_workdir.get(r)
        {
            return w.clone();
        }
        let w = git::workdir_of(path);
        if let Some(r) = root {
            self.git.root_workdir.insert(r, w.clone());
        }
        w
    }

    pub(crate) fn git_workdir_for_doc(&mut self, d: DocId) -> Option<PathBuf> {
        if let Some(w) = self.git.doc_workdir.get(&d) {
            return w.clone();
        }
        let path = self.docs.get(&d).and_then(|doc| doc.path.clone())?;
        let w = self.git_workdir_for_path(&path);
        self.git.doc_workdir.insert(d, w.clone());
        w
    }

    /// Called from the frame loop: poll scans and schedule refreshes.
    pub(crate) fn git_poll(&mut self) -> bool {
        if !self.settings.git_gutter {
            return false;
        }
        let mut changed = false;
        // Ensure a repo entry exists for every root (and open file outside the roots).
        let roots = self.roots.clone();
        let mut wanted: Vec<PathBuf> = roots.iter().filter_map(|r| self.git_workdir_for_path(r)).collect();
        let doc_ids: Vec<DocId> = self.docs.keys().copied().collect();
        for d in doc_ids {
            if let Some(w) = self.git_workdir_for_doc(d) {
                wanted.push(w);
            }
        }
        wanted.sort();
        wanted.dedup();
        self.git.repos.retain(|k, _| wanted.contains(k));
        for w in wanted {
            let entry = self.git.repos.entry(w.clone()).or_insert_with(|| RepoEntry {
                status: RepoStatus::default(),
                scan: None,
                refreshed: Instant::now() - STATUS_REFRESH * 2,
                dirty: true,
                error: None,
            });
            if let Some(scan) = &entry.scan {
                match scan.rx.try_recv() {
                    Ok(Ok(status)) => {
                        entry.status = status;
                        entry.error = None;
                        entry.scan = None;
                        entry.refreshed = Instant::now();
                        changed = true;
                    }
                    Ok(Err(e)) => {
                        entry.error = Some(e);
                        entry.scan = None;
                        entry.refreshed = Instant::now();
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                    Err(_) => entry.scan = None,
                }
            } else if entry.dirty && entry.refreshed.elapsed() >= STATUS_REFRESH {
                entry.dirty = false;
                entry.scan = Some(git::start_status(w.clone(), false));
            }
        }
        if changed {
            self.git_update_sidebar_marks();
            // Base texts may be stale after a commit or checkout.
            self.git.base.retain(|_, b| b.loaded.elapsed() < BASE_REFRESH);
            self.git.hunks.clear();
        }
        changed
    }

    /// Periodic refresh of status only (keeps base texts and hunks).
    pub(crate) fn git_mark_dirty_status_only(&mut self) {
        for e in self.git.repos.values_mut() {
            e.dirty = true;
        }
    }

    /// Mark repositories as needing a status refresh (file watcher, saves).
    pub(crate) fn git_mark_dirty(&mut self) {
        for e in self.git.repos.values_mut() {
            e.dirty = true;
        }
        self.git.base.clear();
        self.git.hunks.clear();
    }

    fn git_update_sidebar_marks(&mut self) {
        let mut marks = HashMap::new();
        for e in self.git.repos.values() {
            for (p, s) in &e.status.files {
                marks.insert(p.clone(), mark_of(*s));
            }
            for (p, s) in &e.status.dirs {
                marks.entry(p.clone()).or_insert(mark_of(*s));
            }
        }
        self.sidebar.git_marks = marks;
    }

    /// Branch label for the status bar (active document's repository, else first root).
    pub(crate) fn git_branch_label(&mut self) -> Option<String> {
        let w = match self.active_ids() {
            Some((_, d)) => self.git_workdir_for_doc(d).or_else(|| self.roots.first().cloned().and_then(|r| self.git_workdir_for_path(&r))),
            None => self.roots.first().cloned().and_then(|r| self.git_workdir_for_path(&r)),
        }?;
        let e = self.git.repos.get(&w)?;
        if e.status.branch.is_empty() {
            return None;
        }
        let mut s = e.status.branch.clone();
        if e.status.unborn {
            s.push_str(" (no commits)");
        }
        Some(s)
    }

    fn git_base_text(&mut self, d: DocId) -> Option<String> {
        let head = self.git.compare_head;
        if let Some(b) = self.git.base.get(&d)
            && b.loaded.elapsed() < BASE_REFRESH
            && b.head == head
        {
            return b.text.clone();
        }
        let path = self.docs.get(&d).and_then(|doc| doc.path.clone())?;
        let workdir = self.git_workdir_for_doc(d)?;
        let text = git::file_at(&workdir, &path, head).ok().flatten();
        self.git.base.insert(d, BaseText { text: text.clone(), loaded: Instant::now(), head });
        text
    }

    /// Hunks of the current buffer against the base, cached by revision.
    pub(crate) fn git_hunks(&mut self, d: DocId) -> Vec<Hunk> {
        let Some(doc) = self.docs.get(&d) else { return Vec::new() };
        let rev = doc.revision();
        if let Some((r, h)) = self.git.hunks.get(&d)
            && *r == rev
        {
            return h.clone();
        }
        if doc.large_file || doc.len_bytes() > 4 * 1024 * 1024 {
            return Vec::new();
        }
        let Some(base) = self.git_base_text(d) else {
            self.git.hunks.insert(d, (rev, Vec::new()));
            return Vec::new();
        };
        let current = self.docs[&d].text();
        let hunks = git::diff_hunks(&base, &current);
        self.git.hunks.insert(d, (rev, hunks.clone()));
        hunks
    }

    pub(crate) fn git_marks(&mut self, d: DocId) -> Vec<(usize, u8)> {
        if !self.settings.git_gutter {
            return Vec::new();
        }
        let mut out = Vec::new();
        for h in self.git_hunks(d) {
            match h.kind {
                HunkKind::Added => out.extend(h.new_lines.clone().map(|l| (l, 0u8))),
                HunkKind::Modified => out.extend(h.new_lines.clone().map(|l| (l, 1u8))),
                HunkKind::Deleted => out.push((h.new_lines.start.saturating_sub(1), 2u8)),
            }
        }
        out
    }

    pub(crate) fn git_hunk_at_caret(&mut self, v: crate::view::ViewId, d: DocId) -> Option<Hunk> {
        let line = self.views[&v].primary_line(&self.docs[&d]);
        self.git_hunks(d).into_iter().find(|h| {
            (h.new_lines.start..h.new_lines.end.max(h.new_lines.start + 1)).contains(&line) || (h.kind == HunkKind::Deleted && h.new_lines.start == line)
        })
    }

    pub(crate) fn git_command(&mut self, ctx: &egui::Context, cmd: Command) -> bool {
        match cmd {
            Command::NextChange | Command::PrevChange => {
                let Some((v, d)) = self.active_ids() else { return true };
                let hunks = self.git_hunks(d);
                if hunks.is_empty() {
                    self.set_status("No changes against the Git index".into(), false);
                    return true;
                }
                let line = self.views[&v].primary_line(&self.docs[&d]);
                let target = if cmd == Command::NextChange {
                    hunks.iter().find(|h| h.new_lines.start > line).or(hunks.first())
                } else {
                    hunks.iter().rev().find(|h| h.new_lines.start < line).or(hunks.last())
                };
                if let Some(h) = target {
                    let l = h.new_lines.start;
                    self.note_jump();
                    self.goto_line_col(v, l + 1, 1);
                    self.git.last_hunk_message = Some(self.git_hunk_summary(h));
                }
            }
            Command::ShowHunkDiff => {
                let Some((v, d)) = self.active_ids() else { return true };
                match self.git_hunk_at_caret(v, d) {
                    Some(h) => {
                        let base = self.git_base_text(d).unwrap_or_default();
                        let removed = git::revert_hunk_text(&base, &h);
                        let msg = if removed.is_empty() {
                            self.git_hunk_summary(&h)
                        } else {
                            format!("{}\nRemoved:\n{}", self.git_hunk_summary(&h), removed.trim_end())
                        };
                        self.dialog = Some(Dialog::Info { title: "Change at caret".into(), body: msg });
                    }
                    None => self.set_status("No change at the caret".into(), false),
                }
            }
            Command::RevertHunk => {
                let Some((v, d)) = self.active_ids() else { return true };
                match self.git_hunk_at_caret(v, d) {
                    Some(h) => {
                        self.dialog = Some(Dialog::ConfirmRevertHunk {
                            view: v,
                            hunk_start: h.new_lines.start,
                            hunk_end: h.new_lines.end,
                            old_start: h.old_lines.start,
                            old_end: h.old_lines.end,
                        })
                    }
                    None => self.set_status("No change at the caret".into(), false),
                }
            }
            Command::CompareWithHead | Command::CompareWithIndex | Command::CompareWithSaved => {
                let Some((_, d)) = self.active_ids() else { return true };
                let Some(doc) = self.docs.get(&d) else { return true };
                let Some(path) = doc.path.clone() else {
                    self.set_status("Save the file first".into(), false);
                    return true;
                };
                let current = doc.text();
                let (base, label) = match cmd {
                    Command::CompareWithSaved => {
                        let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
                        (apex_project::fileio::load(&path, None, fallback).ok().map(|l| l.text), "saved")
                    }
                    Command::CompareWithHead => {
                        let prev = self.git.compare_head;
                        self.git.compare_head = true;
                        let b = self.git_base_text(d);
                        self.git.compare_head = prev;
                        self.git.base.remove(&d);
                        (b, "HEAD")
                    }
                    _ => {
                        let prev = self.git.compare_head;
                        self.git.compare_head = false;
                        let b = self.git_base_text(d);
                        self.git.compare_head = prev;
                        self.git.base.remove(&d);
                        (b, "index")
                    }
                };
                let Some(base) = base else {
                    self.set_status(format!("No {label} version of this file"), false);
                    return true;
                };
                if base == current {
                    self.set_status(format!("No differences against {label}"), false);
                    return true;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let diff = git::unified_diff(&base, &current, &format!("{name} ({label})"), &format!("{name} (buffer)"));
                self.open_scratch_doc(&format!("{name} vs {label}.diff"), &diff, Some("diff"), true, ctx);
            }
            Command::FileHistory => {
                let Some((_, d)) = self.active_ids() else { return true };
                let Some(path) = self.docs.get(&d).and_then(|doc| doc.path.clone()) else { return true };
                let Some(workdir) = self.git_workdir_for_doc(d) else {
                    self.set_status("Not in a Git repository".into(), false);
                    return true;
                };
                match git::file_history(&workdir, &path, 200) {
                    Ok(entries) if entries.is_empty() => self.set_status("No commits touch this file yet".into(), false),
                    Ok(entries) => {
                        let items: Vec<PaletteItem> = entries
                            .iter()
                            .map(|e| PaletteItem {
                                label: format!("{}  {}", e.short_id, e.summary),
                                detail: format!("{}  {}", e.date, e.author),
                                shortcut: String::new(),
                                payload: Payload::Text(e.id.clone()),
                                boost: 0,
                            })
                            .collect();
                        self.palette.show_with(PaletteMode::Prompt("history".into()), items, "File history (Enter copies the commit id)", "");
                    }
                    Err(e) => self.set_status(format!("History failed: {}", e.message()), true),
                }
            }
            Command::GitStatusPanel => {
                let mut items = Vec::new();
                for e in self.git.repos.values() {
                    let mut files: Vec<(&PathBuf, &FileStatus)> = e.status.files.iter().filter(|(_, s)| **s != FileStatus::Ignored).collect();
                    files.sort_by(|a, b| a.0.cmp(b.0));
                    for (p, s) in files {
                        items.push(PaletteItem {
                            label: self.display_path(p),
                            detail: format!("{s:?}").to_lowercase(),
                            shortcut: String::new(),
                            payload: Payload::File(p.clone(), None),
                            boost: 0,
                        });
                    }
                }
                let branch = self.git_branch_label().unwrap_or_else(|| "no repository".into());
                self.palette.show_with(PaletteMode::RecentFiles, items, &format!("Git status — {branch}"), "");
            }
            _ => return false,
        }
        true
    }

    fn git_hunk_summary(&self, h: &Hunk) -> String {
        let n = h.new_lines.end.saturating_sub(h.new_lines.start);
        let o = h.old_lines.end.saturating_sub(h.old_lines.start);
        match h.kind {
            HunkKind::Added => format!("Added {n} line{} at line {}", if n == 1 { "" } else { "s" }, h.new_lines.start + 1),
            HunkKind::Deleted => format!("Deleted {o} line{} before line {}", if o == 1 { "" } else { "s" }, h.new_lines.start + 1),
            HunkKind::Modified => format!("Changed {o} → {n} lines at line {}", h.new_lines.start + 1),
        }
    }

    /// Replace the hunk's lines with the base text (an undoable edit).
    pub(crate) fn git_apply_revert(&mut self, v: crate::view::ViewId, hunk_start: usize, hunk_end: usize, old_start: usize, old_end: usize) {
        let Some(d) = self.views.get(&v).map(|x| x.doc) else { return };
        let Some(base) = self.git_base_text(d) else { return };
        let hunk = Hunk { new_lines: hunk_start..hunk_end, old_lines: old_start..old_end, kind: HunkKind::Modified };
        let replacement = git::revert_hunk_text(&base, &hunk);
        self.run_edit(v, |doc, s| {
            let rope = doc.rope();
            let n = rope.len_lines();
            let start = rope.line_to_char(hunk_start.min(n.saturating_sub(1)));
            let end = if hunk_end < n { rope.line_to_char(hunk_end) } else { rope.len_chars() };
            // Keep the terminator convention of the document.
            let text = apex_buffer::line_ending::normalise(&replacement, doc.line_ending);
            let tx = apex_edit::Transaction::single(apex_edit::Change::replace(start, end, text));
            let sels = SelectionSet::single(Selection::point(start));
            let _ = s;
            Some(apex_edit::ops::EditResult { tx, sels, kind: apex_edit::EditKind::Other })
        });
        self.git.hunks.remove(&d);
    }

    /// Open a read-only scratch document (diff views, history).
    pub(crate) fn open_scratch_doc(&mut self, title: &str, text: &str, language: Option<&str>, read_only: bool, ctx: &egui::Context) {
        let mut doc = apex_edit::Document::from_text(text);
        doc.language_override = language.map(str::to_string);
        doc.read_only = read_only;
        doc.untitled_index = None;
        doc.scratch_title = Some(title.to_string());
        let v = self.add_document(doc);
        self.focus_view(v);
        self.focus_active_editor(ctx);
    }
}
