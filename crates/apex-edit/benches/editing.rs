//! Editing and search benchmarks. Run: `cargo bench -p apex-edit`.
//!
//! * 1,000 cursors typing one character into a 1 MB document
//! * undo/redo of a batch of edits
//! * position mapping through a 1,000-change transaction
//! * literal and regular-expression search over 10 MB

use apex_buffer::Rope;
use apex_edit::ops::{self, InsertOptions};
use apex_edit::search::{Query, SearchOptions};
use apex_edit::{Assoc, Change, Document, EditKind, Selection, SelectionSet, Transaction};
use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};

fn text_of(bytes: usize) -> String {
    let line = "    let value = compute(alpha, beta) + offset; // trailing comment with words\n";
    let mut s = String::with_capacity(bytes + line.len());
    while s.len() < bytes {
        s.push_str(line);
    }
    s
}

fn cursors(doc: &Document, n: usize) -> SelectionSet {
    let rope = doc.rope();
    let lines = rope.len_lines().saturating_sub(1).max(1);
    let step = (lines / n).max(1);
    let sels: Vec<Selection> = (0..n).map(|i| Selection::point(rope.line_to_char((i * step) % lines) + 4)).collect();
    SelectionSet::from_vec(sels, 0)
}

fn bench_cursors(c: &mut Criterion) {
    let src = text_of(1 << 20);
    let mut g = c.benchmark_group("multi_cursor");
    for &n in &[1usize, 100, 1000] {
        g.bench_function(format!("type_char_{n}_cursors"), |b| {
            b.iter_batched(
                || Document::from_text(&src),
                |mut doc| {
                    let sels = cursors(&doc, n);
                    let r = ops::insert_text(&doc, &sels, "x", InsertOptions { auto_pair: true, auto_indent: true, overwrite: false }).expect("edit");
                    doc.apply(r.tx, &sels, &r.sels, EditKind::Typing);
                    black_box(doc.len_chars())
                },
                BatchSize::LargeInput,
            )
        });
    }
    g.finish();
}

fn bench_undo(c: &mut Criterion) {
    let mut doc = Document::from_text(text_of(1 << 20));
    let sels = cursors(&doc, 100);
    let mut cur = sels.clone();
    for i in 0..50 {
        let t = ((b'a' + (i % 26) as u8) as char).to_string();
        let r = ops::insert_text(&doc, &cur, &t, InsertOptions { auto_pair: false, auto_indent: false, overwrite: false }).expect("edit");
        doc.history_mut().mark_boundary();
        doc.apply(r.tx, &cur, &r.sels, EditKind::Other);
        cur = r.sels;
    }
    c.bench_function("undo_redo_50_steps_100_cursors", |b| {
        b.iter(|| {
            for _ in 0..50 {
                doc.undo();
            }
            for _ in 0..50 {
                doc.redo();
            }
            black_box(doc.revision())
        })
    });
}

fn bench_map_pos(c: &mut Criterion) {
    let n = 1 << 20;
    let changes: Vec<Change> = (0..1000).map(|i| Change::replace(i * 1000, i * 1000 + 3, "longer text")).collect();
    let tx = Transaction::new(changes);
    let positions: Vec<usize> = (0..10_000).map(|i| (i * 104_729) % n).collect();
    let mut g = c.benchmark_group("transaction");
    g.throughput(Throughput::Elements(positions.len() as u64));
    g.bench_function("map_10k_positions_through_1000_changes", |b| b.iter(|| positions.iter().map(|&p| tx.map_pos(p, Assoc::After)).fold(0, |a, x| a ^ x)));
    g.bench_function("apply_1000_changes_1mb", |b| {
        b.iter_batched(|| Rope::from_str(&text_of(n)), |mut rope| black_box(tx.apply(&mut rope).0.len_delta()), BatchSize::LargeInput)
    });
    g.finish();
}

fn bench_search(c: &mut Criterion) {
    let text = text_of(10 << 20);
    let rope = Rope::from_str(&text);
    let mut g = c.benchmark_group("search_10mb");
    g.throughput(Throughput::Bytes(text.len() as u64));
    g.sample_size(10);
    let literal = Query::new("offset", SearchOptions { case_sensitive: true, whole_word: false, regex: false, in_range: None }).unwrap();
    g.bench_function("literal", |b| b.iter(|| black_box(literal.find_all(&rope, usize::MAX).len())));
    let ci = Query::new("OFFSET", SearchOptions { case_sensitive: false, whole_word: true, regex: false, in_range: None }).unwrap();
    g.bench_function("case_insensitive_whole_word", |b| b.iter(|| black_box(ci.find_all(&rope, usize::MAX).len())));
    let re = Query::new(r"comp\w+\(([a-z]+), ([a-z]+)\)", SearchOptions { case_sensitive: true, whole_word: false, regex: true, in_range: None }).unwrap();
    g.bench_function("regex_with_groups", |b| b.iter(|| black_box(re.find_all(&rope, usize::MAX).len())));
    g.finish();
}

criterion_group!(benches, bench_cursors, bench_undo, bench_map_pos, bench_search);
criterion_main!(benches);
