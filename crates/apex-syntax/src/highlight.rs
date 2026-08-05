//! Incremental parsing and per-line highlight spans.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use apex_buffer::Rope;
use apex_edit::{ByteEdit, Document};
use crossbeam_channel::{Receiver, bounded};
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Query, QueryCursor, Range, StreamingIterator, Tree};

use crate::language::{LanguageDef, lookup};

/// A highlighted byte range within one line, with a theme scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub scope: &'static str,
}

/// Injected content larger than this is left unhighlighted to bound work.
const MAX_INJECTION_BYTES: usize = 1 << 20;
/// Documents larger than this are never parsed synchronously.
const SYNC_PARSE_LIMIT: usize = 256 * 1024;

struct CompiledQueries {
    highlights: Query,
    injections: Option<Query>,
    tags: Option<Query>,
    /// Theme scope per highlight capture index (`None` for non-highlight captures).
    scopes: Vec<Option<&'static str>>,
}

type QueryCache = Mutex<HashMap<&'static str, Option<Arc<CompiledQueries>>>>;
static QUERY_CACHE: OnceLock<QueryCache> = OnceLock::new();

/// Map a capture name to the theme's scope vocabulary.
fn canonical_scope(name: &str) -> &'static str {
    let mapped = match name {
        "escape" => "string.escape",
        "text.title" | "text.title.1" | "text.title.2" | "text.title.3" | "text.title.4" | "text.title.5" | "text.title.6" | "markup.heading.1"
        | "markup.heading.2" | "markup.heading.3" => "markup.heading",
        "text.uri" | "text.reference" | "markup.link.url" | "markup.link.label" | "markup.link.text" => "markup.link",
        "text.literal" | "markup.raw.block" | "markup.raw.inline" => "markup.raw",
        "text.emphasis" | "markup.italic" => "markup.italic",
        "text.strong" | "markup.strong" => "markup.bold",
        "text.quote" => "markup.quote",
        "delimiter" => "punctuation.delimiter",
        "media" | "keyframes" | "import" | "charset" | "supports" | "namespace.keyword" => "keyword",
        "comment.documentation" => "comment",
        "string.special.key" => "property",
        "string.special.symbol" => "constant",
        "tag.error" => "error",
        "none" => "",
        "spell" | "nospell" | "conceal" => "",
        other => other,
    };
    intern(mapped)
}

fn intern(s: &str) -> &'static str {
    static INTERNED: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut map = INTERNED.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = map.get(s) {
        return v;
    }
    // Bounded by the number of distinct capture names across all queries.
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    map.insert(s.to_owned(), leaked);
    leaked
}

fn compiled(lang: &'static LanguageDef) -> Option<Arc<CompiledQueries>> {
    let cache = QUERY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(lang.id) {
        return v.clone();
    }
    let built = build_queries(lang).map(Arc::new);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(lang.id, built.clone());
    built
}

fn build_queries(lang: &'static LanguageDef) -> Option<CompiledQueries> {
    let g = lang.grammar.as_ref()?;
    let ts_lang = (g.language)();
    let highlights = match Query::new(&ts_lang, g.highlights) {
        Ok(q) => q,
        Err(e) => {
            tracing::error!("{}: highlight query failed to compile: {e}", lang.id);
            return None;
        }
    };
    let scopes = highlights
        .capture_names()
        .iter()
        .map(|n| {
            let s = canonical_scope(n);
            if s.is_empty() || n.starts_with("injection.") || n.starts_with("local.") || n.starts_with('_') { None } else { Some(s) }
        })
        .collect();
    let injections = g.injections.and_then(|src| Query::new(&ts_lang, src).map_err(|e| tracing::warn!("{}: injections query failed: {e}", lang.id)).ok());
    let tags = g.tags.and_then(|src| Query::new(&ts_lang, src).map_err(|e| tracing::warn!("{}: tags query failed: {e}", lang.id)).ok());
    Some(CompiledQueries { highlights, injections, tags, scopes })
}

/// One parsed layer: the root document or an injected sub-language region.
struct Layer {
    lang: &'static LanguageDef,
    queries: Arc<CompiledQueries>,
    tree: Tree,
    /// Byte range of the injected content (whole document for the root).
    range: std::ops::Range<usize>,
    depth: u8,
}

struct BgResult {
    generation: u64,
    revision: u64,
    tree: Option<Tree>,
}

