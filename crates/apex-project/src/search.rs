//! Find in Files: streaming, cancellable, bounded-memory search across roots.
//!
//! Files are read one at a time per worker; nothing is kept in memory beyond
//! the current file and the results. Binary files (NUL in the first 8 KiB)
//! and files over `max_file_bytes` are skipped and counted.
//!
//! Flow: the Find in Files panel builds a `SearchRequest` and calls `start`,
//! which compiles the regex, spawns an `apex-search` coordinator thread and
//! returns a `SearchHandle`. The coordinator runs the `ignore` crate's
//! parallel walker (the same filter rules as the file index) with 2-8
//! worker threads; each worker searches whole files and sends
//! `SearchEvent::File` results over a bounded channel that the UI drains
//! every frame. `SearchEvent::Done` with statistics is always the last
//! event. Dropping the handle cancels the search. `replace_in_file` is the
//! synchronous companion used by Replace in Files, one call per file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::index::{WalkOptions, walker};

/// Everything a search needs; built by the UI from the panel and settings.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Workspace folders to search, in order.
    pub roots: Vec<PathBuf>,
    /// Literal text or regex source, depending on `regex`.
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Wrap the pattern in `\b` word boundaries.
    pub whole_word: bool,
    /// Glob filters: when non-empty a file must match at least one (against
    /// its full path or bare file name).
    pub include: Vec<String>,
    /// Glob patterns to skip, in addition to ignore files.
    pub exclude: Vec<String>,
    /// Search dot-files.
    pub hidden: bool,
    pub respect_gitignore: bool,
    pub follow_symlinks: bool,
    /// Files larger than this are skipped and counted in `skipped_large`.
    pub max_file_bytes: u64,
    /// Stop walking once this many individual matches have been found.
    pub max_results: usize,
    /// Reserved; currently always 0 and not used by the search itself.
    pub context_lines: usize,
}

/// All matching lines of one file.
#[derive(Debug, Clone)]
pub struct FileMatches {
    pub path: PathBuf,
    /// In file order; each line appears once with all its ranges.
    pub lines: Vec<LineMatch>,
}

/// One matching line.
#[derive(Debug, Clone)]
pub struct LineMatch {
    /// Zero-based line number.
    pub line: usize,
    /// The line without its terminator, truncated to about 1000 bytes.
    pub text: String,
    /// Byte ranges within `text`.
    pub ranges: Vec<(usize, usize)>,
}

/// Message from the search threads to the holder of a `SearchHandle`.
#[derive(Debug)]
pub enum SearchEvent {
    /// A file with at least one match.
    File(FileMatches),
    /// The search finished (or was cancelled/truncated); always the last event.
    Done(SearchStats),
    /// Reserved for fatal errors; `start` currently reports those via `Err`.
    Error(String),
}

/// Counters shown in the results header when the search finishes.
#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub files_scanned: usize,
    pub files_matched: usize,
    /// Individual match count across all files.
    pub matches: usize,
    pub skipped_binary: usize,
    pub skipped_large: usize,
    pub unreadable: usize,
    pub elapsed: std::time::Duration,
    /// True if `max_results` stopped the walk early.
    pub truncated: bool,
}

/// Owner-side handle to a running search.
pub struct SearchHandle {
    /// Results and the final `Done`.
    pub rx: Receiver<SearchEvent>,
    /// Shared with the workers; set by `cancel` and on drop.
    cancel: Arc<AtomicBool>,
    /// Files searched so far, for a live progress counter.
    pub scanned: Arc<AtomicUsize>,
}

