//! Background project-wide symbol index built with tree-sitter tags queries.
//!
//! Started on demand (first Goto Symbol in Project) and refreshed when the
//! file index changes. Bounded by file size and count so a huge monorepo
//! cannot exhaust memory; the UI reports when the index is partial.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use apex_syntax::{Symbol, SyntaxState};
use crossbeam_channel::{Receiver, bounded};

pub const MAX_FILE_BYTES: u64 = 1 << 20;
pub const MAX_FILES: usize = 20_000;

pub struct ProjectSymbol {
    pub path: PathBuf,
    pub symbol: Symbol,
}

pub enum SymbolEvent {
    Batch(Vec<ProjectSymbol>),
    Done { files: usize, skipped: usize },
}

pub struct SymbolScan {
    pub rx: Receiver<SymbolEvent>,
    cancel: Arc<AtomicBool>,
}

impl Drop for SymbolScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start(files: Vec<PathBuf>) -> SymbolScan {
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
                let lang = apex_syntax::detect(Some(&path), "");
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
                let state = SyntaxState::new(lang, &mut doc, Duration::from_millis(200));
                if !state.has_tree() {
                    skipped += 1;
                    continue;
                }
                for symbol in apex_syntax::symbols::document_symbols(&state, doc.rope(), 2000) {
                    batch.push(ProjectSymbol { path: path.clone(), symbol });
                }
                done += 1;
                if batch.len() >= 256 && tx.send(SymbolEvent::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(SymbolEvent::Batch(batch));
            }
            let _ = tx.send(SymbolEvent::Done { files: done, skipped });
        })
        .map_err(|e| tracing::error!("could not spawn symbol index thread: {e}"))
        .ok();
    SymbolScan { rx, cancel }
}

#[derive(Default)]
pub struct ProjectSymbols {
    pub symbols: Vec<ProjectSymbol>,
    pub complete: bool,
    pub skipped: usize,
    pub index_generation: u64,
}

impl ProjectSymbols {
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