/// Per-document syntax state.
pub struct SyntaxState {
    pub lang: &'static LanguageDef,
    parser: Parser,
    root: Option<Layer>,
    children: Vec<Layer>,
    /// Document revision the trees have been edited up to.
    revision: u64,
    line_cache: HashMap<usize, Vec<Span>>,
    cache_valid: bool,
    bg: Option<Receiver<BgResult>>,
    bg_generation: u64,
    edits_since_bg: Vec<InputEdit>,
    /// Edits not yet applied because there is no tree to apply them to.
    pub last_parse: Duration,
    pub enabled: bool,
}

fn to_input_edit(e: &ByteEdit) -> InputEdit {
    InputEdit {
        start_byte: e.start_byte,
        old_end_byte: e.old_end_byte,
        new_end_byte: e.new_end_byte,
        start_position: Point::new(e.start_point.0, e.start_point.1),
        old_end_position: Point::new(e.old_end_point.0, e.old_end_point.1),
        new_end_position: Point::new(e.new_end_point.0, e.new_end_point.1),
    }
}

fn parse_rope(parser: &mut Parser, rope: &Rope, old: Option<&Tree>, budget: Option<Duration>) -> Option<Tree> {
    let start = Instant::now();
    let len = rope.len_bytes();
    let mut read = |byte: usize, _pt: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    match budget {
        Some(b) => {
            let mut over_budget = |_: &tree_sitter::ParseState| if start.elapsed() > b { ControlFlow::Break(()) } else { ControlFlow::Continue(()) };
            let opts = ParseOptions::new().progress_callback(&mut over_budget);
            parser.parse_with_options(&mut read, old, Some(opts))
        }
        None => parser.parse_with_options(&mut read, old, None),
    }
}

impl SyntaxState {
    /// Create state for a document; parses immediately within `budget` and
    /// otherwise starts a background parse.
    pub fn new(lang: &'static LanguageDef, doc: &mut Document, budget: Duration) -> Self {
        let mut s = Self {
            lang,
            parser: Parser::new(),
            root: None,
            children: Vec::new(),
            revision: doc.revision(),
            line_cache: HashMap::new(),
            cache_valid: false,
            bg: None,
            bg_generation: 0,
            edits_since_bg: Vec::new(),
            last_parse: Duration::ZERO,
            enabled: (lang.grammar.is_some() || lang.simple.is_some()) && !doc.large_file,
        };
        // Edits before we existed are irrelevant: we parse the current text.
        doc.take_pending_byte_edits();
        if s.enabled && lang.grammar.is_some() {
            s.reparse(doc, budget);
        }
        s
    }

    pub fn has_tree(&self) -> bool {
        self.root.is_some()
    }

    pub fn is_parsing_in_background(&self) -> bool {
        self.bg.is_some()
    }

    /// Bring the trees up to date with the document. Call once per frame
    /// before asking for spans. Returns true if highlights changed.
    pub fn update(&mut self, doc: &mut Document, budget: Duration) -> bool {
        if !self.enabled || self.lang.grammar.is_none() {
            let edited = doc.has_pending_byte_edits();
            doc.take_pending_byte_edits();
            if edited {
                self.cache_valid = false;
            }
            return edited;
        }
        let mut changed = false;
        let edits: Vec<InputEdit> = doc.take_pending_byte_edits().iter().map(to_input_edit).collect();
        if !edits.is_empty() {
            for e in &edits {
                if let Some(r) = &mut self.root {
                    r.tree.edit(e);
                }
                for c in &mut self.children {
                    c.tree.edit(e);
                }
            }
            if self.bg.is_some() {
                self.edits_since_bg.extend(edits.iter().copied());
            }
            self.revision = doc.revision();
            self.cache_valid = false;
            changed = true;
        }
        // Collect a finished background parse.
        if let Some(rx) = &self.bg {
            match rx.try_recv() {
                Ok(res) if res.generation == self.bg_generation => {
                    self.bg = None;
                    if let Some(mut tree) = res.tree {
                        for e in &self.edits_since_bg {
                            tree.edit(e);
                        }
                        self.edits_since_bg.clear();
                        self.install_root(doc, tree);
                        // The tree is edited but not reparsed for edits made
                        // during the background parse; do that now (fast).
                        if res.revision != doc.revision() {
                            self.reparse(doc, budget);
                        }
                        changed = true;
                    }
                }
                Ok(_) => self.bg = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.bg = None,
            }
        }
        if (!edits.is_empty() || self.root.is_none()) && self.bg.is_none() {
            self.reparse(doc, budget);
            changed = true;
        }
        changed
    }