impl SearchHandle {
    /// Ask the walker to stop; workers check this per file.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for SearchHandle {
    /// Dropping the handle cancels the search so a replaced search does not
    /// keep reading files for a channel nobody drains.
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Compile the request's pattern the same way in-document search does.
///
/// Literal patterns are escaped; `whole_word` wraps in `\b(?:...)\b`. The
/// size limits stop a pathological user regex from consuming unbounded
/// memory. The error string is the regex crate's message, shown verbatim.
pub fn compile(req: &SearchRequest) -> Result<regex::Regex, String> {
    let mut src = if req.regex { req.pattern.clone() } else { regex::escape(&req.pattern) };
    if req.whole_word {
        src = format!(r"\b(?:{src})\b");
    }
    regex::RegexBuilder::new(&src)
        .case_insensitive(!req.case_sensitive)
        .multi_line(true)
        .size_limit(10 * 1024 * 1024)
        .dfa_size_limit(2 * 1024 * 1024)
        .build()
        .map_err(|e| e.to_string())
}

/// Validate and compile the request, then start the search on a background
/// thread. Errors: empty pattern, invalid regex, or thread spawn failure.
pub fn start(req: SearchRequest) -> Result<SearchHandle, String> {
    if req.pattern.is_empty() {
        return Err("empty pattern".into());
    }
    let re = compile(&req)?;
    // Bounded: workers block rather than buffering unbounded results while
    // the UI is busy.
    let (tx, rx) = bounded::<SearchEvent>(256);
    let cancel = Arc::new(AtomicBool::new(false));
    let scanned = Arc::new(AtomicUsize::new(0));
    let (c, s) = (cancel.clone(), scanned.clone());
    std::thread::Builder::new().name("apex-search".into()).spawn(move || run(req, re, tx, c, s)).map_err(|e| e.to_string())?;
    Ok(SearchHandle { rx, cancel, scanned })
}

/// Body of the `apex-search` thread: one parallel walk per root, then the
/// final `Done` event. Per-file work happens in the walker's worker threads
/// via the closure built in `run`; shared counters are atomics plus one
/// mutex-protected `SearchStats`.
fn run(req: SearchRequest, re: regex::Regex, tx: Sender<SearchEvent>, cancel: Arc<AtomicBool>, scanned: Arc<AtomicUsize>) {
    let start = Instant::now();
    let include = build_globs(&req.include);
    let opts = WalkOptions {
        exclude: req.exclude.clone(),
        hidden: req.hidden,
        follow_symlinks: req.follow_symlinks,
        respect_gitignore: req.respect_gitignore,
        max_files: usize::MAX,
    };
    // Searching is I/O and regex bound; more than 8 threads mostly adds
    // contention, fewer than 2 wastes an idle disk.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    let stats = std::sync::Mutex::new(SearchStats::default());
    let total_matches = AtomicUsize::new(0);
    for root in &req.roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut builder = walker(root, &opts);
        builder.threads(threads);
        let walker = builder.build_parallel();
        // `run` calls this factory once per worker thread; the returned boxed
        // closure is that worker's per-entry visitor.
        walker.run(|| {
            let tx = tx.clone();
            let re = &re;
            let req = &req;
            let include = &include;
            let cancel = &cancel;
            let scanned = &scanned;
            let stats = &stats;
            let total_matches = &total_matches;
            Box::new(move |entry| {
                // `Quit` stops the whole walk, not just this worker.
                if cancel.load(Ordering::Relaxed) || total_matches.load(Ordering::Relaxed) >= req.max_results {
                    return ignore::WalkState::Quit;
                }
                let Ok(entry) = entry else { return ignore::WalkState::Continue };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return ignore::WalkState::Continue;
                }
                let path = entry.path();
                if !include.is_empty() && !include.iter().any(|g| g.is_match(path) || path.file_name().is_some_and(|n| g.is_match(Path::new(n)))) {
                    return ignore::WalkState::Continue;
                }
                scanned.fetch_add(1, Ordering::Relaxed);
                match search_file(path, re, req) {
                    Ok(Some(fm)) => {
                        let n = fm.lines.iter().map(|l| l.ranges.len()).sum::<usize>();
                        total_matches.fetch_add(n, Ordering::Relaxed);
                        if let Ok(mut s) = stats.lock() {
                            s.files_matched += 1;
                            s.matches += n;
                        }
                        // Receiver gone (handle dropped): stop everything.
                        if tx.send(SearchEvent::File(fm)).is_err() {
                            return ignore::WalkState::Quit;
                        }
                    }
                    Ok(None) => {}
                    Err(Skip::Binary) => {
                        if let Ok(mut s) = stats.lock() {
                            s.skipped_binary += 1;
                        }
                    }
                    Err(Skip::Large) => {
                        if let Ok(mut s) = stats.lock() {
                            s.skipped_large += 1;
                        }
                    }
                    Err(Skip::Unreadable) => {
                        if let Ok(mut s) = stats.lock() {
                            s.unreadable += 1;
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    }
    // A poisoned mutex (a worker panicked) just loses the counters.
    let mut s = stats.into_inner().unwrap_or_default();
    s.files_scanned = scanned.load(Ordering::Relaxed);
    s.elapsed = start.elapsed();
    s.truncated = total_matches.load(Ordering::Relaxed) >= req.max_results;
    let _ = tx.send(SearchEvent::Done(s));
}

/// Why `search_file` did not search a file; each maps to a `SearchStats` counter.
enum Skip {
    Binary,
    Large,
    Unreadable,
}

/// Search one file. `Ok(None)` means readable but no match. Lines are
/// capped at 10,000 per file and each line's text at about 1000 bytes so
/// a minified bundle cannot blow up the results view.
fn search_file(path: &Path, re: &regex::Regex, req: &SearchRequest) -> Result<Option<FileMatches>, Skip> {
    let md = std::fs::metadata(path).map_err(|_| Skip::Unreadable)?;
    if md.len() > req.max_file_bytes {
        return Err(Skip::Large);
    }
    let bytes = std::fs::read(path).map_err(|_| Skip::Unreadable)?;
    if apex_buffer::detect_binary(&bytes) {
        return Err(Skip::Binary);
    }
    // Lossy decoding is fine for searching; the editor re-decodes properly on open.
    let text = String::from_utf8_lossy(&bytes);
    // Cheap pre-check: most files have no match, so avoid building the line
    // table for them.
    if !re.is_match(&text) {
        return Ok(None);
    }
    // Collect matches, then group by line.
    let mut lines: Vec<LineMatch> = Vec::new();
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    // Binary search for the line containing a byte offset.
    let line_of = |byte: usize| line_starts.partition_point(|&s| s <= byte).saturating_sub(1);
    for m in re.find_iter(&text) {
        // Empty matches (e.g. `a*`) would highlight nothing and spam lines.
        if m.start() == m.end() {
            continue;
        }
        let line = line_of(m.start());
        let ls = line_starts[line];
        let le = text[ls..].find('\n').map(|i| ls + i).unwrap_or(text.len());
        // Keep `\r` out of the displayed text for CRLF files.
        let le = if text[..le].ends_with('\r') { le - 1 } else { le };
        // A multi-line match is reported on its first line only, clipped to
        // that line's end.
        let range = (m.start() - ls, m.end().min(le).max(m.start()) - ls);
        match lines.last_mut() {
            Some(last) if last.line == line => last.ranges.push(range),
            _ => {
                let mut t = text[ls..le].to_string();
                if t.len() > 1000 {
                    t.truncate(t.floor_char_boundary(1000));
                }
                lines.push(LineMatch { line, text: t, ranges: vec![range] });
            }
        }
        if lines.len() > 10_000 {
            break;
        }
    }
    Ok(Some(FileMatches { path: path.to_path_buf(), lines }))
}

/// Compile include globs, dropping blanks and patterns that fail to parse.
fn build_globs(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns.iter().filter(|p| !p.trim().is_empty()).filter_map(|p| globset::Glob::new(p.trim()).ok().map(|g| g.compile_matcher())).collect()
}

/// Outcome of replacing in one file.
#[derive(Debug, Clone)]
pub struct ReplaceOutcome {
    pub path: PathBuf,
    /// Number of matches replaced; 0 when there were none or on error.
    pub replaced: usize,
    /// User-facing reason the file was left untouched, if it was.
    pub error: Option<String>,
}

/// Replace all matches of `re` in `path` with `replacement`, preserving the
/// file's encoding and line endings and refusing if it changed since
/// `expected` was taken. Writes through `fileio::save`.
///
/// `replacement` uses the regex crate's `$1`/`${name}` syntax. With
/// `expected == None` the identity from the fresh load is used, which still
/// catches a write racing between load and save. Binary files and files
/// with no non-empty match are left untouched.
pub fn replace_in_file(path: &Path, re: &regex::Regex, replacement: &str, expected: Option<apex_edit::FileIdentity>) -> ReplaceOutcome {
    // Windows-1252 decodes every byte, so non-UTF-8 files round-trip rather
    // than failing.
    let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
    let loaded = match crate::fileio::load(path, None, fallback) {
        Ok(l) => l,
        Err(e) => return ReplaceOutcome { path: path.to_path_buf(), replaced: 0, error: Some(e.to_string()) },
    };
    if loaded.binary.is_some() {
        return ReplaceOutcome { path: path.to_path_buf(), replaced: 0, error: Some("binary file".into()) };
    }
    let count = re.find_iter(&loaded.text).filter(|m| m.start() != m.end()).count();
    if count == 0 {
        return ReplaceOutcome { path: path.to_path_buf(), replaced: 0, error: None };
    }
    let new_text = re.replace_all(&loaded.text, replacement).into_owned();
    let opts = crate::fileio::SaveOptions {
        encoding: loaded.encoding,
        line_ending: loaded.line_ending,
        expected: expected.or(Some(loaded.identity)),
        force: false,
        in_place: false,
    };
    match crate::fileio::save(path, &new_text, &opts) {
        Ok(_) => ReplaceOutcome { path: path.to_path_buf(), replaced: count, error: None },
        Err(e) => ReplaceOutcome { path: path.to_path_buf(), replaced: 0, error: Some(e.to_string()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A literal, case-insensitive request over one root with no filters.
    fn req(root: &Path, pattern: &str) -> SearchRequest {
        SearchRequest {
            roots: vec![root.to_path_buf()],
            pattern: pattern.into(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            include: vec![],
            exclude: vec![],
            hidden: false,
            respect_gitignore: false,
            follow_symlinks: false,
            max_file_bytes: 10 << 20,
            max_results: 10_000,
            context_lines: 0,
        }
    }

    /// Matches are grouped per line, CRLF is stripped from line text, and a
    /// binary file is counted rather than searched.
    #[test]
    fn finds_matches_and_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "hello world\nHELLO again\n").unwrap();
        std::fs::write(root.join("b.bin"), b"\x00\x01hello").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/c.md"), "nothing here\r\nhello\r\n").unwrap();
        let h = start(req(root, "hello")).unwrap();
        let mut files = Vec::new();
        let mut stats = None;
        for ev in h.rx.iter() {
            match ev {
                SearchEvent::File(f) => files.push(f),
                SearchEvent::Done(s) => {
                    stats = Some(s);
                    break;
                }
                SearchEvent::Error(e) => panic!("{e}"),
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].lines.len(), 2);
        assert_eq!(files[0].lines[0].ranges, vec![(0, 5)]);
        assert_eq!(files[1].lines[0].text, "hello");
        let s = stats.unwrap();
        assert_eq!(s.matches, 3);
        assert_eq!(s.skipped_binary, 1);
    }

    /// Replace keeps CRLF endings and refuses when the identity is stale.
    #[test]
    fn replace_preserves_crlf_and_refuses_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, "foo\r\nfoo bar\r\n").unwrap();
        let re = compile(&req(dir.path(), "foo")).unwrap();
        let out = replace_in_file(&p, &re, "qux", None);
        assert_eq!(out.replaced, 2);
        assert_eq!(std::fs::read(&p).unwrap(), b"qux\r\nqux bar\r\n");
        let stale = crate::fileio::identity(&p);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, "foo changed\r\n").unwrap();
        let out = replace_in_file(&p, &re, "z", stale);
        assert!(out.error.is_some());
    }
}
