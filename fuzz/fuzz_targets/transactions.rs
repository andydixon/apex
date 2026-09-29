#![no_main]
//! Random edit sequences must apply, invert and map positions without
//! panicking, and undo must restore the exact original text.
use apex_edit::{Assoc, Change, Document, EditKind, SelectionSet, Transaction};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Case {
    text: String,
    edits: Vec<(u16, u16, String)>,
    probes: Vec<u16>,
}

fuzz_target!(|case: Case| {
    let mut doc = Document::from_text(&case.text);
    let original = doc.text();
    let sels = SelectionSet::default();
    let mut applied = 0;
    for (a, b, t) in case.edits.iter().take(32) {
        let n = doc.len_chars();
        let (a, b) = ((*a as usize) % (n + 1), (*b as usize) % (n + 1));
        let (a, b) = (a.min(b), a.max(b));
        let tx = Transaction::single(Change::replace(a, b, t.clone()));
        for p in case.probes.iter().take(8) {
            let p = (*p as usize) % (n + 1);
            let m = tx.map_pos(p, Assoc::After);
            assert!(m <= n + t.chars().count());
            let _ = tx.map_pos(p, Assoc::Before);
        }
        doc.history_mut().mark_boundary();
        if doc.apply(tx, &sels, &sels, EditKind::Other) {
            applied += 1;
        }
    }
    for _ in 0..applied {
        doc.undo();
    }
    assert_eq!(doc.text(), original, "undo must restore the original text");
    for _ in 0..applied {
        doc.redo();
    }
});
