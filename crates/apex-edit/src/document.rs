//! The document: text, revision, history and file metadata.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use apex_buffer::{Encoding, LineEnding, Rope};
use ropey::RopeSlice;

use crate::history::History;
use crate::indent::IndentSettings;
use crate::selection::SelectionSet;
use crate::transaction::{Assoc, Transaction};

/// Process-unique document identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocId(pub u64);

static NEXT_DOC_ID: AtomicU64 = AtomicU64::new(1);

impl DocId {
    pub fn next() -> Self {
        Self(NEXT_DOC_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Classification of an edit for history coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Character-by-character insertion.
    Typing,
    /// Character-by-character backward deletion.
    DeleteBackward,
    /// Character-by-character forward deletion.
    DeleteForward,
    /// Anything else: never coalesced.
    Other,
}

impl EditKind {
    pub fn coalesces(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// A byte-level edit as a syntax tree wants to hear about it (tree-sitter
/// `InputEdit` without the dependency). Points are (row, byte column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

/// What the file looked like when we last read or wrote it, used to detect
/// external modification before overwriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub modified: Option<std::time::SystemTime>,
    pub len: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(unix)]
    pub dev: u64,
}

impl FileIdentity {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let md = std::fs::metadata(path)?;
        Ok(Self::from_metadata(&md))
    }

    pub fn from_metadata(md: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            modified: md.modified().ok(),
            len: md.len(),
            #[cfg(unix)]
            inode: md.ino(),
            #[cfg(unix)]
            dev: md.dev(),
        }
    }
}

/// A text document. Views hold selections; the document holds everything that
/// is shared between views.
#[derive(Debug)]
pub struct Document {
    pub id: DocId,
    rope: Rope,
    revision: u64,
    saved_revision: u64,
    history: History,
    /// Recently applied transactions `(revision_after, tx)` so that views can
    /// remap stale selections. Bounded ring.
    recent: VecDeque<(u64, Arc<Transaction>)>,
    /// Byte edits not yet consumed by the syntax layer, in application order.
    pending_byte_edits: Vec<ByteEdit>,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    pub mixed_line_endings: bool,
    pub had_decode_errors: bool,
    pub read_only: bool,
    pub large_file: bool,
    pub file_identity: Option<FileIdentity>,
    /// Language identifier override (`None` = detect from path/content).
    pub language_override: Option<String>,
    pub indent: IndentSettings,
    pub indent_detected: bool,
    /// Bookmarked char offsets, kept in sync with edits.
    bookmarks: Vec<usize>,
    /// Named marks (Vim-style and user named), kept in sync with edits.
    marks: BTreeMap<String, usize>,
    /// Untitled documents get a stable display name.
    pub untitled_index: Option<u32>,
    /// Display name for scratch buffers (diff views, history) with no path.
    pub scratch_title: Option<String>,
}

impl Document {
    pub fn empty() -> Self {
        Self::from_text(String::new())
    }

    pub fn from_text(text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        let stats = apex_buffer::line_ending::scan(text, 1 << 20);
        let line_ending = stats.dominant(LineEnding::native());
        let rope = Rope::from_str(text);
        let large_file = rope.len_bytes() > LARGE_FILE_BYTES || has_huge_line(&rope);
        Self {
            id: DocId::next(),
            rope,
            revision: 1,
            saved_revision: 1,
            history: History::default(),
            recent: VecDeque::with_capacity(RECENT_TX),
            pending_byte_edits: Vec::new(),
            path: None,
            encoding: Encoding::Utf8,
            line_ending,
            mixed_line_endings: stats.is_mixed(),
            had_decode_errors: false,
            read_only: false,
            large_file,
            file_identity: None,
            language_override: None,
            indent: IndentSettings::default(),
            indent_detected: false,
            bookmarks: Vec::new(),
            marks: BTreeMap::new(),
            untitled_index: None,
            scratch_title: None,
        }
    }

