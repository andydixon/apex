//! Language-server features in the application: server lifecycle per
//! workspace root and language, document synchronisation, diagnostics,
//! completion (merged with word completion), hover, signature help,
//! navigation, rename, formatting and code actions.
//!
//! Everything here is optional: when no server is configured or running the
//! editor behaves exactly as without LSP.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apex_buffer::position::{char_to_utf16_position, utf16_position_to_char};
use apex_edit::{Change, DocId, EditKind, Selection, SelectionSet, Transaction};
use apex_project::lsp::{self, CompletionItem, Diagnostic, LspClient, LspEvent, Position, Range, RequestKind, ServerConfig, TextEdit, WorkspaceEdit};
use apex_project::tasks::Problem;
use serde_json::Value;

use crate::app::{ApexApp, Dialog};
use crate::command::Command;
use crate::palette::{PaletteItem, PaletteMode, Payload};
use crate::view::ViewId;

const CHANGE_DEBOUNCE: Duration = Duration::from_millis(250);
const HOVER_DELAY: Duration = Duration::from_millis(600);
const MAX_RESTARTS: u32 = 3;
const MAX_SYNC_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum CompletionSource {
    Lsp,
    Word,
}

#[derive(Debug, Clone)]
pub struct CompletionEntry {
    pub snippet: Option<String>,
    pub label: String,
    pub detail: String,
    pub kind: String,
    pub insert: String,
    /// Char range to replace (defaults to the typed prefix).
    pub replace: Option<(usize, usize)>,
    pub additional: Vec<(usize, usize, String)>,
    pub source: CompletionSource,
    pub sort: String,
}

pub struct CompletionState {
    pub view: ViewId,
    pub doc: DocId,
    pub prefix_start: usize,
    pub items: Vec<CompletionEntry>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub request: Option<i64>,
    pub waiting_since: Option<Instant>,
}

pub struct HoverState {
    pub text: String,
    pub at: egui::Pos2,
    pub doc: DocId,
    pub pos: usize,
}

struct SyncedDoc {
    uri: String,
    key: (PathBuf, String),
    revision: u64,
    changed_at: Option<Instant>,
}

#[derive(Default)]
pub struct LspUi {
    clients: HashMap<(PathBuf, String), LspClient>,
    pub diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    synced: HashMap<DocId, SyncedDoc>,
    pub completion: Option<CompletionState>,
    pub hover: Option<HoverState>,
    pub signature: Option<(String, Option<usize>, DocId, usize)>,
    hover_pending: Option<(DocId, usize, Instant, i64)>,
    pub progress: HashMap<(PathBuf, String), String>,
    failures: HashMap<(PathBuf, String), (u32, Instant)>,
    pending_apply: Option<(Value, (PathBuf, String))>,
    pub last_trigger: Option<Instant>,
    disabled_pairs: HashSet<(PathBuf, String)>,
    unavailable_reported: HashSet<String>,
    pub last_locations: Vec<lsp::Location>,
    pub code_actions: Vec<lsp::CodeAction>,
}

