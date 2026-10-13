//! Background project-wide symbol index built with tree-sitter tags queries.
//!
//! Started on demand (first Goto Symbol in Project) and refreshed when the
//! file index changes. Bounded by file size and count so a huge monorepo
//! cannot exhaust memory; the UI reports when the index is partial.
//!
//! Data flow: `workspace_ui::open_project_symbol_palette` calls [`start`] with
//! the paths from the file index, which spawns the `apex-symbols` thread and
//! returns a [`SymbolScan`] handle. Each frame `workspace_ui::poll_background`
//! drains the handle's channel into [`ProjectSymbols`] via `poll`, and drops
//! the handle once `complete` is set. Dropping the handle early (roots change,
//! index restarts) raises the cancel flag so the thread stops at its next file.
//! The channel is bounded, so a thread whose consumer has gone away blocks
//! briefly and then exits when `send` fails.
//!
//! Invariants: `ProjectSymbols::complete` is only ever set by a `Done`
//! event, so a scan that was cancelled leaves the index partial and
//! `workspace_ui` restarts it on the next request. Symbols are appended in
//! scan order and never sorted here; the palette does its own ranking.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use apex_syntax::{Symbol, SyntaxState};
use crossbeam_channel::{Receiver, bounded};

/// Files larger than this (in bytes) are not parsed; they count as `skipped`.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Hard cap on the number of files one scan will look at, in index order.
pub const MAX_FILES: usize = 20_000;

/// One symbol definition found in a project file.
pub struct ProjectSymbol {
    /// File the symbol lives in (as listed by the file index).
    pub path: PathBuf,
    /// The tree-sitter symbol: name, kind, line and name range.
    pub symbol: Symbol,
}

/// Messages from the scan thread to the UI.
pub enum SymbolEvent {
    /// A chunk of symbols; sent every 256 symbols and once at the end.
    Batch(Vec<ProjectSymbol>),
    /// The scan finished. `files` were parsed; `skipped` were too large or
    /// failed to parse within the time budget.
    Done { files: usize, skipped: usize },
}

/// Handle to a running scan. Dropping it cancels the thread.
pub struct SymbolScan {
    /// Receiving end of the event channel; drained by `ProjectSymbols::poll`.
    pub rx: Receiver<SymbolEvent>,
    /// Shared with the thread, which checks it before every file.
    cancel: Arc<AtomicBool>,
}

impl Drop for SymbolScan {
    /// Ask the thread to stop; it exits before parsing its next file.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Spawn the `apex-symbols` thread over `files` and return its handle.
///
/// Only files whose language has a tags query (or markdown, which has its own
/// symbol extraction) are parsed. Binary files and files over `MAX_FILE_BYTES`
/// are skipped. If the thread cannot be spawned the error is logged and the
/// returned handle simply never yields events.
pub fn start(files: Vec<PathBuf>) -> SymbolScan {
    // Small bound: the UI drains every frame, and back-pressure stops the
    // thread racing ahead of a consumer that has stopped polling.
    let (tx, rx) = bounded(32);
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("apex-symbols".into())
        .spawn(move || {
            let mut batch = Vec::new();
            let mut done = 0usize;
            let mut skipped = 0usize;
            for path in files.into_iter().take(MAX_FILES) {
                if c.load(Ordering::Relaxed) {
                    return;
                }
                // Detect from the path alone; reading the file just to sniff a
                // shebang would cost an extra read for every file in the project.
                let lang = apex_syntax::detect(Some(&path), "");
                // Without a tags query there is nothing to extract; markdown is the
                // exception because its headings are extracted without one.
                if lang.grammar.as_ref().is_none_or(|g| g.tags.is_none()) && lang.id != "markdown" {
                    continue;
                }
                let Ok(md) = std::fs::metadata(&path) else { continue };
                if md.len() > MAX_FILE_BYTES {
                    skipped += 1;
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                if apex_buffer::detect_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut doc = apex_edit::Document::from_text(&text);
                // 200 ms parse budget per file: a pathological file counts as
                // skipped rather than stalling the whole scan.
                let state = SyntaxState::new(lang, &mut doc, Duration::from_millis(200));
                if !state.has_tree() {
                    skipped += 1;
                    continue;
                }
                // 2000 symbols per file is plenty for navigation and bounds memory
                // on generated code.
                for symbol in apex_syntax::symbols::document_symbols(&state, doc.rope(), 2000) {
                    batch.push(ProjectSymbol { path: path.clone(), symbol });
                }
                done += 1;
                // A failed send means the receiver was dropped: stop quietly.
                if batch.len() >= 256 && tx.send(SymbolEvent::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(SymbolEvent::Batch(batch));
            }
            // The receiver may already be gone; nothing useful to do if so.
            let _ = tx.send(SymbolEvent::Done { files: done, skipped });
        })
        .map_err(|e| tracing::error!("could not spawn symbol index thread: {e}"))
        .ok();
    SymbolScan { rx, cancel }
}

/// The accumulated index, owned by `ApexApp::symbols` and reset whenever the
/// workspace roots change.
#[derive(Default)]
pub struct ProjectSymbols {
    /// Every symbol received so far, in scan order.
    pub symbols: Vec<ProjectSymbol>,
    /// Set once the `Done` event arrives; until then the palette says "indexing".
    pub complete: bool,
    /// Files the scan could not include (too large or unparsed), for the UI note.
    pub skipped: usize,
    /// Not read anywhere in the crate at present.
    pub index_generation: u64,
}

impl ProjectSymbols {
    /// Drain any events the scan has produced. Returns `true` when something
    /// changed, so the caller knows to repaint or rebuild the palette list.
    pub fn poll(&mut self, scan: &SymbolScan) -> bool {
        let mut changed = false;
        while let Ok(ev) = scan.rx.try_recv() {
            match ev {
                SymbolEvent::Batch(b) => {
                    self.symbols.extend(b);
                    changed = true;
                }
                SymbolEvent::Done { skipped, .. } => {
                    self.complete = true;
                    self.skipped = skipped;
                    changed = true;
                }
            }
        }
        changed
    }
}