    #[inline]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    #[inline]
    pub fn slice(&self) -> RopeSlice<'_> {
        self.rope.slice(..)
    }

    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Mark the document as pristine at its current revision (used when the
    /// text was replaced wholesale by a reload).
    pub fn mark_clean(&mut self) {
        self.saved_revision = self.revision;
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Display name: file name or "Untitled N".
    pub fn title(&self) -> String {
        if let Some(t) = &self.scratch_title {
            return t.clone();
        }
        match &self.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None => match self.untitled_index {
                Some(i) if i > 1 => format!("Untitled {i}"),
                _ => "Untitled".into(),
            },
        }
    }

    /// Drain byte edits accumulated since the last call (for the syntax layer).
    pub fn take_pending_byte_edits(&mut self) -> Vec<ByteEdit> {
        std::mem::take(&mut self.pending_byte_edits)
    }

    pub fn has_pending_byte_edits(&self) -> bool {
        !self.pending_byte_edits.is_empty()
    }

    /// Apply `tx` as a new undoable step.
    ///
    /// `sel_before`/`sel_after` are the selections of the view performing the
    /// edit; they are restored on undo/redo. Returns `false` when the document
    /// is read-only or the transaction was empty.
    pub fn apply(&mut self, tx: Transaction, sel_before: &SelectionSet, sel_after: &SelectionSet, kind: EditKind) -> bool {
        if self.read_only || tx.is_empty() {
            return false;
        }
        let tx = tx.clamped(&self.rope);
        let rev_before = self.revision;
        let (inverse, byte_edits) = self.apply_raw(&tx);
        self.pending_byte_edits.extend(byte_edits);
        let rev_after = self.revision;
        self.history.record(tx, inverse, sel_before, sel_after, kind, rev_before, rev_after);
        true
    }

    /// Apply without recording history; still bumps the revision and remaps
    /// marks. Used by undo/redo and by reload.
    fn apply_raw(&mut self, tx: &Transaction) -> (Transaction, Vec<ByteEdit>) {
        let (inverse, byte_edits) = tx.apply(&mut self.rope);
        self.revision += 1;
        for b in &mut self.bookmarks {
            *b = tx.map_pos(*b, Assoc::Before);
        }
        self.bookmarks.sort_unstable();
        self.bookmarks.dedup();
        for m in self.marks.values_mut() {
            *m = tx.map_pos(*m, Assoc::Before);
        }
        if self.recent.len() >= RECENT_TX {
            self.recent.pop_front();
        }
        self.recent.push_back((self.revision, Arc::new(tx.clone())));
        (inverse, byte_edits)
    }

    /// Undo one history entry. Returns the selections to restore.
    pub fn undo(&mut self) -> Option<SelectionSet> {
        let entry = self.history.pop_undo()?;
        for (_, inverse) in entry.steps.iter().rev() {
            let (_, edits) = self.apply_raw(inverse);
            self.pending_byte_edits.extend(edits);
        }
        let sels = entry.sel_before.clone();
        self.history.push_redo(entry);
        Some(sels)
    }

    /// Redo one history entry. Returns the selections to restore.
    pub fn redo(&mut self) -> Option<SelectionSet> {
        let entry = self.history.pop_redo()?;
        for (forward, _) in &entry.steps {
            let (_, edits) = self.apply_raw(forward);
            self.pending_byte_edits.extend(edits);
        }
        let sels = entry.sel_after.clone();
        self.history.push_undo_entry(entry);
        Some(sels)
    }

    /// Replace the entire text (e.g. reload from disk) as a single undoable step.
    pub fn replace_all(&mut self, text: &str, sels: &SelectionSet) {
        let tx = Transaction::single(crate::transaction::Change::replace(0, self.rope.len_chars(), text));
        let mut after = sels.clone();
        after.map(&tx, Assoc::Before);
        after.clamp(&Rope::from_str(text));
        self.history.mark_boundary();
        self.apply(tx, sels, &after, EditKind::Other);
        self.history.mark_boundary();
    }

    /// Remap a view's selections that were valid at `seen_revision` to the
    /// current revision. Returns `false` if the history ring no longer covers
    /// the gap and the selections were merely clamped.
    pub fn sync_selections(&self, sels: &mut SelectionSet, seen_revision: u64) -> bool {
        if seen_revision == self.revision {
            return true;
        }
        let covered = self.recent.front().is_some_and(|(rev, _)| *rev <= seen_revision + 1);
        if covered {
            for (rev, tx) in &self.recent {
                if *rev > seen_revision {
                    sels.map(tx, Assoc::Before);
                }
            }
        }
        sels.clamp(&self.rope);
        covered
    }

    /// Remap arbitrary char positions recorded at `seen_revision` (fold anchors etc.).
    pub fn sync_positions(&self, positions: &mut [usize], seen_revision: u64) {
        if seen_revision == self.revision {
            return;
        }
        let covered = self.recent.front().is_some_and(|(rev, _)| *rev <= seen_revision + 1);
        if covered {
            for (rev, tx) in &self.recent {
                if *rev > seen_revision {
                    for p in positions.iter_mut() {
                        *p = tx.map_pos(*p, Assoc::Before);
                    }
                }
            }
        }
        let n = self.rope.len_chars();
        for p in positions.iter_mut() {
            *p = (*p).min(n);
        }
    }

    pub fn bookmarks(&self) -> &[usize] {
        &self.bookmarks
    }

    pub fn toggle_bookmark(&mut self, line: usize) {
        let line_start = self.rope.line_to_char(line.min(self.rope.len_lines().saturating_sub(1)));
        let line_end = apex_buffer::position::line_end_char(&self.rope, line);
        if let Some(i) = self.bookmarks.iter().position(|&b| b >= line_start && b <= line_end) {
            self.bookmarks.remove(i);
        } else {
            self.bookmarks.push(line_start);
            self.bookmarks.sort_unstable();
        }
    }

    pub fn clear_bookmarks(&mut self) {
        self.bookmarks.clear();
    }

    pub fn bookmarked_lines(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.bookmarks.iter().map(|&b| self.rope.char_to_line(b.min(self.rope.len_chars()))).collect();
        v.dedup();
        v
    }

    pub fn set_mark(&mut self, name: impl Into<String>, pos: usize) {
        self.marks.insert(name.into(), pos.min(self.rope.len_chars()));
    }

    pub fn mark(&self, name: &str) -> Option<usize> {
        self.marks.get(name).copied()
    }

    pub fn marks(&self) -> &BTreeMap<String, usize> {
        &self.marks
    }

    /// Text of line `line` without its terminator.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        let start = self.rope.line_to_char(line);
        let end = apex_buffer::position::line_end_char(&self.rope, line);
        self.rope.slice(start..end).into()
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Run indentation detection if not done yet and the document is large
    /// enough to have evidence.
    pub fn detect_indent_once(&mut self, default: IndentSettings) {
        if self.indent_detected {
            return;
        }
        self.indent_detected = true;
        self.indent = default;
        if let Some(d) = crate::indent::detect_indent(&self.rope, 2000) {
            self.indent.style = d.style;
            if d.style == crate::indent::IndentStyle::Spaces {
                self.indent.indent_width = d.indent_width;
            }
        }
    }
}

