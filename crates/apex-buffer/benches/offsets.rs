//! Offset conversions on a 1 MB mixed-width text. Run: `cargo bench -p apex-buffer`.
//!
//! The sample mixes ASCII, tabs, Latin-1, CJK and an emoji so that every
//! conversion path (byte/char/UTF-16/display column/grapheme) is exercised on
//! realistic data rather than pure ASCII. Each benchmark folds its results
//! with XOR so the optimiser cannot discard the work.

use apex_buffer::Rope;
use apex_buffer::motion::{next_grapheme_boundary, next_word_start, prev_grapheme_boundary};
use apex_buffer::position::{byte_to_char, char_col_to_display_col, char_to_byte, char_to_utf16_position, display_col_to_char_col};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

/// Build roughly 1 MiB of text by repeating one line that contains a tab,
/// accented Latin letters, CJK and an emoji.
fn sample_text() -> String {
    let line = "fn main() {\tlet naïve = \"héllo wörld\"; // 日本語のコメント 😀 end\n";
    let mut s = String::with_capacity(1 << 20);
    while s.len() < 1 << 20 {
        s.push_str(line);
    }
    s
}

/// Register the `offsets` group: 10,000 pseudo-random char positions
/// (stride 7919, a prime, so they spread across the whole rope) converted
/// through each offset space.
fn bench(c: &mut Criterion) {
    let text = sample_text();
    let rope = Rope::from_str(&text);
    let n = rope.len_chars();
    let positions: Vec<usize> = (0..10_000).map(|i| (i * 7919) % n).collect();

    let mut g = c.benchmark_group("offsets");
    g.throughput(Throughput::Elements(positions.len() as u64));
    g.bench_function("char_to_byte", |b| b.iter(|| positions.iter().map(|&p| char_to_byte(&rope, p)).fold(0, |a, x| a ^ x)));
    g.bench_function("byte_to_char", |b| {
        let bytes: Vec<usize> = positions.iter().map(|&p| char_to_byte(&rope, p)).collect();
        b.iter(|| bytes.iter().map(|&p| byte_to_char(&rope, p)).fold(0, |a, x| a ^ x))
    });
    g.bench_function("utf16_position", |b| b.iter(|| positions.iter().map(|&p| char_to_utf16_position(&rope, p).1).fold(0, |a, x| a ^ x)));
    g.bench_function("display_col_round_trip", |b| {
        b.iter(|| {
            positions
                .iter()
                .map(|&p| {
                    let line = rope.char_to_line(p);
                    let l = rope.line(line);
                    let col = p - rope.line_to_char(line);
                    let d = char_col_to_display_col(l, col, 4);
                    display_col_to_char_col(l, d, 4)
                })
                .fold(0, |a, x| a ^ x)
        })
    });
    g.bench_function("grapheme_next_prev", |b| {
        let slice = rope.slice(..);
        b.iter(|| positions.iter().map(|&p| prev_grapheme_boundary(slice, next_grapheme_boundary(slice, p))).fold(0, |a, x| a ^ x))
    });
    g.bench_function("word_motion", |b| {
        let slice = rope.slice(..);
        b.iter(|| positions.iter().map(|&p| next_word_start(slice, black_box(p))).fold(0, |a, x| a ^ x))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