    fn reparse(&mut self, doc: &Document, budget: Duration) {
        let Some(queries) = compiled(self.lang) else {
            self.enabled = false;
            return;
        };
        let Some(g) = self.lang.grammar.as_ref() else { return };
        let ts_lang = (g.language)();
        if self.parser.set_language(&ts_lang).is_err() {
            tracing::error!("{}: grammar ABI mismatch; highlighting disabled", self.lang.id);
            self.enabled = false;
            return;
        }
        let _ = self.parser.set_included_ranges(&[]);
        let rope = doc.rope();
        let old = self.root.as_ref().map(|r| &r.tree);
        let sync_ok = old.is_some() || rope.len_bytes() <= SYNC_PARSE_LIMIT;
        let start = Instant::now();
        let tree = if sync_ok { parse_rope(&mut self.parser, rope, old, Some(budget)) } else { None };
        self.last_parse = start.elapsed();
        match tree {
            Some(t) => {
                let _ = queries;
                self.install_root(doc, t);
            }
            None => self.start_background(doc),
        }
    }

    fn start_background(&mut self, doc: &Document) {
        let Some(g) = self.lang.grammar.as_ref() else { return };
        self.bg_generation += 1;
        let generation = self.bg_generation;
        let revision = doc.revision();
        let rope = doc.rope().clone();
        let old = self.root.as_ref().map(|r| r.tree.clone());
        let language = g.language;
        let (tx, rx) = bounded(1);
        self.bg = Some(rx);
        self.edits_since_bg.clear();
        let lang_id = self.lang.id;
        std::thread::Builder::new()
            .name(format!("apex-parse-{lang_id}"))
            .spawn(move || {
                let mut parser = Parser::new();
                let tree = if parser.set_language(&language()).is_ok() { parse_rope(&mut parser, &rope, old.as_ref(), None) } else { None };
                let _ = tx.send(BgResult { generation, revision, tree });
            })
            .map_err(|e| tracing::error!("could not spawn parser thread: {e}"))
            .ok();
    }

    fn install_root(&mut self, doc: &Document, tree: Tree) {
        let Some(queries) = compiled(self.lang) else { return };
        let len = doc.len_bytes();
        self.root = Some(Layer { lang: self.lang, queries, tree, range: 0..len, depth: 0 });
        self.cache_valid = false;
        self.rebuild_injections(doc);
    }

