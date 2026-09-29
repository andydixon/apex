#![no_main]
//! Offset conversions must be total and consistent for any text and index.
use apex_buffer::Rope;
use apex_buffer::motion::{next_grapheme_boundary, next_word_end, next_word_start, prev_grapheme_boundary, prev_word_start, snap_to_grapheme, word_at};
use apex_buffer::position::{byte_to_char, char_col_to_display_col, char_to_byte, char_to_utf16_position, display_col_to_char_col, line_end_char, utf16_position_to_char};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (String, u32, u8)| {
    let (text, idx, tab) = data;
    let rope = Rope::from_str(&text);
    let n = rope.len_chars();
    let p = (idx as usize) % (n + 1);
    let tab = (tab as usize % 16) + 1;
    assert_eq!(byte_to_char(&rope, char_to_byte(&rope, p)), p);
    let (line, col16) = char_to_utf16_position(&rope, p);
    let back = utf16_position_to_char(&rope, line, col16);
    assert_eq!(back, p);
    let l = rope.line(line);
    let col = p - rope.line_to_char(line);
    let d = char_col_to_display_col(l, col, tab);
    let c = display_col_to_char_col(l, d, tab);
    assert!(c <= col);
    assert!(line_end_char(&rope, line) >= rope.line_to_char(line));
    let slice = rope.slice(..);
    let s = snap_to_grapheme(slice, p);
    assert!(s <= p);
    assert!(next_grapheme_boundary(slice, p) >= p);
    assert!(prev_grapheme_boundary(slice, p) <= p);
    assert!(next_word_start(slice, p) >= p);
    assert!(next_word_end(slice, p) >= p);
    assert!(prev_word_start(slice, p) <= p);
    let (a, b) = word_at(slice, p);
    assert!(a <= b && b <= n);
});
