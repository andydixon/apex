//! In-document search with plain, whole-word and regex modes.
//!
//! Regex execution uses the `regex` crate, which guarantees linear-time
//! matching and has no backreferences or look-around, so attacker-controlled
//! documents cannot cause catastrophic backtracking. Pattern compilation is
//! bounded by a size limit so absurd patterns fail fast with a clear error.

use std::ops::Range;

use ropey::Rope;
use serde::{Deserialize, Serialize};

/// User-facing search toggles. The default is a plain, case-insensitive
/// search over the whole document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchOptions {
    /// Match case exactly; otherwise the regex is compiled case-insensitive.
    pub case_sensitive: bool,
    /// Only match at word boundaries (`\b` on both sides of the pattern).
    pub whole_word: bool,
    /// Treat the pattern as a regular expression rather than literal text.
    pub regex: bool,
    /// Restrict the search to this char range (find in selection).
    #[serde(skip)]
    pub in_range: Option<(usize, usize)>,
}

/// Why a search query could not be compiled.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SearchError {
    /// The pattern was empty, invalid or too large; holds a short message
    /// suitable for showing to the user.
    #[error("invalid regular expression: {0}")]
    Regex(String),
}

/// A compiled query. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Query {
    /// Compiled form of `pattern` with the options applied.
    re: regex::Regex,
    /// Options the query was compiled with.
    pub options: SearchOptions,
    /// The pattern exactly as the user typed it.
    pub pattern: String,
}

/// A match in char offsets, with capture groups (char offsets) for replacement.
///
/// Capture groups are not stored; `Query::expand_replacement` recomputes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Char index range of the matched text in the whole document.
    pub range: Range<usize>,
}

impl Query {
    /// Compile `pattern` with `options`.
    ///
    /// Literal patterns are escaped; whole-word wraps the pattern in `\b`.
    /// `^` and `$` match at line boundaries. Returns `SearchError::Regex` for an
    /// empty pattern or one that fails to compile within the size limits.
    pub fn new(pattern: &str, options: SearchOptions) -> Result<Self, SearchError> {
        if pattern.is_empty() {
            return Err(SearchError::Regex("empty pattern".into()));
        }
        let mut src = if options.regex { pattern.to_owned() } else { regex::escape(pattern) };
        if options.whole_word {
            src = format!(r"\b(?:{src})\b");
        }
        let re = regex::RegexBuilder::new(&src)
            .case_insensitive(!options.case_sensitive)
            .multi_line(true)
            // Compiled-program and lazy-DFA caps (bytes) so a huge pattern errors
            // out instead of consuming memory.
            .size_limit(10 * 1024 * 1024)
            .dfa_size_limit(2 * 1024 * 1024)
            .build()
            .map_err(|e| SearchError::Regex(friendly_regex_error(&e)))?;
        Ok(Self { re, options, pattern: pattern.to_owned() })
    }

    /// Find every match. The rope is materialised once; for large documents
    /// this is bounded by [`crate::document::LARGE_FILE_BYTES`] by the caller.
    ///
    /// Returns at most `limit` non-empty matches in ascending order, with char
    /// index ranges relative to the whole document (also when
    /// `SearchOptions::in_range` is set).
    pub fn find_all(&self, rope: &Rope, limit: usize) -> Vec<Match> {
        let (text, base_char, base_byte) = self.haystack(rope);
        let mut out = Vec::new();
        // Convert byte offsets to char offsets incrementally (matches are ascending).
        let mut last_byte = 0usize;
        let mut last_char = 0usize;
        for m in self.re.find_iter(&text) {
            if m.start() == m.end() {
                continue; // skip empty matches so Replace All terminates
            }
            last_char += text[last_byte..m.start()].chars().count();
            let start = last_char;
            last_char += text[m.start()..m.end()].chars().count();
            last_byte = m.end();
            out.push(Match { range: base_char + start..base_char + last_char });
            if out.len() >= limit {
                break;
            }
        }
        // The byte base is not needed; offsets are rebuilt from char counts.
        let _ = base_byte;
        out
    }

    /// Text to search, plus the char index and byte offset in `rope` at which
    /// it starts. Honours `SearchOptions::in_range`, clamped to the document.
    fn haystack(&self, rope: &Rope) -> (String, usize, usize) {
        match self.options.in_range {
            Some((a, b)) => {
                let a = a.min(rope.len_chars());
                let b = b.clamp(a, rope.len_chars());
                (rope.slice(a..b).to_string(), a, rope.char_to_byte(a))
            }
            None => (rope.to_string(), 0, 0),
        }
    }

    /// Expand a replacement template (`$1`, `${name}`, `$$`) for `m`.
    ///
    /// Non-regex queries return `template` unchanged. For regex queries the
    /// pattern is re-run on the matched text alone, so captures come from that
    /// slice; if it no longer matches in isolation (for example a `\b` that
    /// depended on surrounding text), `template` is returned unexpanded.
    pub fn expand_replacement(&self, rope: &Rope, m: &Match, template: &str) -> String {
        if !self.options.regex {
            return template.to_owned();
        }
        let s: String = rope.slice(m.range.clone()).into();
        let mut out = String::new();
        if let Some(c) = self.re.captures(&s) {
            c.expand(template, &mut out);
        } else {
            out.push_str(template);
        }
        out
    }

