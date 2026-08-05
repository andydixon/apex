//! Declarative fallback highlighter: keywords, comments, strings and numbers.
//!
//! Used for languages that have no usable tree-sitter grammar. It is
//! deliberately line-local (no multi-line strings or comments), which keeps it
//! O(line length) and safe on any input.

use crate::highlight::Span;
use crate::language::SimpleSyntax;

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
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(bytes.len());
            out.push(Span { start: i, end, scope: "string" });
            i = end;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
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