impl ApexApp {
    fn lsp_root_for(&self, path: &Path) -> PathBuf {
        self.roots.iter().find(|r| path.starts_with(r)).cloned().or_else(|| path.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
    }

    /// Server configuration for a language: settings first, then known servers on PATH.
    fn lsp_config_for(&mut self, lang: &str) -> Option<ServerConfig> {
        if let Some(cmd) = self.settings.lsp_servers.get(lang) {
            if cmd.is_empty() {
                return None; // explicitly disabled for this language
            }
            return Some(ServerConfig { command: cmd.clone(), languages: vec![lang.to_string()], ..Default::default() });
        }
        let found = lsp::known_servers().into_iter().find(|s| s.languages.iter().any(|l| l == lang) && lsp::program_available(&s.command[0]));
        if found.is_none()
            && lsp::known_servers().iter().any(|s| s.languages.iter().any(|l| l == lang))
            && self.lsp.unavailable_reported.insert(lang.to_string())
        {
            tracing::info!("no language server for {lang} found on PATH");
        }
        found
    }

    fn client_key_for_doc(&mut self, d: DocId) -> Option<(PathBuf, String)> {
        let path = self.docs.get(&d)?.path.clone()?;
        let lang = self.language_of(d).id.to_string();
        if matches!(lang.as_str(), "plain" | "hex" | "diff") {
            return None;
        }
        Some((self.lsp_root_for(&path), lang))
    }

    fn ensure_client(&mut self, key: &(PathBuf, String)) -> bool {
        if self.lsp.clients.contains_key(key) {
            return true;
        }
        if self.lsp.disabled_pairs.contains(key) {
            return false;
        }
        if let Some((n, at)) = self.lsp.failures.get(key)
            && *n >= MAX_RESTARTS
            && at.elapsed() < Duration::from_secs(300)
        {
            return false;
        }
        let Some(cfg) = self.lsp_config_for(&key.1) else { return false };
        match LspClient::start(cfg, &key.0) {
            Ok(c) => {
                self.set_status(format!("Starting {} for {}", c.config.command[0], key.1), false);
                self.lsp.clients.insert(key.clone(), c);
                true
            }
            Err(e) => {
                self.set_status(format!("Language server failed to start: {e}"), true);
                self.lsp.failures.entry(key.clone()).or_insert((0, Instant::now())).0 += MAX_RESTARTS;
                false
            }
        }
    }

    /// Per-frame: start servers for open documents, synchronise, poll events.
    pub(crate) fn lsp_tick(&mut self, ctx: &egui::Context) -> bool {
        if !self.settings.lsp_enabled {
            if !self.lsp.clients.is_empty() {
                for (_, mut c) in self.lsp.clients.drain() {
                    c.shutdown();
                }
                self.lsp.diagnostics.clear();
                self.lsp.synced.clear();
            }
            return false;
        }
        let mut repaint = false;
        // Documents that should be synchronised.
        let doc_ids: Vec<DocId> = self.docs.keys().copied().collect();
        for d in doc_ids {
            let Some(key) = self.client_key_for_doc(d) else { continue };
            let too_big = self.docs.get(&d).is_some_and(|doc| doc.len_bytes() > MAX_SYNC_BYTES || doc.large_file);
            if too_big {
                continue;
            }
            if !self.ensure_client(&key) {
                continue;
            }
            let (text, revision, path) = {
                let doc = &self.docs[&d];
                (doc.text(), doc.revision(), doc.path.clone().expect("path"))
            };
            let lang = key.1.clone();
            let uri = lsp::uri_for(&path);
            match self.lsp.synced.get_mut(&d) {
                None => {
                    if let Some(c) = self.lsp.clients.get_mut(&key) {
                        c.did_open(&uri, &lang, &text);
                    }
                    self.lsp.synced.insert(d, SyncedDoc { uri, key, revision, changed_at: None });
                }
                Some(s) => {
                    if s.key != key {
                        // Language changed: close on the old server, reopen on the new.
                        if let Some(old) = self.lsp.clients.get_mut(&s.key) {
                            old.did_close(&s.uri);
                        }
                        if let Some(c) = self.lsp.clients.get_mut(&key) {
                            c.did_open(&uri, &lang, &text);
                        }
                        s.key = key;
                        s.uri = uri;
                        s.revision = revision;
                    } else if s.revision != revision {
                        if s.changed_at.is_none() {
                            s.changed_at = Some(Instant::now());
                        }
                        if s.changed_at.is_some_and(|t| t.elapsed() >= CHANGE_DEBOUNCE) {
                            if let Some(c) = self.lsp.clients.get_mut(&s.key) {
                                c.did_change(&s.uri, &text);
                            }
                            s.revision = revision;
                            s.changed_at = None;
                        } else {
                            repaint = true;
                        }
                    }
                }
            }
        }
        // Closed documents.
        let gone: Vec<DocId> = self.lsp.synced.keys().copied().filter(|d| !self.docs.contains_key(d)).collect();
        for d in gone {
            if let Some(s) = self.lsp.synced.remove(&d)
                && let Some(c) = self.lsp.clients.get_mut(&s.key)
            {
                c.did_close(&s.uri);
                if let Some(p) = lsp::path_for(&s.uri) {
                    self.lsp.diagnostics.remove(&p);
                }
            }
        }
        // Poll every client.
        let keys: Vec<(PathBuf, String)> = self.lsp.clients.keys().cloned().collect();
        for key in keys {
            let events = match self.lsp.clients.get_mut(&key) {
                Some(c) => c.poll(),
                None => continue,
            };
            for ev in events {
                repaint = true;
                self.lsp_handle_event(ctx, &key, ev);
            }
        }
        // Hover request timing.
        // id == 0: waiting for the pointer to rest; id != 0: request sent, waiting for the reply.
        match self.lsp.hover_pending {
            Some((d, pos, since, 0)) if since.elapsed() >= HOVER_DELAY => {
                self.lsp.hover_pending = None;
                self.lsp_request_hover(d, pos);
            }
            Some((_, _, since, _)) if since.elapsed() > Duration::from_secs(8) => self.lsp.hover_pending = None,
            Some(_) => repaint = true,
            None => {}
        }
        if self.lsp.completion.as_ref().is_some_and(|c| c.waiting_since.is_some_and(|t| t.elapsed() > Duration::from_secs(3))) {
            // Server too slow: show what we have (word completions).
            if let Some(c) = &mut self.lsp.completion {
                c.waiting_since = None;
            }
        }
        repaint
    }

    fn lsp_handle_event(&mut self, ctx: &egui::Context, key: &(PathBuf, String), ev: LspEvent) {
        match ev {
            LspEvent::Ready => {
                let name = self.lsp.clients.get(key).map(|c| c.server_name.clone()).unwrap_or_default();
                self.set_status(format!("{} ready for {}", if name.is_empty() { "language server".into() } else { name }, key.1), false);
                self.lsp.failures.remove(key);
            }
            LspEvent::Diagnostics { uri, diagnostics, .. } => {
                if let Some(p) = lsp::path_for(&uri) {
                    if diagnostics.is_empty() {
                        self.lsp.diagnostics.remove(&p);
                    } else {
                        self.lsp.diagnostics.insert(p, diagnostics);
                    }
                }
            }
            LspEvent::Response { id, kind, result } => self.lsp_handle_response(ctx, key, id, kind, result),
            LspEvent::ApplyEdit { id, label, edit } => {
                let files = edit.changes.len();
                if files == 0 {
                    if let Some(c) = self.lsp.clients.get(key) {
                        c.respond_apply_edit(id, false, Some("no edits"));
                    }
                } else {
                    self.lsp.pending_apply = Some((id, key.clone()));
                    self.dialog = Some(Dialog::ConfirmWorkspaceEdit { label: label.unwrap_or_else(|| "Language server edit".into()), edit: Box::new(edit) });
                }
            }
            LspEvent::Message { level, text } => {
                let is_error = level <= 1;
                if level <= 2 {
                    self.set_status(format!("{}: {text}", key.1), is_error);
                } else {
                    tracing::info!("{}: {text}", key.1);
                }
            }
            LspEvent::Log(l) => tracing::debug!("lsp {}: {l}", key.1),
            LspEvent::Progress(p) => {
                if p.is_empty() {
                    self.lsp.progress.remove(key);
                } else {
                    self.lsp.progress.insert(key.clone(), p);
                }
            }
            LspEvent::Exited { code } => {
                let stderr = self.lsp.clients.get(key).map(|c| c.stderr_tail()).unwrap_or_default();
                if !stderr.trim().is_empty() {
                    tracing::warn!("language server {} stderr:\n{stderr}", key.1);
                }
                self.lsp.clients.remove(key);
                let synced: Vec<DocId> = self.lsp.synced.iter().filter(|(_, s)| &s.key == key).map(|(d, _)| *d).collect();
                for d in synced {
                    self.lsp.synced.remove(&d);
                }
                self.lsp.progress.remove(key);
                let count = {
                    let entry = self.lsp.failures.entry(key.clone()).or_insert((0, Instant::now()));
                    entry.0 += 1;
                    entry.1 = Instant::now();
                    entry.0
                };
                if count >= MAX_RESTARTS {
                    let reason = stderr.lines().last().map(|l| format!(" — {l}")).unwrap_or_default();
                    self.set_status(
                        format!("Language server for {} stopped {} times (exit code {:?}){reason}. Use Tools ▸ Restart Language Server.", key.1, count, code),
                        true,
                    );
                } else {
                    self.set_status(format!("Language server for {} exited ({:?}); restarting", key.1, code), true);
                }
            }
        }
    }

    // ----- positions ---------------------------------------------------------------

    fn lsp_position(&self, d: DocId, pos: usize) -> Option<Position> {
        let doc = self.docs.get(&d)?;
        let (line, col) = char_to_utf16_position(doc.rope(), pos);
        Some(Position { line: line as u32, character: col as u32 })
    }

    fn char_range(&self, d: DocId, r: &Range) -> Option<(usize, usize)> {
        let doc = self.docs.get(&d)?;
        let rope = doc.rope();
        let a = utf16_position_to_char(rope, r.start.line as usize, r.start.character as usize);
        let b = utf16_position_to_char(rope, r.end.line as usize, r.end.character as usize);
        Some((a.min(b), a.max(b)))
    }

    fn doc_for_uri(&self, uri: &str) -> Option<DocId> {
        let p = lsp::path_for(uri)?;
        self.docs.iter().find(|(_, d)| d.path.as_ref() == Some(&p)).map(|(id, _)| *id)
    }

    fn client_for_doc(&mut self, d: DocId) -> Option<(PathBuf, String)> {
        let s = self.lsp.synced.get(&d)?;
        let key = s.key.clone();
        self.lsp.clients.get(&key).filter(|c| c.is_running()).map(|_| key)
    }

    /// Flush a pending change immediately (before position-sensitive requests).
    fn lsp_flush(&mut self, d: DocId) {
        let Some(s) = self.lsp.synced.get_mut(&d) else { return };
        let Some(doc) = self.docs.get(&d) else { return };
        if s.revision != doc.revision() {
            if let Some(c) = self.lsp.clients.get_mut(&s.key) {
                c.did_change(&s.uri, &doc.text());
            }
            s.revision = doc.revision();
            s.changed_at = None;
        }
    }

    /// Diagnostic underlines for the editor: (start, end, severity).
    pub(crate) fn lsp_diagnostics_for(&self, d: DocId) -> Vec<(usize, usize, u8)> {
        let Some(doc) = self.docs.get(&d) else { return Vec::new() };
        let Some(path) = &doc.path else { return Vec::new() };
        let Some(diags) = self.lsp.diagnostics.get(path) else { return Vec::new() };
        diags.iter().filter_map(|dg| self.char_range(d, &dg.range).map(|(a, b)| (a, b.max(a + 1), dg.severity.unwrap_or(3)))).collect()
    }

    pub(crate) fn lsp_problem_list(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        for (path, diags) in &self.lsp.diagnostics {
            for dg in diags {
                out.push(Problem {
                    file: path.clone(),
                    line: dg.range.start.line as usize + 1,
                    col: Some(dg.range.start.character as usize + 1),
                    message: match &dg.source {
                        Some(s) => format!("[{s}] {}", dg.message),
                        None => dg.message.clone(),
                    },
                    is_error: dg.severity.unwrap_or(1) == 1,
                });
            }
        }
        out
    }

    /// Diagnostic messages at a char offset (for hover and the status bar).
    pub(crate) fn lsp_diagnostics_at(&self, d: DocId, pos: usize) -> Vec<String> {
        let Some(path) = self.docs.get(&d).and_then(|doc| doc.path.clone()) else { return Vec::new() };
        let Some(diags) = self.lsp.diagnostics.get(&path) else { return Vec::new() };
        diags.iter().filter(|dg| self.char_range(d, &dg.range).is_some_and(|(a, b)| pos >= a && pos <= b.max(a + 1))).map(|dg| dg.message.clone()).collect()
    }

    // ----- hover --------------------------------------------------------------------

    pub(crate) fn lsp_hover_moved(&mut self, view: ViewId, pos: Option<usize>) {
        match pos {
            Some(p) => {
                let Some(d) = self.views.get(&view).map(|v| v.doc) else { return };
                if self.lsp.hover.as_ref().is_some_and(|h| h.doc == d && h.pos == p) {
                    return;
                }
                if self.lsp.hover_pending.as_ref().is_some_and(|(hd, hp, _, _)| *hd == d && *hp == p) {
                    return;
                }
                self.lsp.hover = None;
                self.lsp.hover_pending = Some((d, p, Instant::now(), 0));
            }
            None => {
                self.lsp.hover_pending = None;
            }
        }
    }

    fn lsp_request_hover(&mut self, d: DocId, pos: usize) {
        let mut text = self.lsp_diagnostics_at(d, pos).join("\n\n");
        let Some(key) = self.client_for_doc(d) else {
            if !text.is_empty() {
                self.lsp_show_hover(d, pos, text);
            }
            return;
        };
        if !self.lsp.clients[&key].supports("hoverProvider") {
            if !text.is_empty() {
                self.lsp_show_hover(d, pos, text);
            }
            return;
        }
        self.lsp_flush(d);
        let Some(p) = self.lsp_position(d, pos) else { return };
        let uri = self.lsp.synced[&d].uri.clone();
        if let Some(c) = self.lsp.clients.get_mut(&key) {
            let id = c.hover(&uri, p);
            self.lsp.hover_pending = Some((d, pos, Instant::now(), id));
            if !text.is_empty() {
                text.push_str("\n\n");
            }
        }
        // Diagnostics show immediately; hover contents are appended when they arrive.
        if !text.trim().is_empty() {
            self.lsp_show_hover(d, pos, text);
        }
    }

    fn lsp_show_hover(&mut self, d: DocId, pos: usize, text: String) {
        let at = self.views.values().find(|v| v.doc == d).map(|v| v.last_caret_rect.map(|r| r.left_bottom()).unwrap_or_default()).unwrap_or_default();
        let at = self.last_pointer.unwrap_or(at);
        self.lsp.hover = Some(HoverState { text, at, doc: d, pos });
    }

    // ----- completion ----------------------------------------------------------------

    /// Called after text insertion. Opens or updates the completion popup.
    pub(crate) fn lsp_after_insert(&mut self, view: ViewId, inserted: &str) {
        if !self.settings.completion {
            return;
        }
        let Some(d) = self.views.get(&view).map(|v| v.doc) else { return };
        let last = inserted.chars().last().unwrap_or(' ');
        let trigger_chars: Vec<char> = self
            .client_for_doc(d)
            .and_then(|k| {
                self.lsp.clients.get(&k).map(|c| {
                    c.capabilities
                        .pointer("/completionProvider/triggerCharacters")
                        .and_then(|t| t.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).filter_map(|s| s.chars().next()).collect())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        let sig_chars: Vec<char> = self
            .client_for_doc(d)
            .and_then(|k| {
                self.lsp.clients.get(&k).map(|c| {
                    c.capabilities
                        .pointer("/signatureHelpProvider/triggerCharacters")
                        .and_then(|t| t.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).filter_map(|s| s.chars().next()).collect())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        let is_word = last.is_alphanumeric() || last == '_';
        if sig_chars.contains(&last) {
            self.lsp_request_signature(view);
        }
        if !is_word && !trigger_chars.contains(&last) {
            self.lsp.completion = None;
            if last == ')' {
                self.lsp.signature = None;
            }
            return;
        }
        if trigger_chars.contains(&last) {
            // A trigger character starts a new request at the new position.
            self.lsp.completion = None;
        }
        // Existing popup: refilter on the new prefix.
        if let Some(c) = &self.lsp.completion
            && c.view == view
        {
            self.lsp_refilter_completion();
            return;
        }
        let head = self.views[&view].sels.primary().head;
        let doc = &self.docs[&d];
        let slice = doc.slice();
        let (ws, _) = apex_buffer::motion::word_at(slice, head.saturating_sub(1));
        let prefix_start = if is_word { ws.min(head) } else { head };
        let prefix: String = doc.rope().slice(prefix_start..head).to_string();
        if is_word && prefix.chars().count() < 2 && trigger_chars.is_empty() {
            // Wait for a second character before offering word completions.
            if prefix.chars().count() < 2 {
                return;
            }
        }
        let mut state =
            CompletionState { view, doc: d, prefix_start, items: Vec::new(), filtered: Vec::new(), selected: 0, request: None, waiting_since: None };
        state.items = self.word_completions(d, prefix_start, head);
        // LSP request.
        if let Some(key) = self.client_for_doc(d)
            && self.lsp.clients[&key].supports("completionProvider")
        {
            self.lsp_flush(d);
            if let Some(p) = self.lsp_position(d, head) {
                let uri = self.lsp.synced[&d].uri.clone();
                let trigger = if trigger_chars.contains(&last) { Some(last) } else { None };
                if let Some(c) = self.lsp.clients.get_mut(&key) {
                    state.request = Some(c.completion(&uri, p, trigger));
                    state.waiting_since = Some(Instant::now());
                }
            }
        }
        tracing::debug!("completion opened: prefix={prefix:?} words={} request={:?}", state.items.len(), state.request);
        self.lsp.completion = Some(state);
        self.lsp_refilter_completion();
    }

    fn word_completions(&self, d: DocId, prefix_start: usize, head: usize) -> Vec<CompletionEntry> {
        let doc = &self.docs[&d];
        let prefix: String = doc.rope().slice(prefix_start..head).to_string();
        if prefix.is_empty() {
            return Vec::new();
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let lang = self.language_of(d).id.to_string();
        for (trigger, description, body) in self.snippet_completions(&lang, &prefix) {
            seen.insert(trigger.clone());
            out.push(CompletionEntry {
                snippet: Some(body),
                label: trigger.clone(),
                detail: description,
                kind: "snippet".into(),
                insert: trigger.clone(),
                replace: None,
                additional: Vec::new(),
                source: CompletionSource::Word,
                sort: format!("!{trigger}"),
            });
        }
        let re = word_regex();
        // Current document first (bounded), then other open documents.
        let mut docs: Vec<&apex_edit::Document> = vec![doc];
        docs.extend(self.docs.values().filter(|x| x.id != d));
        for (i, dd) in docs.iter().enumerate() {
            if dd.len_bytes() > 4 * 1024 * 1024 {
                continue;
            }
            let text = dd.text();
            for m in re.find_iter(&text).take(200_000) {
                let w = m.as_str();
                if w == prefix || !w.starts_with(prefix.chars().next().unwrap_or(' ')) && !w.to_lowercase().starts_with(&prefix.to_lowercase()) {
                    continue;
                }
                if seen.insert(w.to_string()) {
                    out.push(CompletionEntry {
                        snippet: None,
                        label: w.to_string(),
                        detail: if i == 0 { String::new() } else { dd.title() },
                        kind: "word".into(),
                        insert: w.to_string(),
                        replace: None,
                        additional: Vec::new(),
                        source: CompletionSource::Word,
                        sort: format!("{}{}", if i == 0 { "0" } else { "1" }, w),
                    });
                }
                if out.len() > 2000 {
                    break;
                }
            }
        }
        out
    }

    pub(crate) fn lsp_refilter_completion(&mut self) {
        let Some(c) = &mut self.lsp.completion else { return };
        let Some(view) = self.views.get(&c.view) else {
            self.lsp.completion = None;
            return;
        };
        let head = view.sels.primary().head;
        if head < c.prefix_start || view.sels.len() != 1 {
            self.lsp.completion = None;
            return;
        }
        let Some(doc) = self.docs.get(&c.doc) else {
            self.lsp.completion = None;
            return;
        };
        let prefix: String = doc.rope().slice(c.prefix_start..head).to_string();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern =
            nucleo_matcher::pattern::Pattern::parse(&prefix, nucleo_matcher::pattern::CaseMatching::Smart, nucleo_matcher::pattern::Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = c
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                if prefix.is_empty() {
                    return Some((0, i));
                }
                let label = nucleo_matcher::Utf32Str::new(&it.label, &mut buf);
                pattern.score(label, &mut matcher).map(|s| (s + if it.source == CompletionSource::Lsp { 50 } else { 0 }, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| c.items[a.1].sort.cmp(&c.items[b.1].sort)));
        c.filtered = scored.into_iter().map(|(_, i)| i).take(200).collect();
        c.selected = c.selected.min(c.filtered.len().saturating_sub(1));
        if c.filtered.is_empty() && c.request.is_none() && c.waiting_since.is_none() {
            self.lsp.completion = None;
        }
    }

    /// Returns true if the key was consumed by the completion popup.
    pub(crate) fn lsp_completion_key(&mut self, ctx: &egui::Context) -> bool {
        if self.lsp.completion.is_none() && self.lsp.signature.is_none() {
            return false;
        }
        let (up, down, enter, tab, escape, pgup, pgdn) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            )
        });
        if escape {
            self.lsp.completion = None;
            self.lsp.signature = None;
            return true;
        }
        let Some(c) = &mut self.lsp.completion else {
            if up || down || enter || tab {
                // Keys consumed only when a popup exists; put nothing back (rare path).
            }
            return false;
        };
        let n = c.filtered.len();
        if n == 0 {
            return false;
        }
        if up {
            c.selected = if c.selected == 0 { n - 1 } else { c.selected - 1 };
            return true;
        }
        if down {
            c.selected = (c.selected + 1) % n;
            return true;
        }
        if pgup {
            c.selected = c.selected.saturating_sub(8);
            return true;
        }
        if pgdn {
            c.selected = (c.selected + 8).min(n - 1);
            return true;
        }
        if enter || tab {
            self.lsp_accept_completion();
            return true;
        }
        false
    }

    pub(crate) fn lsp_accept_completion(&mut self) {
        let Some(c) = self.lsp.completion.take() else { return };
        let Some(&idx) = c.filtered.get(c.selected) else { return };
        let item = c.items[idx].clone();
        let view = c.view;
        let head = self.views.get(&view).map(|v| v.sels.primary().head).unwrap_or(0);
        let (start, end) = item.replace.unwrap_or((c.prefix_start, head));
        if let Some(body) = &item.snippet {
            self.insert_snippet(view, body, Some((start, end)));
            return;
        }
        let insert = strip_snippet(&item.insert);
        let additional = item.additional.clone();
        self.run_edit(view, |d, s| {
            let n = d.len_chars();
            let mut changes = vec![Change::replace(start.min(n), end.min(n), insert.clone())];
            for (a, b, t) in &additional {
                changes.push(Change::replace((*a).min(n), (*b).min(n), t.clone()));
            }
            let tx = Transaction::new(changes);
            let caret = tx.map_pos(start, apex_edit::Assoc::Before) + insert.chars().count();
            let _ = s;
            Some(apex_edit::ops::EditResult { tx, sels: SelectionSet::single(Selection::point(caret)), kind: EditKind::Other })
        });
    }

    fn lsp_request_signature(&mut self, view: ViewId) {
        let Some(d) = self.views.get(&view).map(|v| v.doc) else { return };
        let Some(key) = self.client_for_doc(d) else { return };
        if !self.lsp.clients[&key].supports("signatureHelpProvider") {
            return;
        }
        self.lsp_flush(d);
        let head = self.views[&view].sels.primary().head;
        let Some(p) = self.lsp_position(d, head) else { return };
        let uri = self.lsp.synced[&d].uri.clone();
        if let Some(c) = self.lsp.clients.get_mut(&key) {
            c.signature_help(&uri, p);
        }
    }

    // ----- responses -------------------------------------------------------------------

    fn lsp_handle_response(&mut self, ctx: &egui::Context, key: &(PathBuf, String), id: i64, kind: RequestKind, result: Result<Value, String>) {
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                if !matches!(kind, RequestKind::Completion { .. } | RequestKind::Hover { .. } | RequestKind::SignatureHelp { .. }) {
                    self.set_status(format!("Language server: {e}"), true);
                }
                if matches!(kind, RequestKind::Completion { .. })
                    && let Some(c) = &mut self.lsp.completion
                    && c.request == Some(id)
                {
                    c.request = None;
                    c.waiting_since = None;
                }
                return;
            }
        };
        match kind {
            RequestKind::Completion { uri, .. } => {
                let Some(d) = self.doc_for_uri(&uri) else { return };
                if self.lsp.completion.as_ref().is_none_or(|c| c.doc != d || c.request != Some(id)) {
                    return; // popup closed or superseded by a newer request
                }
                let items_v = result.get("items").cloned().unwrap_or(result.clone());
                let items: Vec<CompletionItem> = serde_json::from_value(items_v).unwrap_or_default();
                tracing::debug!("completion response: {} items", items.len());
                let doc = &self.docs[&d];
                let rope = doc.rope();
                let mut entries = Vec::with_capacity(items.len());
                for it in items.into_iter().take(500) {
                    let (insert, range) = it.insertion();
                    let replace = range.map(|r| {
                        let a = utf16_position_to_char(rope, r.start.line as usize, r.start.character as usize);
                        let b = utf16_position_to_char(rope, r.end.line as usize, r.end.character as usize);
                        (a.min(b), a.max(b))
                    });
                    let additional = it
                        .additional_text_edits
                        .iter()
                        .map(|te| {
                            let a = utf16_position_to_char(rope, te.range.start.line as usize, te.range.start.character as usize);
                            let b = utf16_position_to_char(rope, te.range.end.line as usize, te.range.end.character as usize);
                            (a.min(b), a.max(b), te.new_text.clone())
                        })
                        .collect();
                    entries.push(CompletionEntry {
                        snippet: None,
                        label: it.label.clone(),
                        detail: it.detail.clone().unwrap_or_default().chars().take(80).collect(),
                        kind: it.kind_label().to_string(),
                        insert,
                        replace,
                        additional,
                        source: CompletionSource::Lsp,
                        sort: it.sort_text.clone().unwrap_or_else(|| it.label.clone()),
                    });
                }
                // Server items first, then words not already offered.
                let labels: HashSet<String> = entries.iter().map(|e| e.label.clone()).collect();
                let Some(state) = &mut self.lsp.completion else { return };
                state.request = None;
                state.waiting_since = None;
                state.items.retain(|e| !labels.contains(&e.label));
                let mut merged = entries;
                merged.append(&mut state.items);
                state.items = merged;
                self.lsp_refilter_completion();
            }
            RequestKind::Hover { uri } => {
                let Some(d) = self.doc_for_uri(&uri) else { return };
                let Some((hd, pos, _, hid)) = self.lsp.hover_pending.take() else { return };
                if hd != d || hid != id {
                    return;
                }
                let contents = result.get("contents").map(lsp::hover_text).unwrap_or_default();
                if contents.trim().is_empty() {
                    return;
                }
                let mut text = self.lsp.hover.take().filter(|h| h.doc == d && h.pos == pos).map(|h| h.text).unwrap_or_default();
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(contents.trim());
                self.lsp_show_hover(d, pos, text);
            }
            RequestKind::SignatureHelp { uri } => {
                let Some(d) = self.doc_for_uri(&uri) else { return };
                let sigs = result.get("signatures").and_then(|s| s.as_array()).cloned().unwrap_or_default();
                let active = result.get("activeSignature").and_then(|a| a.as_u64()).unwrap_or(0) as usize;
                let Some(sig) = sigs.get(active).or(sigs.first()) else {
                    self.lsp.signature = None;
                    return;
                };
                let label = sig.get("label").and_then(|l| l.as_str()).unwrap_or("").to_string();
                let active_param = result.get("activeParameter").or_else(|| sig.get("activeParameter")).and_then(|a| a.as_u64()).map(|a| a as usize);
                let head = self.views.values().find(|v| v.doc == d).map(|v| v.sels.primary().head).unwrap_or(0);
                self.lsp.signature = Some((label, active_param, d, head));
            }
            RequestKind::Definition { .. } | RequestKind::Declaration { .. } | RequestKind::Implementation { .. } | RequestKind::References { .. } => {
                let locs = lsp::parse_locations(&result);
                if locs.is_empty() {
                    self.set_status("No results from the language server".into(), false);
                    return;
                }
                let is_refs = matches!(kind, RequestKind::References { .. });
                if locs.len() == 1 && !is_refs {
                    self.lsp_goto_location(ctx, &locs[0]);
                } else {
                    self.remember_palette_restore_pub();
                    let items: Vec<PaletteItem> = locs
                        .iter()
                        .filter_map(|l| {
                            let p = lsp::path_for(&l.uri)?;
                            let preview = std::fs::read_to_string(&p)
                                .ok()
                                .and_then(|t| t.lines().nth(l.range.start.line as usize).map(|s| s.trim().chars().take(100).collect::<String>()))
                                .unwrap_or_default();
                            Some(PaletteItem {
                                label: format!("{}:{}", self.display_path(&p), l.range.start.line + 1),
                                detail: preview,
                                shortcut: String::new(),
                                payload: Payload::Symbol { line: l.range.start.line as usize, byte: usize::MAX, path: Some(p) },
                                boost: 0,
                            })
                        })
                        .collect();
                    let title = if is_refs { format!("{} references", items.len()) } else { "Definitions".to_string() };
                    self.palette.show_with(PaletteMode::ProjectSymbols, items, &title, "");
                    // Store ranges so the accept path can place the caret precisely.
                    self.lsp.last_locations = locs;
                }
            }
            RequestKind::Rename { .. } => {
                let edit = WorkspaceEdit::parse(&result);
                if edit.is_empty() {
                    self.set_status("Nothing to rename".into(), false);
                } else {
                    self.lsp.pending_apply = None;
                    self.dialog = Some(Dialog::ConfirmWorkspaceEdit { label: "Rename".into(), edit: Box::new(edit) });
                }
            }
            RequestKind::Formatting { uri, version } => {
                let Some(d) = self.doc_for_uri(&uri) else { return };
                if self.lsp.synced.get(&d).and_then(|s| self.lsp.clients.get(&s.key)).and_then(|c| c.version(&uri)) != Some(version) {
                    self.set_status("Document changed during formatting; result discarded".into(), true);
                    return;
                }
                let edits: Vec<TextEdit> = serde_json::from_value(result).unwrap_or_default();
                if edits.is_empty() {
                    self.set_status("Already formatted".into(), false);
                    return;
                }
                self.lsp_apply_text_edits(d, &edits);
                self.set_status("Formatted".into(), false);
            }
            RequestKind::CodeAction { uri } => {
                let Some(d) = self.doc_for_uri(&uri) else { return };
                let actions = lsp::parse_code_actions(&result);
                if actions.is_empty() {
                    self.set_status("No code actions here".into(), false);
                    return;
                }
                self.lsp.code_actions = actions;
                let items: Vec<PaletteItem> = self
                    .lsp
                    .code_actions
                    .iter()
                    .enumerate()
                    .map(|(i, a)| PaletteItem {
                        label: a.title.clone(),
                        detail: a.kind.clone().unwrap_or_default(),
                        shortcut: String::new(),
                        payload: Payload::Index(i),
                        boost: a.is_preferred as u32,
                    })
                    .collect();
                let _ = d;
                self.palette.show_with(PaletteMode::Prompt("code_action".into()), items, "Code actions", "");
            }
            RequestKind::ExecuteCommand | RequestKind::Initialize | RequestKind::Shutdown | RequestKind::Other(_) => {}
            RequestKind::DocumentSymbols { .. } | RequestKind::WorkspaceSymbols => {
                let syms = lsp::parse_symbols(&result, "");
                if self.palette.open && self.palette.mode == PaletteMode::ProjectSymbols && !syms.is_empty() {
                    let items: Vec<PaletteItem> = syms
                        .iter()
                        .filter_map(|s| {
                            let p = lsp::path_for(&s.uri)?;
                            Some(PaletteItem {
                                label: s.name.clone(),
                                detail: format!("{} · {}:{}", lsp::symbol_kind_label(s.kind), self.display_path(&p), s.range.start.line + 1),
                                shortcut: String::new(),
                                payload: Payload::Symbol { line: s.range.start.line as usize, byte: usize::MAX, path: Some(p) },
                                boost: 0,
                            })
                        })
                        .collect();
                    self.palette.items = items;
                    self.palette.results.clear();
                    self.palette.rescore();
                }
            }
        }
        let _ = key;
    }

    pub(crate) fn remember_palette_restore_pub(&mut self) {
        self.palette_preview = None;
        if let Some(v) = self.active_view_id()
            && let Some(view) = self.views.get(&v)
        {
            self.palette.restore = Some((v, view.sels.clone(), view.scroll));
        }
    }

    fn lsp_goto_location(&mut self, ctx: &egui::Context, loc: &lsp::Location) {
        let Some(path) = lsp::path_for(&loc.uri) else { return };
        self.note_jump();
        if let Some(v) = self.open_path(&path) {
            self.make_permanent(v);
            let d = self.views[&v].doc;
            if let Some((a, _)) = self.char_range(d, &loc.range)
                && let Some(view) = self.views.get_mut(&v)
            {
                view.set_selections(SelectionSet::single(Selection::point(a)));
                view.want_centre = true;
            }
            self.focus_active_editor(ctx);
        }
    }

    /// Apply LSP text edits to an open document as one undoable transaction.
    pub(crate) fn lsp_apply_text_edits(&mut self, d: DocId, edits: &[TextEdit]) -> bool {
        let Some(v) = self.views.values().find(|v| v.doc == d).map(|v| v.id) else { return false };
        let ranges: Vec<(usize, usize, String)> = edits.iter().filter_map(|e| self.char_range(d, &e.range).map(|(a, b)| (a, b, e.new_text.clone()))).collect();
        self.run_edit(v, |doc, s| {
            let changes: Vec<Change> =
                ranges.iter().map(|(a, b, t)| Change::replace(*a, *b, apex_buffer::line_ending::normalise(t, apex_buffer::LineEnding::Lf))).collect();
            let tx = Transaction::new(changes);
            let mut ns = s.clone();
            ns.map(&tx, apex_edit::Assoc::Before);
            ns.clamp(doc.rope());
            Some(apex_edit::ops::EditResult { tx, sels: ns, kind: EditKind::Other })
        })
    }

    /// Apply a workspace edit: open documents are edited (undoable), other
    /// files are rewritten through the safe save path. Returns a report.
    pub(crate) fn lsp_apply_workspace_edit(&mut self, edit: &WorkspaceEdit) -> (usize, Vec<String>) {
        let mut applied = 0usize;
        let mut failures = Vec::new();
        for (uri, edits) in &edit.changes {
            let Some(path) = lsp::path_for(uri) else {
                failures.push(format!("{uri}: not a file"));
                continue;
            };
            if let Some(d) = self.doc_for_uri(uri) {
                if let Some(v) = edit.versions.get(uri)
                    && self.lsp.synced.get(&d).and_then(|s| self.lsp.clients.get(&s.key)).and_then(|c| c.version(&s_uri(&self.lsp.synced[&d]))) != Some(*v)
                {
                    failures.push(format!("{}: changed since the server computed the edit", self.display_path(&path)));
                    continue;
                }
                if self.lsp_apply_text_edits(d, edits) {
                    applied += 1;
                } else {
                    failures.push(format!("{}: could not apply", self.display_path(&path)));
                }
            } else {
                // Unopened file: load, apply, save.
                let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
                match apex_project::fileio::load(&path, None, fallback) {
                    Ok(l) if l.binary.is_none() => {
                        let mut doc = apex_edit::Document::from_text(&l.text);
                        let rope = doc.rope().clone();
                        let changes: Vec<Change> = edits
                            .iter()
                            .map(|e| {
                                let a = utf16_position_to_char(&rope, e.range.start.line as usize, e.range.start.character as usize);
                                let b = utf16_position_to_char(&rope, e.range.end.line as usize, e.range.end.character as usize);
                                Change::replace(a.min(b), a.max(b), e.new_text.clone())
                            })
                            .collect();
                        let s = SelectionSet::default();
                        doc.apply(Transaction::new(changes), &s, &s, EditKind::Other);
                        let opts = apex_project::fileio::SaveOptions {
                            encoding: l.encoding,
                            line_ending: l.line_ending,
                            expected: Some(l.identity),
                            force: false,
                            in_place: false,
                        };
                        match apex_project::fileio::save(&path, &doc.text(), &opts) {
                            Ok(_) => applied += 1,
                            Err(e) => failures.push(format!("{}: {e}", self.display_path(&path))),
                        }
                    }
                    Ok(_) => failures.push(format!("{}: binary file", self.display_path(&path))),
                    Err(e) => failures.push(format!("{}: {e}", self.display_path(&path))),
                }
            }
        }
        for op in &edit.unsupported_operations {
            failures.push(format!("file {op} operations are not applied by Apex"));
        }
        (applied, failures)
    }

    pub(crate) fn lsp_confirm_workspace_edit(&mut self, edit: &WorkspaceEdit, accepted: bool) {
        let pending = self.lsp.pending_apply.take();
        if accepted {
            let (applied, failures) = self.lsp_apply_workspace_edit(edit);
            let mut msg = format!("Applied edits in {applied} file{}", if applied == 1 { "" } else { "s" });
            if !failures.is_empty() {
                msg.push_str(&format!("; {}", failures.join("; ")));
            }
            self.set_status(msg, !failures.is_empty());
            if let Some((id, key)) = pending
                && let Some(c) = self.lsp.clients.get(&key)
            {
                c.respond_apply_edit(id, failures.is_empty(), if failures.is_empty() { None } else { Some("some edits failed") });
            }
        } else if let Some((id, key)) = pending
            && let Some(c) = self.lsp.clients.get(&key)
        {
            c.respond_apply_edit(id, false, Some("declined by the user"));
        }
    }

    // ----- commands ------------------------------------------------------------------------

    pub(crate) fn lsp_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::GotoDefinition | Command::GotoDeclaration | Command::GotoImplementation | Command::GotoReferences => {
                let Some((v, d)) = self.active_ids() else { return true };
                let Some(key) = self.client_for_doc(d) else {
                    if cmd == Command::GotoDefinition {
                        self.goto_definition_fallback(ctx, v, d);
                    } else {
                        self.set_status("No language server for this file".into(), false);
                    }
                    return true;
                };
                let cap = match cmd {
                    Command::GotoDefinition => "definitionProvider",
                    Command::GotoDeclaration => "declarationProvider",
                    Command::GotoImplementation => "implementationProvider",
                    _ => "referencesProvider",
                };
                if !self.lsp.clients[&key].supports(cap) {
                    if cmd == Command::GotoDefinition {
                        self.goto_definition_fallback(ctx, v, d);
                    } else {
                        self.set_status(format!("The language server does not support {}", cmd.name()), false);
                    }
                    return true;
                }
                self.lsp_flush(d);
                let head = self.views[&v].sels.primary().head;
                let Some(p) = self.lsp_position(d, head) else { return true };
                let uri = self.lsp.synced[&d].uri.clone();
                if let Some(c) = self.lsp.clients.get_mut(&key) {
                    match cmd {
                        Command::GotoDefinition => c.definition(&uri, p),
                        Command::GotoDeclaration => c.declaration(&uri, p),
                        Command::GotoImplementation => c.implementation(&uri, p),
                        _ => c.references(&uri, p),
                    };
                }
            }
            Command::LspRename => {
                let Some((v, d)) = self.active_ids() else { return true };
                let Some(key) = self.client_for_doc(d) else {
                    self.set_status("Rename needs a language server; use Find in Files ▸ Replace instead".into(), false);
                    return true;
                };
                if !self.lsp.clients[&key].supports("renameProvider") {
                    self.set_status("The language server does not support rename".into(), false);
                    return true;
                }
                match arg {
                    Some(name) if !name.trim().is_empty() => {
                        self.lsp_flush(d);
                        let head = self.views[&v].sels.primary().head;
                        let Some(p) = self.lsp_position(d, head) else { return true };
                        let uri = self.lsp.synced[&d].uri.clone();
                        if let Some(c) = self.lsp.clients.get_mut(&key) {
                            c.rename(&uri, p, name.trim());
                        }
                    }
                    _ => {
                        let word = {
                            let doc = &self.docs[&d];
                            let head = self.views[&v].sels.primary().head;
                            let (a, b) = apex_buffer::motion::word_at(doc.slice(), head);
                            doc.rope().slice(a..b).to_string()
                        };
                        self.prompt("Rename symbol to", &word, crate::workspace_ui::PromptKind::Rename);
                    }
                }
            }
            Command::FormatDocument => {
                let Some((v, d)) = self.active_ids() else { return true };
                let Some(key) = self.client_for_doc(d).filter(|k| self.lsp.clients[k].supports("documentFormattingProvider")) else {
                    // Built-in fallback: reindent to detected settings and trim whitespace.
                    self.run_edit(v, apex_edit::ops::trim_trailing_whitespace);
                    self.set_status("No formatter available; trimmed trailing whitespace".into(), false);
                    return true;
                };
                self.lsp_flush(d);
                let uri = self.lsp.synced[&d].uri.clone();
                let indent = self.docs[&d].indent;
                if let Some(c) = self.lsp.clients.get_mut(&key) {
                    c.formatting(&uri, indent.indent_width as u32, indent.style == apex_edit::IndentStyle::Spaces);
                }
            }
            Command::LspCodeAction => {
                let Some((v, d)) = self.active_ids() else { return true };
                let Some(key) = self.client_for_doc(d).filter(|k| self.lsp.clients[k].supports("codeActionProvider")) else {
                    self.set_status("No code actions available (no language server)".into(), false);
                    return true;
                };
                self.lsp_flush(d);
                let sel = self.views[&v].sels.primary();
                let (Some(a), Some(b)) = (self.lsp_position(d, sel.start()), self.lsp_position(d, sel.end())) else { return true };
                let uri = self.lsp.synced[&d].uri.clone();
                let path = self.docs[&d].path.clone().unwrap_or_default();
                let diags: Vec<Diagnostic> = self
                    .lsp
                    .diagnostics
                    .get(&path)
                    .map(|ds| ds.iter().filter(|dg| self.char_range(d, &dg.range).is_some_and(|(x, y)| x <= sel.end() && y >= sel.start())).cloned().collect())
                    .unwrap_or_default();
                if let Some(c) = self.lsp.clients.get_mut(&key) {
                    c.code_actions(&uri, Range { start: a, end: b }, &diags);
                }
            }
            Command::LspHover => {
                let Some((v, d)) = self.active_ids() else { return true };
                let head = self.views[&v].sels.primary().head;
                self.last_pointer = self.views[&v].last_caret_rect.map(|r| r.left_bottom());
                self.lsp_request_hover(d, head);
            }
            Command::LspSignatureHelp => {
                let Some((v, _)) = self.active_ids() else { return true };
                self.lsp_request_signature(v);
            }
            Command::LspRestart => {
                let Some((_, d)) = self.active_ids() else { return true };
                let Some(key) = self.client_key_for_doc(d) else { return true };
                if let Some(mut c) = self.lsp.clients.remove(&key) {
                    c.shutdown();
                }
                self.lsp.failures.remove(&key);
                let synced: Vec<DocId> = self.lsp.synced.iter().filter(|(_, s)| s.key == key).map(|(d, _)| *d).collect();
                for d in synced {
                    self.lsp.synced.remove(&d);
                }
                self.set_status(format!("Restarting language server for {}", key.1), false);
            }
            Command::LspToggle => {
                self.settings.lsp_enabled = !self.settings.lsp_enabled;
                self.set_status(format!("Language servers {}", if self.settings.lsp_enabled { "enabled" } else { "disabled" }), false);
            }
            Command::TriggerCompletion => {
                let Some((v, d)) = self.active_ids() else { return true };
                let head = self.views[&v].sels.primary().head;
                let doc = &self.docs[&d];
                let (ws, _) = apex_buffer::motion::word_at(doc.slice(), head.saturating_sub(1));
                let prefix_start = ws.min(head);
                let mut state = CompletionState {
                    view: v,
                    doc: d,
                    prefix_start,
                    items: self.word_completions(d, prefix_start, head),
                    filtered: Vec::new(),
                    selected: 0,
                    request: None,
                    waiting_since: None,
                };
                if let Some(key) = self.client_for_doc(d)
                    && self.lsp.clients[&key].supports("completionProvider")
                {
                    self.lsp_flush(d);
                    if let Some(p) = self.lsp_position(d, head) {
                        let uri = self.lsp.synced[&d].uri.clone();
                        if let Some(c) = self.lsp.clients.get_mut(&key) {
                            state.request = Some(c.completion(&uri, p, None));
                            state.waiting_since = Some(Instant::now());
                        }
                    }
                }
                self.lsp.completion = Some(state);
                self.lsp_refilter_completion();
            }
            _ => return false,
        }
        true
    }

    /// Definition without a server: tree-sitter symbols in this file, then the project index.
    fn goto_definition_fallback(&mut self, ctx: &egui::Context, v: ViewId, d: DocId) {
        let word = {
            let doc = &self.docs[&d];
            let head = self.views[&v].sels.primary().head;
            let (a, b) = apex_buffer::motion::word_at(doc.slice(), head);
            doc.rope().slice(a..b).to_string()
        };
        if word.is_empty() {
            return;
        }
        self.ensure_syntax(d);
        let local = match (self.syntax.get(&d), self.docs.get(&d)) {
            (Some(st), Some(doc)) => apex_syntax::symbols::document_symbols(st, doc.rope(), 5000).into_iter().find(|s| s.name == word),
            _ => None,
        };
        if let Some(s) = local {
            self.note_jump();
            self.goto_byte(v, s.line, s.name_range.0, false);
            return;
        }
        let candidates: Vec<(PathBuf, usize, usize)> =
            self.symbols.symbols.iter().filter(|ps| ps.symbol.name == word).map(|ps| (ps.path.clone(), ps.symbol.line, ps.symbol.name_range.0)).collect();
        match candidates.len() {
            0 => {
                if self.symbol_scan.is_none() && !self.symbols.complete && !self.index.files.is_empty() {
                    let files: Vec<PathBuf> = self.index.files.iter().map(|f| f.path.clone()).collect();
                    self.symbol_scan = Some(crate::symbol_index::start(files));
                    self.set_status(format!("Indexing project symbols; try Go to Definition for “{word}” again shortly"), false);
                } else {
                    self.set_status(format!("No definition of “{word}” found (no language server; tree-sitter symbols only)"), false);
                }
            }
            1 => {
                let (p, line, byte) = candidates[0].clone();
                self.note_jump();
                if let Some(nv) = self.open_path(&p) {
                    self.make_permanent(nv);
                    self.goto_byte(nv, line, byte, false);
                    self.focus_active_editor(ctx);
                }
            }
            _ => {
                self.remember_palette_restore_pub();
                let items: Vec<PaletteItem> = candidates
                    .iter()
                    .map(|(p, line, byte)| PaletteItem {
                        label: word.clone(),
                        detail: format!("{}:{}", self.display_path(p), line + 1),
                        shortcut: String::new(),
                        payload: Payload::Symbol { line: *line, byte: *byte, path: Some(p.clone()) },
                        boost: 0,
                    })
                    .collect();
                self.palette.show_with(PaletteMode::ProjectSymbols, items, "Definitions", "");
            }
        }
    }

    /// Draw the completion popup, hover tooltip and signature help.
    pub(crate) fn lsp_overlays(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        // Completion popup.
        let mut accept: Option<usize> = None;
        if let Some(c) = &self.lsp.completion {
            let Some(view) = self.views.get(&c.view) else { return };
            if let Some(caret) = view.last_caret_rect {
                let waiting = c.waiting_since.is_some();
                if !c.filtered.is_empty() || waiting {
                    let selected = c.selected;
                    let rows: Vec<(usize, String, String, String, bool)> = c
                        .filtered
                        .iter()
                        .take(10)
                        .enumerate()
                        .map(|(i, &idx)| {
                            let it = &c.items[idx];
                            (i, it.label.clone(), it.kind.clone(), it.detail.clone(), it.source == CompletionSource::Lsp)
                        })
                        .collect();
                    let area = egui::Area::new(egui::Id::new("apex-completion"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(caret.left_bottom() + egui::vec2(-4.0, 2.0))
                        .interactable(true);
                    area.show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(theme.ui.surface_raised)
                            .stroke(egui::Stroke::new(1.0, theme.ui.border))
                            .corner_radius(6.0)
                            .inner_margin(4.0)
                            .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 12, spread: 0, color: egui::Color32::from_black_alpha(100) })
                            .show(ui, |ui| {
                                ui.set_min_width(260.0);
                                ui.set_max_width(520.0);
                                let row_h = ui_font + 8.0;
                                for (i, label, kind, detail, is_lsp) in &rows {
                                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(260.0), row_h), egui::Sense::click());
                                    if *i == selected {
                                        ui.painter().rect_filled(rect, 3.0, theme.ui.selection);
                                    } else if resp.hovered() {
                                        ui.painter().rect_filled(rect, 3.0, theme.ui.hover);
                                    }
                                    ui.painter().text(
                                        rect.min + egui::vec2(8.0, row_h / 2.0),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        egui::FontId::monospace(ui_font),
                                        theme.ui.text,
                                    );
                                    let x = rect.min.x + 8.0 + label.chars().count() as f32 * ui_font * 0.62 + 12.0;
                                    let meta = if detail.is_empty() { kind.clone() } else { format!("{kind}  {detail}") };
                                    ui.painter().text(
                                        egui::pos2(x, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        meta,
                                        egui::FontId::proportional(ui_font - 2.0),
                                        if *is_lsp { theme.ui.text_muted } else { theme.ui.text_muted.linear_multiply(0.7) },
                                    );
                                    if resp.clicked() {
                                        accept = Some(*i);
                                    }
                                }
                                if waiting && rows.is_empty() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(egui::RichText::new("Waiting for the language server…").color(theme.ui.text_muted));
                                    });
                                }
                            });
                    });
                }
            }
        }
        if let Some(i) = accept {
            if let Some(c) = &mut self.lsp.completion {
                c.selected = i;
            }
            self.lsp_accept_completion();
        }
        // Hover tooltip.
        if let Some(h) = &self.lsp.hover {
            let text = h.text.clone();
            let at = h.at;
            let area = egui::Area::new(egui::Id::new("apex-hover")).order(egui::Order::Tooltip).fixed_pos(at + egui::vec2(12.0, 8.0)).interactable(false);
            area.show(ctx, |ui| {
                egui::Frame::new().fill(theme.ui.surface_raised).stroke(egui::Stroke::new(1.0, theme.ui.border)).corner_radius(6.0).inner_margin(8.0).show(
                    ui,
                    |ui| {
                        ui.set_max_width(560.0);
                        for block in markdown_blocks(&text) {
                            match block {
                                MdBlock::Code(code) => {
                                    ui.label(egui::RichText::new(code).font(egui::FontId::monospace(ui_font - 1.0)).color(theme.ui.text));
                                }
                                MdBlock::Text(t) => {
                                    ui.label(egui::RichText::new(t).color(theme.ui.text));
                                }
                            }
                        }
                    },
                );
            });
            // Dismiss when the pointer moves well away or a key is pressed.
            let moved = ctx.input(|i| i.pointer.delta().length() > 2.0 && i.pointer.latest_pos().is_some_and(|p| p.distance(at) > 40.0));
            if moved || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)))) {
                self.lsp.hover = None;
            }
        }
        // Signature help above the caret.
        if let Some((label, active, d, _)) = self.lsp.signature.clone() {
            let caret = self.views.values().find(|v| v.doc == d).and_then(|v| v.last_caret_rect);
            if let Some(caret) = caret {
                let area = egui::Area::new(egui::Id::new("apex-signature"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(caret.left_top() - egui::vec2(4.0, ui_font + 16.0))
                    .interactable(false);
                area.show(ctx, |ui| {
                    egui::Frame::new().fill(theme.ui.surface_raised).stroke(egui::Stroke::new(1.0, theme.ui.border)).corner_radius(6.0).inner_margin(6.0).show(
                        ui,
                        |ui| {
                            let mut job = egui::text::LayoutJob::default();
                            let normal = egui::TextFormat { font_id: egui::FontId::monospace(ui_font - 1.0), color: theme.ui.text, ..Default::default() };
                            let strong = egui::TextFormat {
                                font_id: egui::FontId::monospace(ui_font - 1.0),
                                color: theme.ui.accent,
                                underline: egui::Stroke::new(1.0, theme.ui.accent),
                                ..Default::default()
                            };
                            // Emphasise the active parameter by position within the parentheses.
                            if let (Some(ap), Some(open)) = (active, label.find('(')) {
                                let close = label.rfind(')').unwrap_or(label.len());
                                let params = &label[open + 1..close];
                                let mut parts: Vec<&str> = params.split(',').collect();
                                if parts.len() == 1 && parts[0].is_empty() {
                                    parts.clear();
                                }
                                job.append(&label[..=open], 0.0, normal.clone());
                                for (i, p) in parts.iter().enumerate() {
                                    if i > 0 {
                                        job.append(",", 0.0, normal.clone());
                                    }
                                    job.append(p, 0.0, if i == ap { strong.clone() } else { normal.clone() });
                                }
                                job.append(&label[close..], 0.0, normal.clone());
                            } else {
                                job.append(&label, 0.0, normal.clone());
                            }
                            ui.label(job);
                        },
                    );
                });
            }
        }
    }

    pub(crate) fn lsp_status_label(&self) -> Option<String> {
        if !self.settings.lsp_enabled {
            return None;
        }
        let (_, d) = self.active_ids()?;
        let s = self.lsp.synced.get(&d)?;
        let c = self.lsp.clients.get(&s.key)?;
        let name = if c.server_name.is_empty() { c.config.command[0].clone() } else { c.server_name.clone() };
        match self.lsp.progress.get(&s.key) {
            Some(p) => Some(format!("{name}: {}", p.chars().take(40).collect::<String>())),
            None if c.is_running() => Some(name),
            None => Some(format!("{name} (starting)")),
        }
    }

    pub(crate) fn lsp_shutdown_all(&mut self) {
        for (_, mut c) in self.lsp.clients.drain() {
            c.shutdown();
        }
    }

    /// Accept from palettes opened by this module.
    pub(crate) fn lsp_palette_accept(&mut self, ctx: &egui::Context, mode: &PaletteMode, payload: &Payload) -> bool {
        match (mode, payload) {
            (PaletteMode::Prompt(kind), Payload::Index(i)) if kind == "code_action" => {
                let Some(action) = self.lsp.code_actions.get(*i).cloned() else { return true };
                if let Some(edit) = action.edit {
                    self.lsp.pending_apply = None;
                    let (applied, failures) = self.lsp_apply_workspace_edit(&edit);
                    self.set_status(
                        format!(
                            "{}: applied in {applied} file(s){}",
                            action.title,
                            if failures.is_empty() { String::new() } else { format!("; {}", failures.join("; ")) }
                        ),
                        !failures.is_empty(),
                    );
                }
                if let Some(cmd) = action.command
                    && let Some((_, d)) = self.active_ids()
                    && let Some(key) = self.client_for_doc(d)
                    && let Some(name) = cmd.get("command").and_then(|c| c.as_str())
                    && let Some(c) = self.lsp.clients.get_mut(&key)
                {
                    c.execute_command(name, cmd.get("arguments").cloned().unwrap_or(Value::Null));
                }
                self.focus_active_editor(ctx);
                true
            }
            (PaletteMode::ProjectSymbols, Payload::Symbol { line, byte, path: Some(p) }) if *byte == usize::MAX => {
                // A location from the language server: place the caret from the stored ranges.
                self.note_jump();
                let target = self
                    .lsp
                    .last_locations
                    .iter()
                    .find(|l| lsp::path_for(&l.uri).as_deref() == Some(p.as_path()) && l.range.start.line as usize == *line)
                    .cloned();
                if let Some(v) = self.open_path(p) {
                    self.make_permanent(v);
                    let d = self.views[&v].doc;
                    let pos = target.and_then(|l| self.char_range(d, &l.range).map(|(a, _)| a));
                    if let Some(view) = self.views.get_mut(&v) {
                        match pos {
                            Some(a) => view.set_selections(SelectionSet::single(Selection::point(a))),
                            None => {
                                let rope = self.docs[&d].rope();
                                let a = rope.line_to_char((*line).min(rope.len_lines().saturating_sub(1)));
                                view.set_selections(SelectionSet::single(Selection::point(a)));
                            }
                        }
                        view.want_centre = true;
                    }
                    self.focus_active_editor(ctx);
                }
                true
            }
            _ => false,
        }
    }
}

