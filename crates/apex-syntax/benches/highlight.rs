//! Parser and highlighter throughput. Run: `cargo bench -p apex-syntax`.
//!
//! * full parse + highlight of a 100 KB Rust file
//! * incremental re-highlight after a one-character edit in the middle
//! * a single 2 MB line (pathological for naive highlighters)
//!
//! The parse budget is set to ten seconds so every parse completes
//! synchronously; what is measured is parser and query throughput, not the
//! background hand-off.

use std::time::Duration;

use apex_edit::{Change, Document, EditKind, SelectionSet, Transaction};
use apex_syntax::highlight::SyntaxState;
use apex_syntax::language::lookup;
use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// One realistic Rust function, repeated (with unique names) to build the
/// 100 KB source so the parser sees a normal mix of node kinds.
const UNIT: &str = r#"
/// Computes a thing from two inputs and an offset.
pub fn compute(alpha: u32, beta: &str, offset: Option<usize>) -> Result<String, std::io::Error> {
    let mut total = alpha as usize + offset.unwrap_or(0);
    for (i, ch) in beta.chars().enumerate() {
        if ch.is_alphabetic() && i % 2 == 0 {
            total += ch as usize; // comment with `code` and "quotes"
        }
    }
    match total {
        0 => Err(std::io::Error::other("empty")),
        n if n > 1_000_000 => Ok(format!("large: {n:#x}")),
        n => Ok(n.to_string()),
    }
}
"#;

/// Repeat [`UNIT`] until the source is at least `bytes` long, renaming the
/// function each time so the file is valid Rust without duplicate items.
fn rust_source(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + UNIT.len());
    let mut i = 0;
    while s.len() < bytes {
        s.push_str(&UNIT.replace("compute", &format!("compute_{i}")));
        i += 1;
    }
    s
}

/// Request spans for every line, returning the total span count so the
/// result is observable and the work cannot be optimised away.
fn highlight_all(st: &mut SyntaxState, doc: &Document) -> usize {
    let mut spans = 0;
    for line in 0..doc.len_lines() {
        spans += st.spans_for_line(doc, line).len();
    }
    spans
}

/// Register the `rust_100kb` and `single_2mb_line` groups.
fn bench(c: &mut Criterion) {
    let lang = lookup("rust").expect("rust grammar");
    let src = rust_source(100 * 1024);
    let budget = Duration::from_secs(10);

    let mut g = c.benchmark_group("rust_100kb");
    g.throughput(Throughput::Bytes(src.len() as u64));
    g.sample_size(20);
    g.bench_function("full_parse_and_highlight", |b| {
        b.iter_batched(
            || Document::from_text(&src),
            |mut doc| {
                let mut st = SyntaxState::new(lang, &mut doc, budget);
                black_box(highlight_all(&mut st, &doc))
            },
            BatchSize::LargeInput,
        )
    });
    g.bench_function("incremental_edit_and_rehighlight_line", |b| {
        let mut doc = Document::from_text(&src);
        let mut st = SyntaxState::new(lang, &mut doc, budget);
        highlight_all(&mut st, &doc);
        let mid = doc.len_chars() / 2;
        let sels = SelectionSet::default();
        // Alternate insert/delete so the document length stays bounded
        // across iterations instead of growing by one byte each time.
        let mut toggle = false;
        b.iter(|| {
            let tx = if toggle { Transaction::single(Change::delete(mid, mid + 1)) } else { Transaction::single(Change::insert(mid, "x")) };
            toggle = !toggle;
            doc.apply(tx, &sels, &sels, EditKind::Other);
            st.update(&mut doc, budget);
            let line = doc.rope().char_to_line(mid);
            black_box(st.spans_for_line(&doc, line).len())
        })
    });
    g.finish();

    let long_line = format!("let v = vec![{}];\n", "\"item\", 1, 2.5, ".repeat(2 * 1024 * 1024 / 18));
    let mut g = c.benchmark_group("single_2mb_line");
    g.throughput(Throughput::Bytes(long_line.len() as u64));
    g.sample_size(10);
    g.bench_function("parse_and_highlight", |b| {
        b.iter_batched(
            || Document::from_text(&long_line),
            |mut doc| {
                let mut st = SyntaxState::new(lang, &mut doc, budget);
                black_box(st.spans_for_line(&doc, 0).len())
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
