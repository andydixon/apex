//! Snippet insertion with tab-stop sessions, trigger expansion on Tab and
//! the Insert Snippet palette. Built-in snippets live in `assets/snippets`;
//! user snippets are TOML files in `<config>/snippets/`.
//!
//! Parsing and expansion are done by `apex_edit::snippet`; this module wires
//! them to the app. `ApexApp` holds the loaded list in `app.snippets` (filled
//! by `load_snippets` at start-up) and at most one `SnippetSession` in
//! `app.snippet_session`. The app's per-frame input handling calls
//! `snippet_key` (right after the completion popup's key handler) before the
//! editor sees keys, so an active session owns Tab, Shift+Tab and Escape;
//! `try_expand_trigger` runs from the `InsertTab` command. The LSP
//! completion popup also offers snippets via `snippet_completions` and
//! inserts them through `insert_snippet`.
//!
//! Stop positions are stored as absolute char ranges together with the document
//! revision they were computed at, and re-mapped through intervening edits on
//! every step; a session is dropped silently if its view goes away.

use std::path::Path;

use apex_edit::snippet::{self, Expansion, SnippetDef, Variables};
use apex_edit::{Change, DocId, EditKind, Selection, SelectionSet, Transaction};

use crate::app::ApexApp;
use crate::palette::{PaletteItem, PaletteMode, Payload};
use crate::view::ViewId;

/// An in-progress tab-stop walk after inserting a snippet.
pub struct SnippetSession {
    /// The view the snippet was inserted into; the session ends if focus moves.
    pub view: ViewId,
    pub doc: DocId,
    /// Stops in tab order as absolute char ranges.
    pub stops: Vec<(u32, Vec<(usize, usize)>)>,
    /// Index into `stops` of the stop currently selected.
    pub current: usize,
    /// Document revision at which `stops` were last mapped; see `snippet_step`.
    pub revision: u64,
}