    /// Re-parse injected regions, reusing existing child trees incrementally
    /// when the region (language and range) is unchanged.
    fn rebuild_injections(&mut self, doc: &Document) {
        let Some(root) = &self.root else { return };
        let Some(inj) = root.queries.injections.as_ref() else {
            self.children.clear();
            return;
        };
        let rope = doc.rope();
        let names = inj.capture_names();
        let content_idx = names.iter().position(|n| *n == "injection.content");
        let language_idx = names.iter().position(|n| *n == "injection.language");
        let Some(content_idx) = content_idx else {
            self.children.clear();
            return;
        };
        let mut wanted: Vec<(&'static LanguageDef, Range)> = Vec::new();
        let mut total = 0usize;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(inj, root.tree.root_node(), |n: Node| rope.byte_slice(n.byte_range()).chunks().map(|c| c.as_bytes()));
        while let Some(m) = matches.next() {
            let mut lang_name: Option<String> =
                inj.property_settings(m.pattern_index).iter().find(|p| &*p.key == "injection.language").and_then(|p| p.value.as_ref().map(|v| v.to_string()));
            if lang_name.is_none()
                && let Some(li) = language_idx
                && let Some(c) = m.captures().iter().find(|c| c.index as usize == li)
            {
                lang_name = Some(rope.byte_slice(c.node.byte_range()).to_string());
            }
            let Some(name) = lang_name else { continue };
            let Some(lang) = lookup(name.trim()) else { continue };
            if lang.grammar.is_none() || lang.id == self.lang.id {
                continue;
            }
            for c in m.captures().iter().filter(|c| c.index as usize == content_idx) {
                let r = c.node.range();
                if r.end_byte <= r.start_byte {
                    continue;
                }
                total += r.end_byte - r.start_byte;
                if total > MAX_INJECTION_BYTES {
                    tracing::debug!("injection limit reached; remaining regions unhighlighted");
                    break;
                }
                wanted.push((lang, r));
            }
        }
        drop(matches);
        let mut old = std::mem::take(&mut self.children);
        let mut new_children = Vec::with_capacity(wanted.len());
        for (lang, range) in wanted {
            let Some(queries) = compiled(lang) else { continue };
            let Some(g) = lang.grammar.as_ref() else { continue };
            let byte_range = range.start_byte..range.end_byte;
            let reuse = old.iter().position(|c| c.lang.id == lang.id && c.range == byte_range).map(|i| old.swap_remove(i));
            if self.parser.set_language(&(g.language)()).is_err() || self.parser.set_included_ranges(&[range]).is_err() {
                continue;
            }
            let tree = parse_rope(&mut self.parser, rope, reuse.as_ref().map(|c| &c.tree), None);
            if let Some(tree) = tree {
                new_children.push(Layer { lang, queries, tree, range: byte_range, depth: 1 });
            }
        }
        let _ = self.parser.set_included_ranges(&[]);
        self.children = new_children;
    }

    /// Highlight spans for one line (line-local byte offsets).
    pub fn spans_for_line(&mut self, doc: &Document, line: usize) -> &[Span] {
        if !self.cache_valid {
            self.line_cache.clear();
            self.cache_valid = true;
        }
        if !self.line_cache.contains_key(&line) {
            let spans = self.compute_line(doc, line);
            if self.line_cache.len() > 8192 {
                self.line_cache.clear();
            }
            self.line_cache.insert(line, spans);
        }
        self.line_cache.get(&line).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn compute_line(&self, doc: &Document, line: usize) -> Vec<Span> {
        let rope = doc.rope();
        if line >= rope.len_lines() {
            return Vec::new();
        }
        if let Some(simple) = &self.lang.simple {
            return crate::simple::highlight_line(simple, &doc.line_text(line));
        }
        let ls = rope.line_to_byte(line);
        // Exclude the terminator so spans map onto the displayed line text.
        let le = rope.char_to_byte(apex_buffer::position::line_end_char(rope, line));
        if le <= ls {
            return Vec::new();
        }
        // (depth, pattern index, start, end, scope) collected across layers.
        let mut raw: Vec<(u8, usize, usize, usize, &'static str)> = Vec::new();
        let layers = self.root.iter().chain(self.children.iter().filter(|c| c.range.start < le && c.range.end > ls));
        for layer in layers {
            let mut cursor = QueryCursor::new();
            cursor.set_byte_range(ls..le);
            let mut matches =
                cursor.matches(&layer.queries.highlights, layer.tree.root_node(), |n: Node| rope.byte_slice(n.byte_range()).chunks().map(|c| c.as_bytes()));
            while let Some(m) = matches.next() {
                for c in m.captures() {
                    let Some(scope) = layer.queries.scopes.get(c.index as usize).copied().flatten() else { continue };
                    let s = c.node.start_byte().max(ls);
                    let e = c.node.end_byte().min(le);
                    if s < e {
                        raw.push((layer.depth, m.pattern_index, s - ls, e - ls, scope));
                    }
                }
            }
        }
        if raw.is_empty() {
            return Vec::new();
        }
        // For identical ranges in one layer the later pattern wins (tree-sitter
        // highlight semantics); deeper layers win over shallower ones. Then
        // paint larger ranges first so inner captures override outer ones.
        raw.sort_by_key(|r| (r.2, r.3, r.0, r.1));
        raw.dedup_by(|later, earlier| {
            if later.2 == earlier.2 && later.3 == earlier.3 {
                *earlier = *later;
                true
            } else {
                false
            }
        });
        raw.sort_by_key(|r| std::cmp::Reverse(r.3 - r.2));
        let width = le - ls;
        let mut cover: Vec<u16> = vec![u16::MAX; width];
        let mut scopes: Vec<&'static str> = Vec::new();
        for (_, _, s, e, scope) in raw {
            let idx = match scopes.iter().position(|x| *x == scope) {
                Some(i) => i as u16,
                None => {
                    scopes.push(scope);
                    (scopes.len() - 1) as u16
                }
            };
            for c in &mut cover[s..e] {
                *c = idx;
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < width {
            let idx = cover[i];
            let mut j = i + 1;
            while j < width && cover[j] == idx {
                j += 1;
            }
            if idx != u16::MAX {
                out.push(Span { start: i, end: j, scope: scopes[idx as usize] });
            }
            i = j;
        }
        out
    }

    /// Byte ranges of the named nodes enclosing `byte`, innermost first.
    pub fn enclosing_ranges(&self, byte: usize) -> Vec<(usize, usize)> {
        let Some(root) = &self.root else { return Vec::new() };
        let mut out = Vec::new();
        let mut node = root.tree.root_node().descendant_for_byte_range(byte, byte);
        while let Some(n) = node {
            let r = (n.start_byte(), n.end_byte());
            if n.is_named() && out.last() != Some(&r) && r.1 > r.0 {
                out.push(r);
            }
            node = n.parent();
        }
        out
    }

    pub(crate) fn root_tree(&self) -> Option<&Tree> {
        self.root.as_ref().map(|r| &r.tree)
    }

    pub(crate) fn tags_query(&self) -> Option<&Query> {
        self.root.as_ref().and_then(|r| r.queries.tags.as_ref())
    }

    /// The root tree has syntax errors (useful for a status indicator).
    pub fn has_errors(&self) -> bool {
        self.root.as_ref().is_some_and(|r| r.tree.root_node().has_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_edit::{Change, EditKind, SelectionSet, Transaction};

    fn state_for(lang: &str, text: &str) -> (SyntaxState, Document) {
        let mut doc = Document::from_text(text);
        let s = SyntaxState::new(lookup(lang).unwrap(), &mut doc, Duration::from_secs(5));
        (s, doc)
    }

    fn scopes(s: &mut SyntaxState, d: &Document, line: usize) -> Vec<(String, &'static str)> {
        let text = d.line_text(line);
        s.spans_for_line(d, line).iter().map(|sp| (text.as_bytes()[sp.start..sp.end].iter().map(|&b| b as char).collect(), sp.scope)).collect()
    }

    #[test]
    fn rust_highlights() {
        let (mut s, d) = state_for("rust", "fn main() {\n    let x = \"hi\"; // c\n}\n");
        let l0 = scopes(&mut s, &d, 0);
        assert!(l0.contains(&("fn".into(), "keyword")), "{l0:?}");
        assert!(l0.contains(&("main".into(), "function")), "{l0:?}");
        let l1 = scopes(&mut s, &d, 1);
        assert!(l1.iter().any(|(t, sc)| t == "\"hi\"" && *sc == "string"), "{l1:?}");
        assert!(l1.iter().any(|(t, sc)| t == "// c" && *sc == "comment"), "{l1:?}");
    }

    #[test]
    fn incremental_edit_keeps_highlights_right() {
        let (mut s, mut d) = state_for("rust", "fn a() {}\n");
        let sels = SelectionSet::default();
        d.apply(Transaction::single(Change::insert(0, "// top\n")), &sels, &sels, EditKind::Other);
        assert!(s.update(&mut d, Duration::from_secs(5)));
        let l0 = scopes(&mut s, &d, 0);
        assert_eq!(l0, vec![("// top".into(), "comment")]);
        let l1 = scopes(&mut s, &d, 1);
        assert!(l1.contains(&("fn".into(), "keyword")), "{l1:?}");
    }

    #[test]
    fn markdown_injects_inline_and_code_blocks() {
        let (mut s, d) = state_for("markdown", "# Title\n\nSome *emph* text\n\n```rust\nfn x() {}\n```\n");
        let l0 = scopes(&mut s, &d, 0);
        assert!(l0.iter().any(|(_, sc)| *sc == "markup.heading"), "{l0:?}");
        let l2 = scopes(&mut s, &d, 2);
        assert!(l2.iter().any(|(t, sc)| t.contains("emph") && *sc == "markup.italic"), "{l2:?}");
        let l5 = scopes(&mut s, &d, 5);
        assert!(l5.contains(&("fn".into(), "keyword")), "{l5:?}");
    }

    #[test]
    fn html_injects_script_and_style() {
        let (mut s, d) = state_for("html", "<html><script>\nlet x = 1;\n</script><style>\na { color: red }\n</style></html>\n");
        let l1 = scopes(&mut s, &d, 1);
        assert!(l1.contains(&("let".into(), "keyword")), "{l1:?}");
        let l3 = scopes(&mut s, &d, 3);
        assert!(l3.iter().any(|(t, _)| t == "color"), "{l3:?}");
    }

    #[test]
    fn background_parse_for_large_document() {
        let text = "fn f() { let x = 1; }\n".repeat(20_000);
        let mut doc = Document::from_text(&text);
        let mut s = SyntaxState::new(lookup("rust").unwrap(), &mut doc, Duration::from_millis(1));
        // Either it finished within a millisecond (unlikely) or it went to the background.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !s.has_tree() && Instant::now() < deadline {
            s.update(&mut doc, Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(s.has_tree());
        let l = scopes(&mut s, &doc, 19_999);
        assert!(l.contains(&("fn".into(), "keyword")), "{l:?}");
    }

    #[test]
    fn enclosing_ranges_walk_up() {
        let (s, _d) = state_for("rust", "fn a() { b(c); }\n");
        let r = s.enclosing_ranges(11); // inside `c`
        assert!(r.len() >= 3);
        assert!(r.windows(2).all(|w| w[1].0 <= w[0].0 && w[1].1 >= w[0].1));
    }
}