fn s_uri(s: &SyncedDoc) -> String {
    s.uri.clone()
}

/// Strip LSP snippet syntax (`$1`, `${1:placeholder}`, `$0`) for insertion.
pub fn strip_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next();
                let mut depth = 1;
                let mut body = String::new();
                for n in chars.by_ref() {
                    if n == '{' {
                        depth += 1;
                    } else if n == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    body.push(n);
                }
                // `${1:default}` → default; `${1|a,b|}` → a; `${1}` → nothing.
                if let Some(i) = body.find(':') {
                    out.push_str(&strip_snippet(&body[i + 1..]));
                } else if let Some(i) = body.find('|') {
                    out.push_str(body[i + 1..].split([',', '|']).next().unwrap_or(""));
                }
            }
            Some(d) if d.is_ascii_digit() => {
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    chars.next();
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

enum MdBlock {
    Code(String),
    Text(String),
}

/// Split simple markdown into code fences and text (enough for hover content).
fn markdown_blocks(text: &str) -> Vec<MdBlock> {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut buf = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !buf.trim().is_empty() {
                out.push(if in_code { MdBlock::Code(buf.trim_end().to_string()) } else { MdBlock::Text(buf.trim().to_string()) });
            }
            buf.clear();
            in_code = !in_code;
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    if !buf.trim().is_empty() {
        out.push(if in_code { MdBlock::Code(buf.trim_end().to_string()) } else { MdBlock::Text(buf.trim().to_string()) });
    }
    out
}

fn word_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[\p{L}_][\p{L}\p{N}_]{2,}").expect("valid regex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_stripping() {
        assert_eq!(strip_snippet("foo(${1:a}, ${2:b})$0"), "foo(a, b)");
        assert_eq!(strip_snippet("x$1y"), "xy");
        assert_eq!(strip_snippet("${1|one,two|}"), "one");
        assert_eq!(strip_snippet("plain"), "plain");
        assert_eq!(strip_snippet("\\$notvar"), "$notvar");
    }

    #[test]
    fn markdown_split() {
        let b = markdown_blocks("intro\n```rust\nfn x()\n```\nafter");
        assert_eq!(b.len(), 3);
        assert!(matches!(&b[1], MdBlock::Code(c) if c == "fn x()"));
    }
}
