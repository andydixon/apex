//! Incremental parsing and per-line highlight spans.
//!
//! [`SyntaxState`] is owned by the application, one per open document. Each
//! frame the app calls [`SyntaxState::update`], which drains the document's
//! pending [`ByteEdit`]s into the tree-sitter trees (`Tree::edit`), reparses
//! within a time budget (or hands off to a background thread for large
//! files), and rebuilds injection layers for embedded languages. The painter
//! then calls [`SyntaxState::spans_for_line`] per visible line and gets
//! theme scopes over **byte** offsets relative to that line.
//!
//! Data flow: `LanguageDef` → `compiled()` (process-wide query cache) →
//! `Layer` (root tree plus one per injected region) → `compute_line()` runs
//! the highlights query of every layer touching the line, resolves overlaps,
//! and the result is memoised in `line_cache` until the next edit.
//!
//! Invariants: `root` and `children` are always edited (`Tree::edit`) for
//! every byte edit the document has produced, even when no reparse happened
//! yet, so their byte positions are consistent with the current rope. A tree
//! that is edited but not reparsed is "stale but aligned" and still yields
//! usable highlights. `cache_valid == false` means `line_cache` must be
//! cleared before use.

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
///
/// Spans for a line never overlap and are sorted by `start`; gaps between
/// them are unstyled text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset from the start of the line (terminator excluded).
    pub start: usize,
    /// Exclusive end byte offset from the start of the line.
    pub end: usize,
    /// Theme scope such as `keyword` or `string.escape`; interned, so
    /// comparing pointers is enough but `==` on the str is what callers use.
    pub scope: &'static str,
}

/// Injected content larger than this is left unhighlighted to bound work.
const MAX_INJECTION_BYTES: usize = 1 << 20;
/// Documents larger than this are never parsed synchronously.
///
/// Applies only to the first parse of a document (no old tree to reuse);
/// incremental reparses are fast enough to attempt inline regardless of size.
const SYNC_PARSE_LIMIT: usize = 256 * 1024;

/// A language's queries compiled against its grammar, shared process-wide
/// via [`QUERY_CACHE`] because `Query::new` is expensive (milliseconds).
struct CompiledQueries {
    highlights: Query,
    injections: Option<Query>,
    tags: Option<Query>,
    /// Theme scope per highlight capture index (`None` for non-highlight captures).
    scopes: Vec<Option<&'static str>>,
}

/// Language id → compiled queries, or `None` if compilation failed (cached
/// too, so a broken query is logged once rather than every frame).
type QueryCache = Mutex<HashMap<&'static str, Option<Arc<CompiledQueries>>>>;
/// Process-wide instance of [`QueryCache`], initialised on first use.
static QUERY_CACHE: OnceLock<QueryCache> = OnceLock::new();

/// Map a capture name to the theme's scope vocabulary.
///
/// Grammars from different authors use different naming conventions
/// (`text.title` vs `markup.heading.1`); the theme only knows the canonical
/// set, so aliases are folded here. An empty result means "no highlight".
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

/// Return a `'static` copy of `s`, allocating (and leaking) at most once per
/// distinct string. Lets `Span::scope` be `&'static str` without lifetimes.
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

/// Fetch (compiling on first use) the queries for `lang`. `None` when the
/// language has no grammar or its highlights query does not compile.
fn compiled(lang: &'static LanguageDef) -> Option<Arc<CompiledQueries>> {
    let cache = QUERY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(lang.id) {
        return v.clone();
    }
    let built = build_queries(lang).map(Arc::new);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(lang.id, built.clone());
    built
}