impl ApexApp {
    /// Build `self.snippets` from the built-in set plus every `*.toml` in
    /// `<config>/snippets/`. A file named after a language scopes its snippets
    /// to that language unless they declare their own scope; `global.toml` and
    /// `all.toml` are unscoped. Malformed files are logged and skipped.
    pub(crate) fn load_snippets(&mut self) {
        let mut all = snippet::builtin_snippets();
        let dir = self.config_dir.join("snippets");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "toml")
                    && let Ok(src) = std::fs::read_to_string(&p)
                {
                    match snippet::parse_snippet_file(&src) {
                        Ok(list) => {
                            let file_lang = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                            for mut s in list {
                                if s.scope.is_empty() && file_lang != "global" && file_lang != "all" {
                                    s.scope = vec![file_lang.clone()];
                                }
                                // User snippets override built-ins with the same trigger and scope.
                                all.retain(|b| !(b.trigger == s.trigger && b.scope == s.scope));
                                all.push(s);
                            }
                        }
                        Err(e) => tracing::warn!("snippets {}: {e}", p.display()),
                    }
                }
            }
        }
        self.snippets = all;
    }

    /// Snippets applicable to a language id.
    pub(crate) fn snippets_for(&self, lang: &str) -> Vec<&SnippetDef> {
        self.snippets.iter().filter(|s| s.scope.is_empty() || s.scope.iter().any(|l| l == lang)).collect()
    }

    /// Gather the `$TM_*`-style variables a snippet body may reference, taken
    /// from the view's primary selection and the document's path.
    fn snippet_variables(&self, view: ViewId, doc: DocId) -> Variables {
        let d = &self.docs[&doc];
        let v = &self.views[&view];
        let sel = v.sels.primary();
        let rope = d.rope();
        let line = rope.char_to_line(sel.head.min(rope.len_chars()));
        let (ws, we) = apex_buffer::motion::word_at(d.slice(), sel.head);
        let now = crate::app::local_datetime_string();
        // local_datetime_string() is "YYYY-MM-DD HH:MM:SS"; split into the two parts.
        let (date, time) = now.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
        Variables {
            filename: d.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            filename_base: d.path.as_ref().and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            filepath: d.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            directory: d.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default(),
            selected_text: rope.slice(sel.range()).to_string(),
            clipboard: self.clipboard_history.front().cloned().unwrap_or_default(),
            line_number: line,
            current_line: d.line_text(line),
            current_word: rope.slice(ws..we).to_string(),
            year: date.get(..4).unwrap_or("").to_string(),
            date,
            time,
        }
    }

    /// Replace `replace` (or the primary selection) with the expanded snippet and
    /// start a tab-stop session.
    ///
    /// The body is expanded with the current line's indentation and the
    /// document's line ending. With several carets and no explicit `replace`
    /// range the text is inserted plainly at each caret and no session starts.
    /// Otherwise the first stop (or the end of the text) becomes the selection,
    /// and a session is created only if there is more than one stop or the
    /// single stop is not `$0`. Closes any open completion popup.
    pub(crate) fn insert_snippet(&mut self, view: ViewId, body: &str, replace: Option<(usize, usize)>) {
        let Some(doc_id) = self.views.get(&view).map(|v| v.doc) else { return };
        let vars = self.snippet_variables(view, doc_id);
        let (start, end) = replace.unwrap_or_else(|| {
            let s = self.views[&view].sels.primary();
            (s.start(), s.end())
        });
        let (indent, line_ending) = {
            let d = &self.docs[&doc_id];
            let rope = d.rope();
            let line = rope.char_to_line(start.min(rope.len_chars()));
            let l = rope.line(line);
            let n = apex_buffer::motion::first_non_blank(l);
            (l.slice(..n).to_string(), d.line_ending.as_str().to_string())
        };
        let Expansion { text, stops, .. } = snippet::expand(body, &vars, &indent, &line_ending);
        let text_len = text.chars().count();
        let multi = self.views[&view].sels.len() > 1 && replace.is_none();
        if multi {
            // Several carets: plain insertion at each, no session.
            self.run_edit(view, |d, s| {
                apex_edit::ops::insert_text(d, s, &text, apex_edit::ops::InsertOptions { auto_pair: false, auto_indent: false, overwrite: false })
            });
            return;
        }
        // Stop ranges are relative to the expansion start; make them absolute.
        let first_sel: Vec<Selection> = match stops.first() {
            Some((_, ranges)) => ranges.iter().map(|(a, b)| Selection::new(start + a, start + b)).collect(),
            None => vec![Selection::point(start + text_len)],
        };
        let applied = self.run_edit(view, |d, _| {
            let n = d.len_chars();
            let tx = Transaction::single(Change::replace(start.min(n), end.min(n), text.clone()));
            Some(apex_edit::ops::EditResult { tx, sels: SelectionSet::from_vec(first_sel.clone(), 0), kind: EditKind::Other })
        });
        if !applied {
            return;
        }
        self.lsp.completion = None;
        // A lone `$0` means "caret goes here", already handled by the selection
        // above; only real tab stops need a session.
        if stops.len() > 1 || stops.first().is_some_and(|(i, _)| *i != 0) {
            let revision = self.docs[&doc_id].revision();
            let abs: Vec<(u32, Vec<(usize, usize)>)> =
                stops.into_iter().map(|(i, r)| (i, r.into_iter().map(|(a, b)| (start + a, start + b)).collect())).collect();
            self.snippet_session = Some(SnippetSession { view, doc: doc_id, stops: abs, current: 0, revision });
        }
    }

    /// Move to the next/previous stop. Ends the session after the last stop.
    ///
    /// Before moving, every stop boundary is mapped through the edits made since
    /// `session.revision` (typing into a placeholder shifts later stops). Going
    /// back from the first stop keeps the session; going forward past the last
    /// stop leaves the caret at `$0` if present and ends it.
    pub(crate) fn snippet_step(&mut self, back: bool) {
        let Some(mut session) = self.snippet_session.take() else { return };
        let Some(doc) = self.docs.get(&session.doc) else { return };
        // Remap stops through edits made since the last step.
        // Flatten to one position list because `sync_positions` works on a slice.
        let mut flat: Vec<usize> = session.stops.iter().flat_map(|(_, r)| r.iter().flat_map(|(a, b)| [*a, *b])).collect();
        doc.sync_positions(&mut flat, session.revision);
        let mut k = 0;
        for (_, ranges) in &mut session.stops {
            for r in ranges.iter_mut() {
                // `max` guards against a range whose end was mapped before its start.
                *r = (flat[k], flat[k + 1].max(flat[k]));
                k += 2;
            }
        }
        session.revision = doc.revision();
        let next = if back { session.current.checked_sub(1) } else { Some(session.current + 1) };
        match next {
            Some(i) if i < session.stops.len() => {
                session.current = i;
                let sels: Vec<Selection> = session.stops[i].1.iter().map(|(a, b)| Selection::new(*a, *b)).collect();
                if let Some(view) = self.views.get_mut(&session.view) {
                    view.set_selections(SelectionSet::from_vec(sels, 0));
                }
                self.snippet_session = Some(session);
            }
            _ => {
                // Past the last stop: leave the caret at $0 if present, else where it is.
                if !back
                    && let Some((0, ranges)) = session.stops.last()
                    && let Some(view) = self.views.get_mut(&session.view)
                    && let Some((a, _)) = ranges.first()
                {
                    view.set_selections(SelectionSet::single(Selection::point(*a)));
                }
                if back {
                    self.snippet_session = Some(session);
                }
            }
        }
    }

    /// Tab / Shift+Tab / Escape while a session is active.
    ///
    /// Returns true if a key was consumed. The session is dropped if its view is
    /// no longer the active one. Does nothing while the completion popup is
    /// open, since that owns Tab.
    pub(crate) fn snippet_key(&mut self, ctx: &egui::Context) -> bool {
        let Some(s) = &self.snippet_session else { return false };
        if self.active_view_id() != Some(s.view) || !self.views.contains_key(&s.view) {
            self.snippet_session = None;
            return false;
        }
        if self.lsp.completion.is_some() {
            return false; // the completion popup owns Tab
        }
        // consume_key removes the key from egui's input so the editor's own Tab
        // handling does not also see it.
        let (tab, back, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if esc {
            self.snippet_session = None;
            return true;
        }
        if tab || back {
            self.snippet_step(back);
            return true;
        }
        false
    }

    /// Expand a snippet whose trigger is the word before the caret (Tab).
    ///
    /// Only with a single empty selection whose head is at the end of a word.
    /// Returns true if a snippet was inserted, so the caller can skip the
    /// normal Tab action.
    pub(crate) fn try_expand_trigger(&mut self, view: ViewId) -> bool {
        let Some(doc_id) = self.views.get(&view).map(|v| v.doc) else { return false };
        let v = &self.views[&view];
        if v.sels.len() != 1 || !v.sels.primary().is_empty() {
            return false;
        }
        let head = v.sels.primary().head;
        let d = &self.docs[&doc_id];
        // Look at the char before the caret so a caret at a word end finds that word.
        let (ws, we) = apex_buffer::motion::word_at(d.slice(), head.saturating_sub(1));
        if we != head || ws == we {
            return false;
        }
        let word = d.rope().slice(ws..we).to_string();
        let lang = self.language_of(doc_id).id.to_string();
        let body = self.snippets_for(&lang).iter().find(|s| s.trigger == word).map(|s| s.body.clone());
        match body {
            Some(b) => {
                self.insert_snippet(view, &b, Some((ws, we)));
                true
            }
            None => false,
        }
    }

    /// Show the Insert Snippet palette for the active document's language.
    /// Accepted entries are handled by `snippet_palette_accept`.
    pub(crate) fn open_snippet_palette(&mut self) {
        let Some((_, d)) = self.active_ids() else { return };
        let lang = self.language_of(d).id.to_string();
        // Indices into `self.snippets` are the palette payload, so filter with
        // enumerate rather than through `snippets_for`.
        let list: Vec<(usize, &SnippetDef)> =
            self.snippets.iter().enumerate().filter(|(_, s)| s.scope.is_empty() || s.scope.iter().any(|l| l == &lang)).collect();
        if list.is_empty() {
            self.set_status(format!("No snippets for {lang}; add some in {}", self.config_dir.join("snippets").display()), false);
            return;
        }
        let items: Vec<PaletteItem> = list
            .iter()
            .map(|(i, s)| PaletteItem {
                label: format!("{}: {}", s.trigger, s.description),
                detail: s.body.lines().next().unwrap_or("").chars().take(60).collect(),
                shortcut: String::new(),
                payload: Payload::Index(*i),
                boost: 0,
            })
            .collect();
        self.palette.show_with(PaletteMode::Prompt("snippet".into()), items, "Insert snippet", "");
    }

    /// Palette accept hook: inserts the chosen snippet at the selection when the
    /// palette was opened by `open_snippet_palette`. Returns false otherwise so
    /// the caller tries other handlers.
    pub(crate) fn snippet_palette_accept(&mut self, ctx: &egui::Context, mode: &PaletteMode, payload: &Payload) -> bool {
        if let (PaletteMode::Prompt(kind), Payload::Index(i)) = (mode, payload)
            && kind == "snippet"
        {
            if let (Some(v), Some(body)) = (self.active_view_id(), self.snippets.get(*i).map(|s| s.body.clone())) {
                self.insert_snippet(v, &body, None);
            }
            self.focus_active_editor(ctx);
            return true;
        }
        false
    }

    /// Snippets whose trigger starts with `prefix`, as `(trigger, description,
    /// body)` for the completion popup. Empty for an empty prefix.
    pub(crate) fn snippet_completions(&self, lang: &str, prefix: &str) -> Vec<(String, String, String)> {
        if prefix.is_empty() {
            return Vec::new();
        }
        self.snippets_for(lang)
            .into_iter()
            .filter(|s| s.trigger.starts_with(prefix))
            .map(|s| (s.trigger.clone(), s.description.clone(), s.body.clone()))
            .collect()
    }
}

/// Keeps the `Path` import referenced; nothing calls this.
#[allow(dead_code)]
fn _path_unused(_: &Path) {}
