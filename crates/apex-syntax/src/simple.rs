//! Declarative fallback highlighter: keywords, comments, strings and numbers.
//!
//! Used for languages that have no usable tree-sitter grammar. It is
//! deliberately line-local (no multi-line strings or comments), which keeps it
//! O(line length) and safe on any input.
//!
//! Called from `SyntaxState::compute_line` when the document's language has a
//! `simple` definition and no grammar. Spans use **byte** offsets relative to
//! the start of the line, the same convention as tree-sitter highlights. Only
//! ASCII is classified; non-ASCII bytes are skipped one at a time and never
//! split a UTF-8 sequence because span boundaries always fall on ASCII bytes.

use crate::highlight::Span;
use crate::language::SimpleSyntax;

/// Highlight one line (without its terminator) according to `syn`.
///
/// Tokens are recognised left to right with no backtracking: a comment start
/// ends the scan, a string runs to its unescaped closing quote (or the end of
/// the line), identifiers are checked against the keyword list, and digit
/// runs (with `.` and alphanumerics, so `1.80` and `0xFF` stay whole) are
/// numbers. Anything else produces no span.
pub fn highlight_line(syn: &SimpleSyntax, line: &str) -> Vec<Span> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // Section headings ([name] / # Heading style) if configured.
    if let Some(p) = syn.section_prefix
        && line.trim_start().starts_with(p)
    {
        return vec![Span { start: 0, end: bytes.len(), scope: "markup.heading" }];
    }
    while i < bytes.len() {
        let rest = &line[i..];
        if let Some(lc) = syn.line_comment
            && rest.starts_with(lc)
        {
            out.push(Span { start: i, end: bytes.len(), scope: "comment" });
            break;
        }
        let c = bytes[i];
        if syn.strings.contains(&(c as char)) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != c {
                // Skip the escaped char so `\"` does not close the string.
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            // `j` sits on the closing quote (or past the end for an unterminated string).
            let end = (j + 1).min(bytes.len());
            out.push(Span { start: i, end, scope: "string" });
            i = end;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            // `-` is part of a word so `apt-get` is one token and never a keyword prefix match.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-') {
                j += 1;
            }
            let word = &line[i..j];
            let is_kw = syn.keywords.iter().any(|k| if syn.case_insensitive { k.eq_ignore_ascii_case(word) } else { *k == word });
            if is_kw {
                out.push(Span { start: i, end: j, scope: "keyword" });
            }
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.') {
                j += 1;
            }
            out.push(Span { start: i, end: j, scope: "number" });
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::lookup;

    /// Keywords, a trailing comment, and a `#` inside a string that is not a comment.
    #[test]
    fn dockerfile_lines() {
        let syn = lookup("dockerfile").unwrap().simple.as_ref().unwrap();
        let spans = highlight_line(syn, "FROM rust:1.80 AS build # base");
        assert_eq!(spans[0], Span { start: 0, end: 4, scope: "keyword" });
        assert!(spans.iter().any(|s| s.scope == "comment" && s.start == 24));
        assert!(spans.iter().any(|s| s.scope == "keyword" && &"FROM rust:1.80 AS build # base"[s.start..s.end] == "AS"));
        let spans = highlight_line(syn, "RUN echo \"a # not comment\" # real");
        assert!(spans.iter().any(|s| s.scope == "string"));
        assert_eq!(spans.iter().filter(|s| s.scope == "comment").count(), 1);
    }
}
