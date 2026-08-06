//! Find in Files: streaming, cancellable, bounded-memory search across roots.
//!
//! Files are read one at a time per worker; nothing is kept in memory beyond
//! the current file and the results. Binary files (NUL in the first 8 KiB)
//! and files over `max_file_bytes` are skipped and counted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::index::{WalkOptions, walker};

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub roots: Vec<PathBuf>,
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub hidden: bool,
    pub respect_gitignore: bool,
    pub follow_symlinks: bool,
    pub max_file_bytes: u64,
    pub max_results: usize,
    pub context_lines: usize,
}

#[derive(Debug, Clone)]
pub struct FileMatches {
    pub path: PathBuf,
    pub lines: Vec<LineMatch>,
}

#[derive(Debug, Clone)]
pub struct LineMatch {
    /// Zero-based line number.
    pub line: usize,
    pub text: String,
    /// Byte ranges within `text`.
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Debug)]
pub enum SearchEvent {
    File(FileMatches),
    Done(SearchStats),
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub files_scanned: usize,
    pub files_matched: usize,
    pub matches: usize,
    pub skipped_binary: usize,
    pub skipped_large: usize,
    pub unreadable: usize,
    pub elapsed: std::time::Duration,
    pub truncated: bool,
}

pub struct SearchHandle {
    pub rx: Receiver<SearchEvent>,
    cancel: Arc<AtomicBool>,
    pub scanned: Arc<AtomicUsize>,
}

impl SearchHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Compile the request's pattern the same way in-document search does.
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

pub fn start(req: SearchRequest) -> Result<SearchHandle, String> {
    if req.pattern.is_empty() {
        return Err("empty pattern".into());
    }
    let re = compile(&req)?;
    let (tx, rx) = bounded::<SearchEvent>(256);
    let cancel = Arc::new(AtomicBool::new(false));
    let scanned = Arc::new(AtomicUsize::new(0));
    let (c, s) = (cancel.clone(), scanned.clone());
    std::thread::Builder::new().name("apex-search".into()).spawn(move || run(req, re, tx, c, s)).map_err(|e| e.to_string())?;
    Ok(SearchHandle { rx, cancel, scanned })
}

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
    let mut s = stats.into_inner().unwrap_or_default();
    s.files_scanned = scanned.load(Ordering::Relaxed);
    s.elapsed = start.elapsed();
    s.truncated = total_matches.load(Ordering::Relaxed) >= req.max_results;
    let _ = tx.send(SearchEvent::Done(s));
}

enum Skip {
    Binary,
    Large,
    Unreadable,
}

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
    let line_of = |byte: usize| line_starts.partition_point(|&s| s <= byte).saturating_sub(1);
    for m in re.find_iter(&text) {
        if m.start() == m.end() {
            continue;
        }
        let line = line_of(m.start());
        let ls = line_starts[line];
        let le = text[ls..].find('\n').map(|i| ls + i).unwrap_or(text.len());
        let le = if text[..le].ends_with('\r') { le - 1 } else { le };
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

fn build_globs(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns.iter().filter(|p| !p.trim().is_empty()).filter_map(|p| globset::Glob::new(p.trim()).ok().map(|g| g.compile_matcher())).collect()
}

/// Outcome of replacing in one file.
#[derive(Debug, Clone)]
pub struct ReplaceOutcome {
    pub path: PathBuf,
    pub replaced: usize,
    pub error: Option<String>,
}

/// Replace all matches of `re` in `path` with `replacement`, preserving the
/// file's encoding and line endings and refusing if it changed since
/// `expected` was taken. Writes through `fileio::save`.
pub fn replace_in_file(path: &Path, re: &regex::Regex, replacement: &str, expected: Option<apex_edit::FileIdentity>) -> ReplaceOutcome {
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