    /// Does the whole text match?
    ///
    /// Despite the wording, this is an unanchored test: it returns `true` if
    /// the query matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.re.is_match(text)
    }
}

/// Turn a `regex` compile error into a one-line message for the find bar.
fn friendly_regex_error(e: &regex::Error) -> String {
    match e {
        regex::Error::Syntax(s) => {
            // Keep the first line of the syntax diagnostic; it names the problem.
            s.lines()
                .find(|l| l.starts_with("error:"))
                .map(|l| l.trim_start_matches("error:").trim().to_owned())
                .unwrap_or_else(|| s.lines().last().unwrap_or("").trim().to_owned())
        }
        regex::Error::CompiledTooBig(_) => "pattern is too large to compile".into(),
        _ => e.to_string(),
    }
}

/// Adjust the case of `replacement` to mimic `original` (Preserve Case).
///
/// All-caps original gives an upper-case replacement; a leading capital
/// capitalises only the first character; anything else gives lower case.
pub fn preserve_case(original: &str, replacement: &str) -> String {
    if original.chars().all(|c| !c.is_lowercase()) && original.chars().any(|c| c.is_uppercase()) {
        replacement.to_uppercase()
    } else if original.chars().next().is_some_and(|c| c.is_uppercase()) {
        let mut cs = replacement.chars();
        match cs.next() {
            Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
            None => String::new(),
        }
    } else {
        replacement.to_lowercase()
    }
}

/// Next match at or after `from` (wrapping when allowed), as an index into `matches`.
///
/// `from` is a char index; `matches` must be sorted by start. With `wrap`,
/// falls back to the first match when none starts at or after `from`.
pub fn next_match_index(matches: &[Match], from: usize, wrap: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    match matches.iter().position(|m| m.range.start >= from) {
        Some(i) => Some(i),
        None if wrap => Some(0),
        None => None,
    }
}

/// Previous match strictly before `before`, as an index into `matches`.
///
/// `before` is a char index; `matches` must be sorted by start. With
/// `wrap`, falls back to the last match.
pub fn prev_match_index(matches: &[Match], before: usize, wrap: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    match matches.iter().rposition(|m| m.range.start < before) {
        Some(i) => Some(i),
        None if wrap => Some(matches.len() - 1),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins down that the default is case-insensitive and the toggle restricts it.
    #[test]
    fn plain_search_is_case_insensitive_by_default() {
        let r = Rope::from_str("Foo foo FOO");
        let q = Query::new("foo", SearchOptions::default()).unwrap();
        assert_eq!(q.find_all(&r, usize::MAX).len(), 3);
        let q = Query::new("foo", SearchOptions { case_sensitive: true, ..Default::default() }).unwrap();
        assert_eq!(q.find_all(&r, usize::MAX).len(), 1);
    }

    /// Pins down whole-word filtering and char (not byte) ranges after
    /// multi-byte text.
    #[test]
    fn whole_word_and_unicode_offsets() {
        let r = Rope::from_str("é cat concat cat");
        let q = Query::new("cat", SearchOptions { whole_word: true, ..Default::default() }).unwrap();
        let m = q.find_all(&r, usize::MAX);
        assert_eq!(m.iter().map(|m| m.range.clone()).collect::<Vec<_>>(), vec![2..5, 13..16]);
    }

    /// Pins down that syntax errors are reduced to a readable message.
    #[test]
    fn regex_errors_are_friendly() {
        let e = Query::new("(", SearchOptions { regex: true, ..Default::default() }).unwrap_err();
        assert!(e.to_string().contains("unclosed group"), "{e}");
    }

    /// Pins down `$n` capture expansion and the three preserve-case rules.
    #[test]
    fn replacement_expansion_and_preserve_case() {
        let r = Rope::from_str("2024-01-02");
        let q = Query::new(r"(\d+)-(\d+)-(\d+)", SearchOptions { regex: true, ..Default::default() }).unwrap();
        let m = &q.find_all(&r, 10)[0];
        assert_eq!(q.expand_replacement(&r, m, "$3/$2/$1"), "02/01/2024");
        assert_eq!(preserve_case("Hello", "world"), "World");
        assert_eq!(preserve_case("HELLO", "world"), "WORLD");
        assert_eq!(preserve_case("hello", "World"), "world");
    }

    /// Pins down that find-in-selection only reports matches inside the range,
    /// at document-relative positions.
    #[test]
    fn in_range_search() {
        let r = Rope::from_str("aaaa");
        let q = Query::new("a", SearchOptions { in_range: Some((1, 3)), ..Default::default() }).unwrap();
        assert_eq!(q.find_all(&r, 100).iter().map(|m| m.range.start).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// Pins down next/previous match lookup with and without wrapping.
    #[test]
    fn navigation_helpers() {
        let ms = vec![Match { range: 0..1 }, Match { range: 5..6 }];
        assert_eq!(next_match_index(&ms, 1, true), Some(1));
        assert_eq!(next_match_index(&ms, 6, true), Some(0));
        assert_eq!(next_match_index(&ms, 6, false), None);
        assert_eq!(prev_match_index(&ms, 5, true), Some(0));
        assert_eq!(prev_match_index(&ms, 0, true), Some(1));
    }
}
