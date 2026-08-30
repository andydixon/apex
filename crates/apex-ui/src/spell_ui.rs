//! Spell checking in the editor: visible lines only, prose everywhere and
//! comments/strings in code (by syntax scope), with suggestions in the
//! context menu, a user dictionary and a session ignore list.

use std::collections::HashMap;

use apex_edit::{Change, DocId, EditKind, Selection, SelectionSet, Transaction};
use apex_project::spell::{self, Dictionary};

use crate::app::ApexApp;
use crate::command::Command;
use crate::view::ViewId;

/// (revision, lines checked, misspelled char ranges)
type SpellCacheEntry = (u64, (usize, usize), Vec<(usize, usize)>);

#[derive(Default)]
pub struct SpellUi {
    pub dict: Option<Dictionary>,
    tried_language: Option<String>,
    /// Cache: doc → (revision, lines checked, misspelled char ranges).
    cache: HashMap<DocId, SpellCacheEntry>,
    pub per_doc: HashMap<DocId, bool>,
}

/// Severity value used for spelling underlines in the editor.
pub const SPELL_SEVERITY: u8 = 4;

impl ApexApp {
    fn spell_load(&mut self) {
        let lang = self.settings.spell_language.clone();
        if self.spell.tried_language.as_deref() == Some(lang.as_str()) {
            return;
        }
        self.spell.tried_language = Some(lang.clone());
        self.spell.dict = Dictionary::load(&lang, &self.config_dir);
        if self.spell.dict.is_none() {
            let hint = self.config_dir.join("dictionaries").join(format!("{lang}.dic"));
            self.set_status(
                format!(
                    "No dictionary for {lang} found; install hunspell-{} or put a word list at {}",
                    lang.split('_').next().unwrap_or(&lang),
                    hint.display()
                ),
                true,
            );
        } else {
            self.spell.cache.clear();
        }
    }

    pub(crate) fn spell_enabled_for(&self, d: DocId) -> bool {
        if let Some(v) = self.spell.per_doc.get(&d) {
            return *v;
        }
        self.settings.spell_check
    }

    fn is_prose_language(id: &str) -> bool {
        matches!(id, "plain" | "markdown" | "gitignore" | "requirements")
    }

    /// Misspelled char ranges within `lines` of document `d`.
    pub(crate) fn spell_ranges(&mut self, d: DocId, first: usize, last: usize) -> Vec<(usize, usize)> {
        if !self.spell_enabled_for(d) || first >= last {
            return Vec::new();
        }
        self.spell_load();
        let Some(doc) = self.docs.get(&d) else { return Vec::new() };
        let rev = doc.revision();
        if let Some((r, (a, b), ranges)) = self.spell.cache.get(&d)
            && *r == rev
            && *a == first
            && *b == last
        {
            return ranges.clone();
        }
        let Some(dict) = &self.spell.dict else { return Vec::new() };
        let lang = self.language_of(d);
        let prose = Self::is_prose_language(lang.id);
        let in_comments = self.settings.spell_in_comments;
        let in_strings = self.settings.spell_in_strings;
        let rope = doc.rope();
        let mut out = Vec::new();
        let last = last.min(rope.len_lines());
        for line in first..last {
            let text = doc.line_text(line);
            if text.is_empty() {
                continue;
            }
            let line_start = rope.line_to_char(line);
            // Regions to check as byte ranges within the line.
            let regions: Vec<(usize, usize)> = if prose {
                vec![(0, text.len())]
            } else {
                let Some(st) = self.syntax.get_mut(&d) else { continue };
                st.spans_for_line(doc, line)
                    .iter()
                    .filter(|sp| {
                        (in_comments && sp.scope.starts_with("comment")) || (in_strings && sp.scope.starts_with("string") && !sp.scope.contains("escape"))
                    })
                    .map(|sp| (sp.start, sp.end))
                    .collect()
            };
            if regions.is_empty() {
                continue;
            }
            for (bs, be) in regions {
                let be = be.min(text.len());
                if bs >= be || !text.is_char_boundary(bs) || !text.is_char_boundary(be) {
                    continue;
                }
                let region = &text[bs..be];
                let region_char_offset = text[..bs].chars().count();
                for (ws, we, word) in spell::words(region) {
                    if !dict.check(word) {
                        out.push((line_start + region_char_offset + ws, line_start + region_char_offset + we));
                    }
                }
            }
            if out.len() > 5000 {
                break;
            }
        }
        self.spell.cache.insert(d, (rev, (first, last), out.clone()));
        out
    }

    /// The misspelled word at `pos`, with suggestions.
    pub(crate) fn spell_suggestions_at(&mut self, d: DocId, pos: usize) -> Option<(usize, usize, String, Vec<String>)> {
        if !self.spell_enabled_for(d) {
            return None;
        }
        let doc = self.docs.get(&d)?;
        let line = doc.rope().char_to_line(pos.min(doc.len_chars()));
        let ranges = self.spell_ranges(d, line, line + 1);
        let (a, b) = ranges.into_iter().find(|(a, b)| pos >= *a && pos <= *b)?;
        let doc = self.docs.get(&d)?;
        let word = doc.rope().slice(a..b).to_string();
        let suggestions = self.spell.dict.as_ref().map(|dd| dd.suggest(&word, 6)).unwrap_or_default();
        Some((a, b, word, suggestions))
    }

    pub(crate) fn spell_replace(&mut self, v: ViewId, a: usize, b: usize, with: &str) {
        let text = with.to_string();
        self.run_edit(v, |d, _| {
            let n = d.len_chars();
            let tx = Transaction::single(Change::replace(a.min(n), b.min(n), text.clone()));
            Some(apex_edit::ops::EditResult { tx, sels: SelectionSet::single(Selection::point(a + text.chars().count())), kind: EditKind::Other })
        });
        self.spell.cache.clear();
    }

    pub(crate) fn spell_add_word(&mut self, word: &str) {
        if let Some(dict) = &mut self.spell.dict {
            match dict.add_user_word(word) {
                Ok(()) => self.set_status(format!("Added “{word}” to the user dictionary"), false),
                Err(e) => self.set_status(format!("Could not save the user dictionary: {e}"), true),
            }
        }
        self.spell.cache.clear();
    }

    pub(crate) fn spell_ignore_word(&mut self, word: &str) {
        if let Some(dict) = &mut self.spell.dict {
            dict.ignore(word);
        }
        self.spell.cache.clear();
    }

    pub(crate) fn spell_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::ToggleSpellCheck => {
                match self.active_ids() {
                    Some((_, d)) => {
                        let now = !self.spell_enabled_for(d);
                        self.spell.per_doc.insert(d, now);
                        if now {
                            self.spell_load();
                        }
                        self.set_status(format!("Spell check {} for this file ({})", if now { "on" } else { "off" }, self.settings.spell_language), false);
                    }
                    None => self.settings.spell_check = !self.settings.spell_check,
                }
                self.spell.cache.clear();
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn spell_status_label(&self) -> Option<String> {
        let (_, d) = self.active_ids()?;
        if !self.spell_enabled_for(d) {
            return None;
        }
        Some(match &self.spell.dict {
            Some(dict) => format!("Spell: {}", dict.language),
            None => "Spell: no dictionary".to_string(),
        })
    }
}
