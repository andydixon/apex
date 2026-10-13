//! File loading and project search throughput. Run: `cargo bench -p apex-project`.
//!
//! * load + decode of 1 KB, 100 KB, 10 MB and 100 MB files (the 100 MB case is
//!   what the large-file mode opens; set APEX_BENCH_500MB=1 to add 500 MB)
//! * a 1,000,000-line file
//! * project-wide search over a generated 2,000-file tree (10 MB)
//!
//! All inputs are generated into a temporary directory at the start of each
//! group and removed when the `TempDir` drops, so the benchmark needs no
//! fixtures and leaves nothing behind. Sample sizes are small (10) because
//! the large cases take seconds each. Throughput is reported in bytes for
//! the size series and the search, and in elements (lines) for the
//! million-line case.

use std::path::PathBuf;

use apex_buffer::Encoding;
use apex_project::fileio;
use apex_project::search::{SearchEvent, SearchRequest, start};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

/// Create `dir/name` by repeating `line` until it is at least `bytes` long
/// (so the result slightly exceeds `bytes`), and return its path.
fn write_file(dir: &std::path::Path, name: &str, bytes: usize, line: &str) -> PathBuf {
    let p = dir.join(name);
    let mut s = String::with_capacity(bytes + line.len());
    while s.len() < bytes {
        s.push_str(line);
    }
    std::fs::write(&p, s).expect("write bench file");
    p
}

/// `fileio::load` across file sizes plus the million-line case. Measures the
/// read, encoding detection, UTF-8 decode and line-ending scan together, as
/// the editor does when opening a tab.
fn bench_load(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let line = "let value = compute(alpha, beta) + offset; // a fairly typical line of code\n";
    let mut sizes = vec![("1kb", 1 << 10), ("100kb", 100 << 10), ("10mb", 10 << 20), ("100mb", 100 << 20)];
    if std::env::var_os("APEX_BENCH_500MB").is_some() {
        sizes.push(("500mb", 500 << 20));
    }
    let fallback = Encoding::Legacy(encoding_rs::WINDOWS_1252);
    let mut g = c.benchmark_group("load");
    g.sample_size(10);
    for (name, bytes) in sizes {
        let p = write_file(dir.path(), &format!("{name}.rs"), bytes, line);
        g.throughput(Throughput::Bytes(bytes as u64));
        g.bench_function(name, |b| b.iter(|| black_box(fileio::load(&p, None, fallback).expect("load").text.len())));
    }
    // One million short lines.
    let p = write_file(dir.path(), "million.txt", 0, "");
    {
        let mut s = String::with_capacity(12_000_000);
        for i in 0..1_000_000 {
            s.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&p, &s).unwrap();
        g.throughput(Throughput::Elements(1_000_000));
    }
    g.bench_function("1_million_lines", |b| b.iter(|| black_box(fileio::load(&p, None, fallback).expect("load").text.len())));
    g.finish();
}

/// Find in Files over 2,000 generated 5 KiB files in 40 directories: a
/// literal that hits every file, a regex that hits every file, and a literal
/// that hits none (pure walk + read + `is_match` cost).
fn bench_search(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let line = "fn handler(request: Request) -> Response { let needle = 42; respond(request, needle) }\n";
    let mut total = 0usize;
    for i in 0..2000 {
        let sub = dir.path().join(format!("mod{}", i % 40));
        std::fs::create_dir_all(&sub).unwrap();
        let p = write_file(&sub, &format!("file{i}.rs"), 5 * 1024, line);
        total += std::fs::metadata(&p).unwrap().len() as usize;
    }
    let req = |pattern: &str, regex: bool| SearchRequest {
        roots: vec![dir.path().to_path_buf()],
        pattern: pattern.into(),
        regex,
        case_sensitive: true,
        whole_word: false,
        include: Vec::new(),
        exclude: Vec::new(),
        hidden: false,
        respect_gitignore: true,
        follow_symlinks: false,
        max_file_bytes: 64 << 20,
        max_results: 1_000_000,
        context_lines: 0,
    };
    // Drive a search to completion and count matching lines, so the
    // measurement includes draining the channel as the UI would.
    let run = |r: SearchRequest| {
        let h = start(r).expect("search");
        let mut matches = 0;
        for ev in h.rx.iter() {
            match ev {
                SearchEvent::File(f) => matches += f.lines.len(),
                SearchEvent::Done(_) => break,
                SearchEvent::Error(e) => panic!("{e}"),
            }
        }
        matches
    };
    let mut g = c.benchmark_group("find_in_files_2000_files");
    g.throughput(Throughput::Bytes(total as u64));
    g.sample_size(10);
    g.bench_function("literal_needle", |b| b.iter(|| black_box(run(req("needle", false)))));
    g.bench_function("regex_respond_call", |b| b.iter(|| black_box(run(req(r"respond\(\w+, \w+\)", true)))));
    g.bench_function("no_match", |b| b.iter(|| black_box(run(req("zzzz_not_here", false)))));
    g.finish();
}

criterion_group!(benches, bench_load, bench_search);
criterion_main!(benches);
