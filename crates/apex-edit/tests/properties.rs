//! Property-based tests for the editing core.

use apex_buffer::motion::{is_grapheme_boundary, next_grapheme_boundary, prev_grapheme_boundary};
use apex_edit::ops::{self, InsertOptions};
use apex_edit::{Assoc, Change, Document, EditKind, IndentSettings, Selection, SelectionSet, Transaction};
use proptest::prelude::*;

/// Random document text of up to 20 fragments, weighted towards ASCII words
/// but including every awkward case the editor must not split.
fn text_strategy() -> impl Strategy<Value = String> {
    // Mix of ASCII, CRLF, tabs, combining marks, emoji ZWJ sequences and CJK.
    prop::collection::vec(
        prop_oneof![
            8 => "[a-z ]{1,6}",
            2 => Just("\n".to_string()),
            1 => Just("\r\n".to_string()),
            1 => Just("\t".to_string()),
            1 => Just("e\u{301}".to_string()),
            1 => Just("👨‍👩‍👧".to_string()),
            1 => Just("漢字".to_string()),
            1 => Just("🇬🇧".to_string()),
        ],
        0..20,
    )
    .prop_map(|v| v.concat())
}

/// One random edit, positions in char indices; `change_of` clamps them to the
/// document.
#[derive(Debug, Clone)]
enum Op {
    /// Insert the text at the position.
    Insert(usize, String),
    /// Delete the range `start..end` (start never after end).
    Delete(usize, usize),
    /// Replace the range `start..end` with the text.
    Replace(usize, usize, String),
}

/// Random `Op` with positions in `0..200`, so many land past the end and
/// exercise clamping.
fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..200usize, text_strategy()).prop_map(|(a, t)| Op::Insert(a, t)),
        (0..200usize, 0..200usize).prop_map(|(a, b)| Op::Delete(a.min(b), a.max(b))),
        (0..200usize, 0..200usize, text_strategy()).prop_map(|(a, b, t)| Op::Replace(a.min(b), a.max(b), t)),
    ]
}

