#![no_main]
//! Search patterns from users must compile (or fail cleanly) and run in
//! bounded time on any document; replacements must not panic.
use apex_buffer::Rope;
use apex_edit::search::{Query, SearchOptions, preserve_case};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (String, String, String, bool, bool, bool)| {
    let (pattern, text, replacement, regex, case_sensitive, whole_word) = data;
    let rope = Rope::from_str(&text);
    if let Ok(q) = Query::new(&pattern, SearchOptions { case_sensitive, whole_word, regex, in_range: None }) {
        let matches = q.find_all(&rope, 1000);
        for m in &matches {
            assert!(m.range.start <= m.range.end && m.range.end <= rope.len_chars());
            let _ = q.expand_replacement(&rope, m, &replacement);
        }
        let _ = q.is_match(&text);
    }
    let _ = preserve_case(&pattern, &replacement);
});