const RECENT_TX: usize = 256;
/// Above this size the document opens in large-file mode.
pub const LARGE_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Lines longer than this (bytes) trigger large-file mode too.
pub const HUGE_LINE_BYTES: usize = 1 << 20;

fn has_huge_line(rope: &Rope) -> bool {
    if rope.len_bytes() < HUGE_LINE_BYTES {
        return false;
    }
    let lines = rope.len_lines();
    // Average line length is a cheap proxy; exact scan is only needed when it is suspicious.
    if rope.len_bytes() / lines.max(1) < 4096 {
        return false;
    }
    rope.lines().any(|l| l.len_bytes() > HUGE_LINE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;
    use crate::transaction::Change;

    #[test]
    fn apply_undo_redo_restores_text_and_selections() {
        let mut d = Document::from_text("hello");
        let before = SelectionSet::single(Selection::point(5));
        let after = SelectionSet::single(Selection::point(6));
        assert!(d.apply(Transaction::single(Change::insert(5, "!")), &before, &after, EditKind::Typing));
        assert_eq!(d.text(), "hello!");
        assert!(d.is_dirty());
        let s = d.undo().unwrap();
        assert_eq!(d.text(), "hello");
        assert_eq!(s, before);
        let s = d.redo().unwrap();
        assert_eq!(d.text(), "hello!");
        assert_eq!(s, after);
        assert!(d.redo().is_none());
    }

    #[test]
    fn typing_coalesces_and_other_edits_do_not() {
        let mut d = Document::from_text("");
        let mut sel = SelectionSet::single(Selection::point(0));
        for (i, c) in "abc".chars().enumerate() {
            let after = SelectionSet::single(Selection::point(i + 1));
            d.apply(Transaction::single(Change::insert(i, c.to_string())), &sel, &after, EditKind::Typing);
            sel = after;
        }
        assert_eq!(d.history().undo_depth(), 1);
        d.apply(Transaction::single(Change::insert(3, "X")), &sel, &sel, EditKind::Other);
        assert_eq!(d.history().undo_depth(), 2);
        d.undo();
        assert_eq!(d.text(), "abc");
        d.undo();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn bookmarks_follow_edits() {
        let mut d = Document::from_text("a\nb\nc\n");
        d.toggle_bookmark(2);
        assert_eq!(d.bookmarked_lines(), vec![2]);
        let s = SelectionSet::default();
        d.apply(Transaction::single(Change::insert(0, "X\n")), &s, &s, EditKind::Other);
        assert_eq!(d.bookmarked_lines(), vec![3]);
        d.toggle_bookmark(3);
        assert!(d.bookmarked_lines().is_empty());
    }

    #[test]
    fn stale_selections_are_resynced() {
        let mut d = Document::from_text("abcdef");
        let seen = d.revision();
        let mut other_view = SelectionSet::single(Selection::new(3, 5));
        let s = SelectionSet::default();
        d.apply(Transaction::single(Change::insert(0, "XX")), &s, &s, EditKind::Other);
        assert!(d.sync_selections(&mut other_view, seen));
        assert_eq!(other_view.primary(), Selection::new(5, 7));
    }

    #[test]
    fn read_only_blocks_edits() {
        let mut d = Document::from_text("x");
        d.read_only = true;
        let s = SelectionSet::default();
        assert!(!d.apply(Transaction::single(Change::insert(0, "y")), &s, &s, EditKind::Other));
    }
}