/// Turn `op` into a `Change` for a document of `len` chars, clamping both
/// ends. Positions are not snapped to grapheme boundaries.
fn change_of(op: &Op, len: usize) -> Change {
    match op {
        Op::Insert(a, t) => Change::insert((*a).min(len), t.clone()),
        Op::Delete(a, b) => Change::delete((*a).min(len), (*b).min(len)),
        Op::Replace(a, b, t) => Change::replace((*a).min(len), (*b).min(len), t.clone()),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Any sequence of edits followed by the same number of undos restores the
    /// original text, and redoing them all restores the final text.
    #[test]
    fn undo_redo_round_trip(initial in text_strategy(), ops in prop::collection::vec(op_strategy(), 1..12)) {
        let mut doc = Document::from_text(&initial);
        let sels = SelectionSet::default();
        let mut snapshots = vec![doc.text()];
        for op in &ops {
            let c = change_of(op, doc.len_chars());
            // `EditKind::Other` never coalesces, so every applied op is one entry.
            // No-op changes (empty delete) are dropped and record nothing.
            if doc.apply(Transaction::single(c), &sels, &sels, EditKind::Other) {
                snapshots.push(doc.text());
            }
        }
        let final_text = doc.text();
        let mut i = snapshots.len() - 1;
        while doc.undo().is_some() {
            i -= 1;
            prop_assert_eq!(doc.text(), snapshots[i].clone());
        }
        prop_assert_eq!(doc.text(), initial);
        while doc.redo().is_some() {}
        prop_assert_eq!(doc.text(), final_text);
    }

    /// Mapping a position through a transaction gives the same answer as
    /// applying the transaction and tracking where the character went.
    #[test]
    fn map_pos_matches_actual_edit(initial in text_strategy(), ops in prop::collection::vec(op_strategy(), 1..4), pos in 0..100usize) {
        let rope = apex_buffer::Rope::from_str(&initial);
        let len = rope.len_chars();
        let pos = pos.min(len);
        let changes: Vec<Change> = ops.iter().map(|o| change_of(o, len)).collect();
        let tx = Transaction::new(changes);
        let mut after = rope.clone();
        tx.apply(&mut after);
        let before_pos = tx.map_pos(pos, Assoc::Before);
        let after_pos = tx.map_pos(pos, Assoc::After);
        prop_assert!(before_pos <= after.len_chars());
        prop_assert!(after_pos <= after.len_chars());
        prop_assert!(before_pos <= after_pos);
        // Text before an untouched position is preserved up to the first change.
        if let Some(first) = tx.changes().first()
            && pos < first.start {
                prop_assert_eq!(before_pos, pos);
                prop_assert_eq!(after_pos, pos);
            }
    }

    /// Multi-cursor insert keeps every selection valid, on a grapheme
    /// boundary, sorted, and non-overlapping; the text grows exactly as expected.
    #[test]
    fn multi_cursor_insert_invariants(initial in text_strategy(), positions in prop::collection::vec(0..100usize, 1..10), insert in "[a-z]{1,3}") {
        let doc = Document::from_text(&initial);
        let len = doc.len_chars();
        let slice = doc.slice();
        let sels: Vec<Selection> = positions.iter().map(|p| {
            let p = (*p).min(len);
            Selection::point(apex_buffer::motion::snap_to_grapheme(slice, p))
        }).collect();
        let sels = SelectionSet::from_vec(sels, 0);
        let n = sels.len();
        let mut doc2 = Document::from_text(&initial);
        let r = ops::insert_text(&doc2, &sels, &insert, InsertOptions { auto_pair: false, auto_indent: false, overwrite: false }).unwrap();
        doc2.apply(r.tx, &sels, &r.sels, EditKind::Typing);
        prop_assert_eq!(doc2.len_chars(), len + n * insert.chars().count());
        let s2 = doc2.slice();
        let mut prev_end = 0;
        for (i, s) in r.sels.iter().enumerate() {
            prop_assert!(s.end() <= doc2.len_chars());
            prop_assert!(is_grapheme_boundary(s2, s.head));
            if i > 0 {
                prop_assert!(s.start() >= prev_end);
            }
            prev_end = s.end();
            // The inserted text sits immediately before each caret.
            let before: String = doc2.rope().slice(s.head - insert.chars().count()..s.head).into();
            prop_assert_eq!(before, insert.clone());
        }
    }

    /// Grapheme stepping forwards then backwards returns to the same offsets.
    #[test]
    fn grapheme_steps_are_reversible(text in text_strategy()) {
        let rope = apex_buffer::Rope::from_str(&text);
        let s = rope.slice(..);
        let mut i = 0;
        let mut forward = vec![0];
        while i < s.len_chars() {
            let n = next_grapheme_boundary(s, i);
            prop_assert!(n > i);
            forward.push(n);
            i = n;
        }
        let mut back = vec![s.len_chars()];
        let mut j = s.len_chars();
        while j > 0 {
            let p = prev_grapheme_boundary(s, j);
            prop_assert!(p < j);
            back.push(p);
            j = p;
        }
        back.reverse();
        prop_assert_eq!(forward, back);
    }

    /// Delete-backward at every caret removes exactly one grapheme (or the
    /// smart-backspace indent) and never splits a cluster.
    #[test]
    fn delete_backward_is_grapheme_safe(text in text_strategy(), pos in 0..100usize) {
        let doc = Document::from_text(&text);
        let p = apex_buffer::motion::snap_to_grapheme(doc.slice(), pos.min(doc.len_chars()));
        let sels = SelectionSet::single(Selection::point(p));
        if let Some(r) = ops::delete_backward(&doc, &sels, IndentSettings::default(), false) {
            let mut d2 = Document::from_text(&text);
            d2.apply(r.tx, &sels, &r.sels, EditKind::DeleteBackward);
            let s = d2.slice();
            prop_assert!(is_grapheme_boundary(s, r.sels.primary().head));
            prop_assert_eq!(d2.len_chars(), doc.len_chars() - (p - prev_grapheme_boundary(doc.slice(), p)));
            // The remaining text is valid UTF-8 by construction, and the line
            // count only ever drops by at most one.
            prop_assert!(d2.len_lines() + 1 >= doc.len_lines());
        }
    }

    /// Line-ending normalisation is idempotent and preserves line count.
    #[test]
    fn line_ending_normalisation(text in text_strategy(), target in prop_oneof![Just(apex_buffer::LineEnding::Lf), Just(apex_buffer::LineEnding::CrLf), Just(apex_buffer::LineEnding::Cr)]) {
        let once = apex_buffer::line_ending::normalise(&text, target);
        let twice = apex_buffer::line_ending::normalise(&once, target);
        prop_assert_eq!(&once, &twice);
        let stats = apex_buffer::line_ending::scan(&once, usize::MAX);
        prop_assert!(!stats.is_mixed());
        let lines_before = apex_buffer::Rope::from_str(&text).len_lines();
        let lines_after = apex_buffer::Rope::from_str(&once).len_lines();
        prop_assert_eq!(lines_before, lines_after);
    }

    /// Encoding round trips for all Unicode-capable encodings.
    #[test]
    fn unicode_encodings_round_trip(text in text_strategy()) {
        for enc in [apex_buffer::Encoding::Utf8, apex_buffer::Encoding::Utf8Bom, apex_buffer::Encoding::Utf16Le, apex_buffer::Encoding::Utf16Be] {
            let (bytes, lossy) = apex_buffer::encode(&text, enc);
            prop_assert!(!lossy);
            let back = apex_buffer::decode(&bytes, None, apex_buffer::Encoding::Utf8);
            prop_assert_eq!(back.text, text.clone());
            prop_assert_eq!(back.encoding, enc);
        }
    }

    /// Byte/char/line conversions agree with a naive reference.
    #[test]
    fn offset_conversions_agree_with_reference(text in text_strategy(), idx in 0..200usize) {
        let rope = apex_buffer::Rope::from_str(&text);
        let ci = idx.min(rope.len_chars());
        let byte = apex_buffer::position::char_to_byte(&rope, ci);
        let reference_byte = text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len());
        prop_assert_eq!(byte, reference_byte);
        prop_assert_eq!(apex_buffer::position::byte_to_char(&rope, byte), ci);
        // Line/column and UTF-16 positions are clamped to the line's content
        // (never inside a terminator), so only round-trip positions at or
        // before the line end.
        let line = rope.char_to_line(ci);
        if ci <= apex_buffer::position::line_end_char(&rope, line) {
            let pos = apex_buffer::Position::from_char(&rope, ci);
            prop_assert_eq!(pos.to_char(&rope), ci);
            let (l, u16col) = apex_buffer::position::char_to_utf16_position(&rope, ci);
            prop_assert_eq!(apex_buffer::position::utf16_position_to_char(&rope, l, u16col), ci);
        }
    }
}
