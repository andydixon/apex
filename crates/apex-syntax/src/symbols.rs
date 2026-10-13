//! Document symbols from tags queries (and Markdown headings).
//!
//! [`document_symbols`] drives the outline panel and the "go to symbol"
//! picker. For most languages it runs the grammar's tags query (the same
//! `tags.scm` GitHub uses for code navigation) over the root tree and keeps
//! every `@definition.*` capture that has a `@name`. Markdown has no tags
//! query, so its headings are walked directly instead.
//!
//! All ranges are **byte** offsets into the document; `line` is zero-based.
//! Only the root layer is inspected, so symbols inside injected languages
//! (a Rust fence in Markdown) are not reported.

use apex_buffer::Rope;
use tree_sitter::{Node, QueryCursor, StreamingIterator};

use crate::highlight::SyntaxState;

/// Coarse classification of a symbol, used for the outline icon and label.
///
/// Mapped from tags-query capture names by [`kind_of`]; several capture
/// names fold into one kind (e.g. `trait` and `protocol` are `Interface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Method,
    /// Also used for Rust structs and enums, because the Rust tags query
    /// reports them as `definition.class`.
    Class,
    Struct,
    Enum,
    /// Interfaces, traits and protocols.
    Interface,
    /// Modules, namespaces and packages.
    Module,
    /// Type aliases and unions.
    Type,
    /// Constants and macros.
    Constant,
    /// Variables, fields and properties.
    Variable,
    /// A Markdown heading; `depth` is derived from the heading level.
    Heading,
    /// Any `definition.*` capture not listed above.
    Other,
}

impl SymbolKind {
    /// Short lower-case label shown next to the symbol name in the outline.
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::Module => "module",
            Self::Type => "type",
            Self::Constant => "const",
            Self::Variable => "var",
            Self::Heading => "heading",
            Self::Other => "symbol",
        }
    }
}

/// One entry in the document outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The identifier text (or heading title), at most 200 bytes.
    pub name: String,
    pub kind: SymbolKind,
    /// Zero-based line on which the definition starts.
    pub line: usize,
    /// Byte range of the whole definition.
    pub range: (usize, usize),
    /// Byte range of the name token.
    pub name_range: (usize, usize),
    /// Nesting depth for outline display.
    pub depth: u16,
}

/// Map a tags-query capture name such as `definition.function` to a kind.
/// Returns `None` for captures that are not definitions (`name`, `reference.*`).
fn kind_of(capture: &str) -> Option<SymbolKind> {
    let k = capture.strip_prefix("definition.")?;
    Some(match k {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "interface" | "trait" | "protocol" => SymbolKind::Interface,
        "module" | "namespace" | "package" => SymbolKind::Module,
        "type" | "union" | "typealias" => SymbolKind::Type,
        "constant" | "macro" => SymbolKind::Constant,
        "variable" | "field" | "property" => SymbolKind::Variable,
        _ => SymbolKind::Other,
    })
}

/// Extract symbols from the document. Bounded to `limit` entries.
///
/// Returns an empty list when there is no tree yet (background parse in
/// progress) or the language has no tags query. The result is sorted by
/// definition start, de-duplicated on the name token (a query may match the
/// same definition under two patterns), and given a nesting `depth` based on
/// range containment.
pub fn document_symbols(state: &SyntaxState, rope: &Rope, limit: usize) -> Vec<Symbol> {
    let Some(tree) = state.root_tree() else { return Vec::new() };
    let mut out: Vec<Symbol> = Vec::new();
    if state.lang.id == "markdown" {
        let mut cursor = tree.walk();
        let root = tree.root_node();
        for node in root.children(&mut cursor) {
            collect_headings(node, rope, &mut out, limit);
        }
        return out;
    }
    let Some(query) = state.tags_query() else { return out };
    let names = query.capture_names();
    let name_idx = names.iter().position(|n| *n == "name");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), |n: Node| rope.byte_slice(n.byte_range()).chunks().map(|c| c.as_bytes()));
    while let Some(m) = matches.next() {
        let mut def: Option<(SymbolKind, Node)> = None;
        let mut name_node: Option<Node> = None;
        for c in m.captures() {
            let cap = names[c.index as usize];
            if let Some(k) = kind_of(cap) {
                def = Some((k, c.node));
            } else if Some(c.index as usize) == name_idx {
                name_node = Some(c.node);
            }
        }
        let (Some((kind, node)), Some(name)) = (def, name_node) else { continue };
        let text = rope.byte_slice(name.byte_range()).to_string();
        // A huge "name" means the query captured a whole expression (seen
        // with some JS patterns); it would wreck the outline layout.
        if text.is_empty() || text.len() > 200 {
            continue;
        }
        out.push(Symbol {
            name: text,
            kind,
            line: node.start_position().row,
            range: (node.start_byte(), node.end_byte()),
            name_range: (name.start_byte(), name.end_byte()),
            depth: 0,
        });
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by_key(|s| s.range.0);
    out.dedup_by(|a, b| a.name_range == b.name_range);
    // Depth from containment.
    // Symbols are sorted by start, so the stack holds the ranges of every
    // enclosing definition once the ones that ended before `s` are popped.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for s in &mut out {
        while stack.last().is_some_and(|&(_, end)| end <= s.range.0) {
            stack.pop();
        }
        s.depth = stack.len() as u16;
        stack.push(s.range);
    }
    out
}

