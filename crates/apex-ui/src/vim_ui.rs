//! Glue between the Vim engine and the application: egui events in, edits and
//! commands out, plus the status-bar label.

use apex_edit::ops::EditResult;
use apex_edit::{Document, SelectionSet};

use crate::app::ApexApp;
use crate::command::Command;
use crate::view::ViewId;
use crate::vim::{Env, Key, Mode, VimHost};

struct AppHost<'a> {
    app: &'a mut ApexApp,
    vid: ViewId,
    ctx: &'a egui::Context,
}

impl VimHost for AppHost<'_> {
    fn doc(&self) -> &Document {
        let d = self.app.views[&self.vid].doc;
        &self.app.docs[&d]
    }
    fn sels(&self) -> SelectionSet {
        self.app.views[&self.vid].sels.clone()
    }
    fn set_sels(&mut self, sels: SelectionSet) {
        if let Some(v) = self.app.views.get_mut(&self.vid) {
            v.set_selections(sels);
            v.want_cursor_visible = true;
        }
    }
    fn edit(&mut self, f: &mut dyn FnMut(&Document, &SelectionSet) -> Option<EditResult>) -> bool {
        self.app.run_edit(self.vid, |d, s| f(d, s))
    }
    fn command(&mut self, cmd: Command, arg: Option<String>) {
        self.app.execute(self.ctx, cmd, arg);
    }
    fn status(&mut self, text: String, error: bool) {
        self.app.set_status(text, error);
    }
    fn clipboard_get(&mut self) -> Option<String> {
        // ponytail: egui exposes no synchronous clipboard read; the most recent
        // copy made in Apex stands in. `"+p` with text copied elsewhere uses the
        // system paste path instead (see `system_paste`).
        self.app.clipboard_history.front().cloned()
    }
    fn clipboard_set(&mut self, text: String) {
        self.ctx.copy_text(text.clone());
        self.app.clipboard_history.retain(|t| *t != text);
        self.app.clipboard_history.push_front(text);
        self.app.clipboard_history.truncate(32);
    }
    fn system_paste(&mut self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    fn undo_boundary(&mut self) {
        let d = self.app.views[&self.vid].doc;
        if let Some(doc) = self.app.docs.get_mut(&d) {
            doc.history_mut().mark_boundary();
        }
    }
}

impl ApexApp {
    /// Route this frame's key events through the Vim engine, removing the
    /// ones it consumed so the editor and key map do not see them.
    pub(crate) fn vim_input(&mut self, ctx: &egui::Context) {
        let Some(vid) = self.active_view_id() else { return };
        let page_lines = self.views.get(&vid).map(|v| v.lines_per_page.max(1)).unwrap_or(20);
        let env = Env { tab_width: self.settings.tab_width, page_lines };
        let events = ctx.input(|i| i.events.clone());
        let mut consumed = vec![false; events.len()];
        for (idx, ev) in events.iter().enumerate() {
            let insert = self.vim.mode == Mode::Insert;
            match ev {
                egui::Event::Text(t) => {
                    let mut any = false;
                    for c in t.chars() {
                        if c.is_control() {
                            continue;
                        }
                        let mut host = AppHost { app: self, vid, ctx };
                        let mut vim = std::mem::take(&mut host.app.vim);
                        any |= vim.key(Key::Char(c), &env, &mut host);
                        host.app.vim = vim;
                    }
                    consumed[idx] = any;
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    let ctrl = modifiers.ctrl || modifiers.command;
                    let k = if ctrl && !modifiers.alt {
                        let name = key.symbol_or_name().to_ascii_lowercase();
                        let mut cs = name.chars();
                        match (cs.next(), cs.next()) {
                            (Some(c), None) if c.is_ascii_alphabetic() => Some(Key::Ctrl(c)),
                            _ => None,
                        }
                    } else if modifiers.alt {
                        None
                    } else {
                        match key {
                            egui::Key::Escape => Some(Key::Esc),
                            egui::Key::Enter => Some(Key::Enter),
                            egui::Key::Backspace => Some(Key::Backspace),
                            egui::Key::Tab => Some(Key::Tab),
                            egui::Key::ArrowUp => Some(Key::Up),
                            egui::Key::ArrowDown => Some(Key::Down),
                            egui::Key::ArrowLeft => Some(Key::Left),
                            egui::Key::ArrowRight => Some(Key::Right),
                            egui::Key::Home => Some(Key::Home),
                            egui::Key::End => Some(Key::End),
                            egui::Key::PageUp => Some(Key::PageUp),
                            egui::Key::PageDown => Some(Key::PageDown),
                            egui::Key::Delete => Some(Key::Delete),
                            _ => None,
                        }
                    };
                    match k {
                        Some(k) => {
                            let mut host = AppHost { app: self, vid, ctx };
                            let mut vim = std::mem::take(&mut host.app.vim);
                            consumed[idx] = vim.key(k, &env, &mut host);
                            host.app.vim = vim;
                        }
                        None => {
                            // Printable keys arrive again as Text; outside insert mode keep the
                            // key press itself from reaching the key map (e.g. `x`, `/`).
                            consumed[idx] = !insert && !ctrl && !modifiers.alt;
                        }
                    }
                }
                _ => {}
            }
            if !self.views.contains_key(&vid) {
                break; // `:q` closed the view
            }
        }
        if consumed.iter().any(|c| *c) {
            ctx.input_mut(|i| {
                let mut k = 0;
                i.events.retain(|_| {
                    let keep = !consumed.get(k).copied().unwrap_or(false);
                    k += 1;
                    keep
                });
            });
            ctx.request_repaint();
        }
    }

    /// Label for the status bar: mode, pending keys and the command line.
    pub(crate) fn vim_status_label(&self) -> Option<String> {
        if !self.settings.vim_mode {
            return None;
        }
        if let Some(cl) = &self.vim.cmdline {
            return Some(format!("{}{}_", cl.prompt, cl.text));
        }
        let pending = self.vim.pending_text();
        Some(if pending.is_empty() { format!("-- {} --", self.vim.mode_label()) } else { format!("-- {} --  {pending}", self.vim.mode_label()) })
    }

    pub(crate) fn vim_block_caret(&self) -> bool {
        self.settings.vim_mode && !matches!(self.vim.mode, Mode::Insert | Mode::CmdLine)
    }

    pub(crate) fn vim_command(&mut self, ctx: &egui::Context, cmd: Command) -> bool {
        match cmd {
            Command::ToggleVimMode => {
                self.settings.vim_mode = !self.settings.vim_mode;
                self.vim = crate::vim::Vim::new();
                if self.settings.vim_mode {
                    if let Some(vid) = self.active_view_id() {
                        let mut host = AppHost { app: self, vid, ctx };
                        let mut vim = std::mem::take(&mut host.app.vim);
                        vim.reset(&mut host);
                        host.app.vim = vim;
                    }
                    self.set_status("Vim mode on: Esc for normal mode, i to insert (set vim_mode = true in settings to keep it)".into(), false);
                } else {
                    self.set_status("Vim mode off".into(), false);
                }
                true
            }
            _ => false,
        }
    }
}