/// Compile `lang`'s queries. The highlights query is mandatory; injections
/// and tags are optional and a failure there only loses that feature.
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
            // `injection.*`, `local.*` and `_private` captures are query
            // plumbing, not things to paint.
            if s.is_empty() || n.starts_with("injection.") || n.starts_with("local.") || n.starts_with('_') { None } else { Some(s) }
        })
        .collect();
    let injections = g.injections.and_then(|src| Query::new(&ts_lang, src).map_err(|e| tracing::warn!("{}: injections query failed: {e}", lang.id)).ok());
    let tags = g.tags.and_then(|src| Query::new(&ts_lang, src).map_err(|e| tracing::warn!("{}: tags query failed: {e}", lang.id)).ok());
    Some(CompiledQueries { highlights, injections, tags, scopes })
}

/// One parsed layer: the root document or an injected sub-language region.
///
/// Child layers are parsed with `set_included_ranges` so their trees carry
/// document-absolute byte positions, which is what lets `compute_line` query
/// every layer with the same `ls..le` window.
struct Layer {
    lang: &'static LanguageDef,
    queries: Arc<CompiledQueries>,
    tree: Tree,
    /// Byte range of the injected content (whole document for the root).
    range: std::ops::Range<usize>,
    /// 0 for the root, 1 for injections. Deeper layers win when spans overlap.
    depth: u8,
}

/// Message from a background parse thread back to [`SyntaxState::update`].
struct BgResult {
    /// Copy of `bg_generation` at spawn time; a mismatch means a newer parse
    /// superseded this one and its tree must be dropped.
    generation: u64,
    /// Document revision the snapshot was taken at.
    revision: u64,
    /// `None` if the grammar failed to load on the worker.
    tree: Option<Tree>,
}

/// Per-document syntax state.
///
/// Created by the application when a document is opened or its language
/// changes; dropped with the document. Not `Sync`: all access happens on the
/// UI thread, and the only cross-thread traffic is the `bg` channel.
pub struct SyntaxState {
    /// The language being parsed; swap by constructing a new state.
    pub lang: &'static LanguageDef,
    /// Reused for root and injection parses (language switched as needed).
    parser: Parser,
    /// The document-level tree, or `None` before the first parse completes.
    root: Option<Layer>,
    /// Injection layers sorted by `range.start`.
    children: Vec<Layer>,
    /// Document revision the trees have been edited up to.
    revision: u64,
    /// Memoised `spans_for_line` results, keyed by line; cleared on any edit.
    line_cache: HashMap<usize, Vec<Span>>,
    /// False when `line_cache` holds results from a previous tree.
    cache_valid: bool,
    /// Receiver for an in-flight background parse, if any.
    bg: Option<Receiver<BgResult>>,
    /// Incremented per background parse so late results can be discarded.
    bg_generation: u64,
    /// Edits made while a background parse was running, replayed onto its
    /// tree when it arrives so the tree matches the current text.
    edits_since_bg: Vec<InputEdit>,
    /// Wall-clock time of the last synchronous parse attempt (for the status bar / probes).
    pub last_parse: Duration,
    /// Time spent rebuilding injection layers in the last update.
    pub last_injections: Duration,
    /// Byte ranges (post-edit) touched since the last injection rebuild.
    pending_dirty: Vec<std::ops::Range<usize>>,
    /// False when there is nothing to highlight (no grammar and no simple
    /// syntax, large-file mode, or the grammar failed to load).
    pub enabled: bool,
}