/// Recursively gather Markdown headings as [`SymbolKind::Heading`] symbols.
///
/// The block grammar nests content in `section` nodes, one per heading, so
/// recursion only descends into sections. Depth is the heading level minus
/// one (`#` is 0). Setext headings (underlined with `=` or `-`) have no `#`
/// and land at depth 0.
fn collect_headings(node: Node, rope: &Rope, out: &mut Vec<Symbol>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    match node.kind() {
        "atx_heading" | "setext_heading" => {
            let text = rope.byte_slice(node.byte_range()).to_string();
            let level = text.chars().take_while(|c| *c == '#').count().max(1);
            // Strip `#` markers, take the first line only (setext headings
            // include their underline), then drop that underline's chars.
            let title = text.trim_start_matches('#').trim().lines().next().unwrap_or("").trim_end_matches(['=', '-']).trim().to_string();
            if !title.is_empty() {
                out.push(Symbol {
                    name: title,
                    kind: SymbolKind::Heading,
                    line: node.start_position().row,
                    range: (node.start_byte(), node.end_byte()),
                    name_range: (node.start_byte(), node.end_byte()),
                    depth: (level - 1) as u16,
                });
            }
        }
        "section" => {
            let mut c = node.walk();
            for child in node.children(&mut c) {
                collect_headings(child, rope, out, limit);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::lookup;
    use apex_edit::Document;
    use std::time::Duration;

    /// Pins the kinds the Rust tags query yields and that nesting follows `mod` blocks.
    #[test]
    fn rust_symbols() {
        let src = "struct S;\nimpl S {\n    fn m(&self) {}\n}\nfn free() {}\nenum E { A }\n";
        let mut doc = Document::from_text(src);
        let s = SyntaxState::new(lookup("rust").unwrap(), &mut doc, Duration::from_secs(5));
        let syms = document_symbols(&s, doc.rope(), 100);
        let names: Vec<(&str, SymbolKind, usize)> = syms.iter().map(|s| (s.name.as_str(), s.kind, s.line)).collect();
        // The Rust tags query reports structs and enums as classes.
        assert!(names.contains(&("S", SymbolKind::Class, 0)), "{names:?}");
        assert!(names.contains(&("m", SymbolKind::Method, 2)), "{names:?}");
        assert!(names.contains(&("free", SymbolKind::Function, 4)), "{names:?}");
        assert!(names.contains(&("E", SymbolKind::Class, 5)), "{names:?}");
        // `impl` blocks are not tagged, so `m` sits at depth 0; modules nest.
        let mut doc = Document::from_text("mod outer {\n    fn inner() {}\n}\n");
        let s = SyntaxState::new(lookup("rust").unwrap(), &mut doc, Duration::from_secs(5));
        let syms = document_symbols(&s, doc.rope(), 100);
        let inner = syms.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!(inner.depth, 1);
    }

    /// Heading level maps to outline depth.
    #[test]
    fn markdown_headings() {
        let mut doc = Document::from_text("# One\n\ntext\n\n## Two\n\n### Three\n");
        let s = SyntaxState::new(lookup("markdown").unwrap(), &mut doc, Duration::from_secs(5));
        let syms = document_symbols(&s, doc.rope(), 100);
        let v: Vec<(String, u16)> = syms.iter().map(|s| (s.name.clone(), s.depth)).collect();
        assert_eq!(v, vec![("One".to_string(), 0), ("Two".to_string(), 1), ("Three".to_string(), 2)]);
    }
}