/// Sort and merge overlapping or touching byte ranges.
fn merge_ranges(mut v: Vec<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
    v.retain(|r| r.end > r.start);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<std::ops::Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Convert the document's edit record into tree-sitter's `InputEdit`
/// (same fields; `Point` rows and columns are zero-based bytes).
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

/// Parse `rope` without copying it, feeding tree-sitter chunk by chunk.
///
/// `old` enables incremental reuse (it must already have been told about
/// the edits). With a `budget`, the parse is abandoned and `None` returned
/// once that much wall-clock time has elapsed, so the caller can fall back
/// to a background parse.
fn parse_rope(parser: &mut Parser, rope: &Rope, old: Option<&Tree>, budget: Option<Duration>) -> Option<Tree> {
    let start = Instant::now();
    let len = rope.len_bytes();
    // tree-sitter asks for "the bytes starting at `byte`"; returning the rest
    // of the containing chunk lets it read large runs with few callbacks.
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
            last_injections: Duration::ZERO,
            pending_dirty: Vec::new(),
            // Large-file mode disables highlighting entirely to keep the editor responsive.
            enabled: (lang.grammar.is_some() || lang.simple.is_some()) && !doc.large_file,
        };
        // Edits before we existed are irrelevant: we parse the current text.
        doc.take_pending_byte_edits();
        if s.enabled && lang.grammar.is_some() {
            s.reparse(doc, budget);
        }
        s
    }

    /// True once a root tree exists (highlights and folds are available).
    pub fn has_tree(&self) -> bool {
        self.root.is_some()
    }

    /// True while a worker thread is parsing; the status bar shows a spinner.
    pub fn is_parsing_in_background(&self) -> bool {
        self.bg.is_some()
    }

    /// Bring the trees up to date with the document. Call once per frame
    /// before asking for spans. Returns true if highlights changed.
    ///
    /// Steps: (1) drain the document's pending byte edits into every tree
    /// and shift injection ranges and dirty ranges accordingly; (2) collect a
    /// finished background parse, replaying edits made meanwhile; (3) if
    /// anything changed and no background parse is running, reparse within
    /// `budget`. Side effect: always consumes `doc`'s pending byte edits.
    pub fn update(&mut self, doc: &mut Document, budget: Duration) -> bool {
        // No tree to maintain (simple syntax or disabled): just invalidate
        // the line cache and discard the edit records.
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
                let delta = e.new_end_byte as isize - e.old_end_byte as isize;
                for c in &mut self.children {
                    c.tree.edit(e);
                    // Keep the injected region's byte range in step with the text.
                    // Entirely after the edit: shift. Overlapping: grow/shrink
                    // the end but never below the start.
                    if c.range.start >= e.old_end_byte {
                        c.range.start = (c.range.start as isize + delta).max(0) as usize;
                        c.range.end = (c.range.end as isize + delta).max(0) as usize;
                    } else if c.range.end > e.start_byte {
                        c.range.end = (c.range.end as isize + delta).max(c.range.start as isize) as usize;
                    }
                }
                // Earlier dirty ranges move with the text too.
                for d in &mut self.pending_dirty {
                    if d.start >= e.old_end_byte {
                        d.start = (d.start as isize + delta).max(0) as usize;
                        d.end = (d.end as isize + delta).max(0) as usize;
                    } else if d.end > e.start_byte {
                        d.end = (d.end as isize + delta).max(d.start as isize) as usize;
                    }
                }
                // A pure deletion still dirties the byte where it happened.
                self.pending_dirty.push(e.start_byte..e.new_end_byte.max(e.start_byte + 1));
                // Regions deleted outright are gone; a stale empty layer would linger.
                self.children.retain(|c| c.range.end > c.range.start);
            }
            // The worker parsed a snapshot; remember what changed since so
            // its tree can be brought up to date on arrival.
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
                // Stale generation: a newer parse was started; drop this result.
                Ok(_) => self.bg = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                // Worker died (spawn failure or panic); allow a retry below.
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.bg = None,
            }
        }
        if (!edits.is_empty() || self.root.is_none()) && self.bg.is_none() {
            self.reparse(doc, budget);
            changed = true;
        }
        changed
    }

    /// Reparse the root layer, incrementally if a previous tree exists.
    ///
    /// Tries synchronously within `budget` unless this is a first parse of
    /// a document over [`SYNC_PARSE_LIMIT`]; on timeout or size, starts a
    /// background parse instead. Disables the state permanently if the
    /// queries or grammar cannot be loaded.
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
        // The parser is shared with injection parsing; reset to whole-document mode.
        let _ = self.parser.set_included_ranges(&[]);
        let rope = doc.rope();
        let old = self.root.as_ref().map(|r| &r.tree);
        let sync_ok = old.is_some() || rope.len_bytes() <= SYNC_PARSE_LIMIT;
        let start = Instant::now();
        let tree = if sync_ok { parse_rope(&mut self.parser, rope, old, Some(budget)) } else { None };
        self.last_parse = start.elapsed();
        match tree {
            Some(t) => {
                // `queries` was only fetched to fail early; the installer refetches it.
                let _ = queries;
                self.install_root_incremental(doc, t);
            }
            // Timed out (or too large): keep the stale tree and parse off-thread.
            None => self.start_background(doc),
        }
    }

    /// Install a tree produced incrementally from the previous root: only the
    /// regions tree-sitter reports as changed (plus the edited bytes) have
    /// their injections re-queried.
    fn install_root_incremental(&mut self, doc: &Document, tree: Tree) {
        let Some(queries) = compiled(self.lang) else { return };
        let len = doc.len_bytes();
        let mut dirty = std::mem::take(&mut self.pending_dirty);
        let old = self.root.take();
        match old {
            Some(old) => {
                for r in tree.changed_ranges(&old.tree) {
                    dirty.push(r.start_byte..r.end_byte);
                }
            }
            None => {
                self.root = Some(Layer { lang: self.lang, queries, tree, range: 0..len, depth: 0 });
                self.cache_valid = false;
                self.rebuild_injections(doc, None);
                return;
            }
        }
        self.root = Some(Layer { lang: self.lang, queries, tree, range: 0..len, depth: 0 });
        self.cache_valid = false;
        self.rebuild_injections(doc, Some(dirty));
    }

    /// Spawn a worker thread to parse a snapshot of the rope with no time
    /// budget. The rope clone is cheap (shared structure). The old tree is
    /// cloned too so the worker can still parse incrementally.
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

    /// Replace the root layer with a tree that did not come from an
    /// incremental reparse of the previous root (first parse or background
    /// result), so all injections are rebuilt from scratch.
    fn install_root(&mut self, doc: &Document, tree: Tree) {
        let Some(queries) = compiled(self.lang) else { return };
        let len = doc.len_bytes();
        self.root = Some(Layer { lang: self.lang, queries, tree, range: 0..len, depth: 0 });
        self.cache_valid = false;
        self.pending_dirty.clear();
        self.rebuild_injections(doc, None);
    }

    /// Re-parse injected regions, reusing existing child trees incrementally
    /// when the region (language and range) is unchanged.
    fn rebuild_injections(&mut self, doc: &Document, dirty: Option<Vec<std::ops::Range<usize>>>) {
        let started = Instant::now();
        // Incremental only when the dirty set is small; otherwise a full pass is cheaper.
        // "Small" = at most 64 ranges covering under half the document.
        let dirty = dirty.map(merge_ranges).filter(|d| d.len() <= 64 && d.iter().map(|r| r.end - r.start).sum::<usize>() * 2 < doc.len_bytes().max(1));
        self.rebuild_injections_inner(doc, dirty);
        self.last_injections = started.elapsed();
    }

    /// The body of [`rebuild_injections`]: run the injections query (over the
    /// whole document, or only the dirty windows), collect the wanted
    /// `(language, range)` pairs, then parse each one, reusing an existing
    /// child tree when the same language covers exactly the same range.
    ///
    /// `dirty == None` means a full rebuild. Total injected bytes are capped
    /// at [`MAX_INJECTION_BYTES`]; regions beyond that are left unhighlighted.
    fn rebuild_injections_inner(&mut self, doc: &Document, dirty: Option<Vec<std::ops::Range<usize>>>) {
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
        // Incremental: widen each dirty range to cover the existing injection
        // regions it touches (a change inside a code fence must re-query the whole
        // fence), then only those windows are queried and only children inside them
        // are replaced.
        let windows: Option<Vec<std::ops::Range<usize>>> = dirty.map(|d| {
            d.into_iter()
                .map(|mut r| {
                    for c in &self.children {
                        if c.range.start <= r.end && c.range.end >= r.start {
                            r.start = r.start.min(c.range.start);
                            r.end = r.end.max(c.range.end);
                        }
                    }
                    r
                })
                .collect()
        });
        let mut wanted: Vec<(&'static LanguageDef, Range)> = Vec::new();
        let touched = |c: &Layer, ws: &[std::ops::Range<usize>]| ws.iter().any(|r| c.range.start <= r.end && c.range.end >= r.start);
        // Children outside every window are kept as-is; their bytes still
        // count towards the injection budget.
        let mut total = self.children.iter().filter(|c| windows.as_ref().is_some_and(|w| !touched(c, w))).map(|c| c.range.end - c.range.start).sum::<usize>();
        let mut cursor = QueryCursor::new();
        let query_windows: Vec<std::ops::Range<usize>> = match &windows {
            Some(w) => merge_ranges(w.clone()),
            None => std::iter::once(0..root.range.end).collect(),
        };
        for w in &query_windows {
            cursor.set_byte_range(w.clone());
            let mut matches = cursor.matches(inj, root.tree.root_node(), |n: Node| rope.byte_slice(n.byte_range()).chunks().map(|c| c.as_bytes()));
            while let Some(m) = matches.next() {
                // The language comes either from a `(#set! injection.language "x")`
                // directive on the pattern or from an `@injection.language` capture
                // (e.g. the info string of a Markdown fence).
                let mut lang_name: Option<String> = inj
                    .property_settings(m.pattern_index)
                    .iter()
                    .find(|p| &*p.key == "injection.language")
                    .and_then(|p| p.value.as_ref().map(|v| v.to_string()));
                if lang_name.is_none()
                    && let Some(li) = language_idx
                    && let Some(c) = m.captures().iter().find(|c| c.index as usize == li)
                {
                    lang_name = Some(rope.byte_slice(c.node.byte_range()).to_string());
                }
                let Some(name) = lang_name else { continue };
                let Some(lang) = lookup(name.trim()) else { continue };
                // Self-injection would recurse forever; grammarless targets
                // have nothing to highlight with.
                if lang.grammar.is_none() || lang.id == self.lang.id {
                    continue;
                }
                for c in m.captures().iter().filter(|c| c.index as usize == content_idx) {
                    let r = c.node.range();
                    if r.end_byte <= r.start_byte || wanted.iter().any(|(_, w)| w.start_byte == r.start_byte && w.end_byte == r.end_byte) {
                        continue;
                    }
                    // Only regions intersecting a window are re-queried; a wide node that
                    // merely touches the window is still wanted in full.
                    if windows.as_ref().is_some_and(|ws| !ws.iter().any(|x| r.start_byte < x.end && r.end_byte > x.start)) {
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
        }
        // Split existing children into those we may reuse/replace (touched)
        // and those that survive untouched.
        let (mut old, kept): (Vec<Layer>, Vec<Layer>) = match &windows {
            Some(ws) => std::mem::take(&mut self.children).into_iter().partition(|c| touched(c, ws)),
            None => (std::mem::take(&mut self.children), Vec::new()),
        };
        let mut new_children = kept;
        for (lang, range) in wanted {
            let Some(queries) = compiled(lang) else { continue };
            let Some(g) = lang.grammar.as_ref() else { continue };
            let byte_range = range.start_byte..range.end_byte;
            // Same language over the same (already shifted) range: the old
            // tree has been `edit`ed, so it is a valid incremental base.
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
        new_children.sort_by_key(|c| c.range.start);
        self.children = new_children;
    }

    /// Highlight spans for one line (line-local byte offsets).
    ///
    /// Results are memoised until the next edit. Returns an empty slice when
    /// there is no tree yet or the line is out of range. Must be called after
    /// [`SyntaxState::update`] in the same frame for accurate results.
    pub fn spans_for_line(&mut self, doc: &Document, line: usize) -> &[Span] {
        if !self.cache_valid {
            self.line_cache.clear();
            self.cache_valid = true;
        }
        if !self.line_cache.contains_key(&line) {
            let spans = self.compute_line(doc, line);
            // Crude bound on memory for long scroll sessions; a clear just
            // costs re-querying the visible lines.
            if self.line_cache.len() > 8192 {
                self.line_cache.clear();
            }
            self.line_cache.insert(line, spans);
        }
        self.line_cache.get(&line).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Compute spans for `line` from scratch: run the highlights query of the
    /// root and of every injection layer intersecting the line, then flatten
    /// the overlapping captures into a non-overlapping span list.
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
        // Paint onto a per-byte "cover" array indexed into a small scope table;
        // `u16::MAX` is the unpainted sentinel. Then run-length encode it.
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
    ///
    /// Drives "expand selection to scope". Consecutive identical ranges
    /// (a node and its single wrapper) are collapsed so each step grows.
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

    /// The root layer's tree, for `fold` and `symbols`.
    pub(crate) fn root_tree(&self) -> Option<&Tree> {
        self.root.as_ref().map(|r| &r.tree)
    }

    /// The root language's compiled tags query, if it has one.
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

    /// Build a document and a fully parsed state for it (generous budget).
    fn state_for(lang: &str, text: &str) -> (SyntaxState, Document) {
        let mut doc = Document::from_text(text);
        let s = SyntaxState::new(lookup(lang).unwrap(), &mut doc, Duration::from_secs(5));
        (s, doc)
    }

    /// Spans of `line` as `(text, scope)` pairs for readable assertions.
    fn scopes(s: &mut SyntaxState, d: &Document, line: usize) -> Vec<(String, &'static str)> {
        let text = d.line_text(line);
        s.spans_for_line(d, line).iter().map(|sp| (text.as_bytes()[sp.start..sp.end].iter().map(|&b| b as char).collect(), sp.scope)).collect()
    }

    /// Basic keyword, function, string and comment scopes come through.
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

    /// After an insert at the top, lines shift and the new line is highlighted correctly.
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

    /// Markdown injects both the inline grammar (emphasis) and a fenced Rust block.
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

    /// HTML injects JavaScript into `<script>` and CSS into `<style>`.
    #[test]
    fn html_injects_script_and_style() {
        let (mut s, d) = state_for("html", "<html><script>\nlet x = 1;\n</script><style>\na { color: red }\n</style></html>\n");
        let l1 = scopes(&mut s, &d, 1);
        assert!(l1.contains(&("let".into(), "keyword")), "{l1:?}");
        let l3 = scopes(&mut s, &d, 3);
        assert!(l3.iter().any(|(t, _)| t == "color"), "{l3:?}");
    }

    /// A 1 ms budget forces the background path; the tree still arrives and highlights the last line.
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

    /// Each enclosing range contains the previous one.
    #[test]
    fn enclosing_ranges_walk_up() {
        let (s, _d) = state_for("rust", "fn a() { b(c); }\n");
        let r = s.enclosing_ranges(11); // inside `c`
        assert!(r.len() >= 3);
        assert!(r.windows(2).all(|w| w[1].0 <= w[0].0 && w[1].1 >= w[0].1));
    }
}

#[cfg(test)]
mod incremental_injection_tests {
    use super::*;
    use apex_edit::{Change, Document, EditKind, SelectionSet, Transaction};

    /// Whether any span on `line` has a scope starting with `scope`.
    fn has_scope(st: &mut SyntaxState, doc: &Document, line: usize, scope: &str) -> bool {
        st.spans_for_line(doc, line).iter().any(|s| s.scope.starts_with(scope))
    }

    /// Code-fence layers only (Markdown also injects inline layers for paragraphs).
    fn fences(st: &SyntaxState) -> Vec<(&'static str, std::ops::Range<usize>)> {
        st.children.iter().filter(|c| c.lang.id != "markdown_inline").map(|c| (c.lang.id, c.range.clone())).collect()
    }

    /// Fence layers survive edits inside them, shift for edits before them,
    /// appear for new fences and disappear when their fence is deleted.
    #[test]
    fn injections_follow_edits_incrementally() {
        let lang = crate::language::lookup("markdown").unwrap();
        let mut doc = Document::from_text("# Title\n\n```rust\nfn main() {}\n```\n\ntext\n");
        let budget = Duration::from_secs(5);
        let mut st = SyntaxState::new(lang, &mut doc, budget);
        assert!(has_scope(&mut st, &doc, 3, "keyword"), "fence is highlighted as Rust");
        assert_eq!(fences(&st).len(), 1);
        let sels = SelectionSet::default();
        // Typing inside the fence keeps the injected layer and its range.
        let at = doc.rope().line_to_char(3) + 3;
        doc.apply(Transaction::single(Change::insert(at, " ")), &sels, &sels, EditKind::Other);
        st.update(&mut doc, budget);
        assert!(has_scope(&mut st, &doc, 3, "keyword"));
        assert_eq!(fences(&st).len(), 1);
        // Typing before the fence shifts its range.
        doc.apply(Transaction::single(Change::insert(0, "intro\n")), &sels, &sels, EditKind::Other);
        st.update(&mut doc, budget);
        assert!(has_scope(&mut st, &doc, 4, "keyword"));
        assert_eq!(fences(&st)[0].1.start, doc.rope().line_to_byte(4));
        // A new fence appended afterwards gets its own layer.
        let end = doc.len_chars();
        doc.apply(Transaction::single(Change::insert(end, "```python\ndef f(): pass\n```\n")), &sels, &sels, EditKind::Other);
        st.update(&mut doc, budget);
        assert_eq!(fences(&st).len(), 2);
        let py_line = doc.len_lines() - 3;
        assert!(has_scope(&mut st, &doc, py_line, "keyword"), "python fence highlighted");
        // Deleting the first fence removes its layer and the others stay right.
        let a = doc.rope().line_to_char(3);
        let b = doc.rope().line_to_char(6);
        doc.apply(Transaction::single(Change::delete(a, b)), &sels, &sels, EditKind::Other);
        st.update(&mut doc, budget);
        assert_eq!(fences(&st).len(), 1, "{:?}", fences(&st));
        let py_line = doc.len_lines() - 3;
        assert!(has_scope(&mut st, &doc, py_line, "keyword"));
    }
}

#[cfg(test)]
mod timing_probe {
    //! `cargo test -p apex-syntax timing_probe -- --ignored --nocapture` prints
    //! where an incremental edit spends its time on a 100 KB Rust file.
    use super::*;
    use apex_edit::{Change, Document, EditKind, SelectionSet, Transaction};

    /// Manual timing probe, not an assertion; see the module doc for how to run it.
    #[test]
    #[ignore]
    fn incremental_edit_breakdown() {
        let unit = include_str!("highlight.rs");
        let mut src = String::new();
        while src.len() < 100 * 1024 {
            src.push_str(unit);
        }
        let lang = crate::language::lookup("rust").unwrap();
        let mut doc = Document::from_text(&src);
        let budget = Duration::from_secs(10);
        let t = Instant::now();
        let mut st = SyntaxState::new(lang, &mut doc, budget);
        println!("initial parse+injections: {:?} (parse {:?})", t.elapsed(), st.last_parse);
        let mid = doc.len_chars() / 2;
        let sels = SelectionSet::default();
        for i in 0..3 {
            doc.apply(Transaction::single(Change::insert(mid, "x")), &sels, &sels, EditKind::Other);
            let t = Instant::now();
            st.update(&mut doc, budget);
            let upd = t.elapsed();
            let t = Instant::now();
            let line = doc.rope().char_to_line(mid);
            let n = st.spans_for_line(&doc, line).len();
            println!("edit {i}: update {upd:?} (parse {:?}, injections {:?}), spans_for_line {:?} ({n} spans)", st.last_parse, st.last_injections, t.elapsed());
        }
    }
}
