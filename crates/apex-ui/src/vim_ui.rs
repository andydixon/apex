//! Glue between the Vim engine and the application: egui events in, edits and
//! commands out, plus the status-bar label.
//!
//! `crate::vim` is deliberately egui-free, so this module owns the two
//! adapters it needs: [`AppHost`], which implements [`VimHost`] on top of
//! `ApexApp` (document, selections, clipboard, command dispatch), and
//! [`ApexApp::vim_input`], which translates the frame's `egui::Event`s into
//! [`Key`]s and strips the ones the engine consumed before the editor widget
//! and the key map run. `vim_status_label` and `vim_block_caret` are the small
//! read-only queries the status bar and caret painter make each frame.
//!
//! Gotcha: `Vim` lives in `ApexApp`, and `AppHost` borrows `ApexApp` mutably,
//! so every call into the engine `mem::take`s the `Vim` out, runs it, and puts
//! it back. Nothing may rely on `app.vim` while a key is being processed.

use apex_edit::ops::EditResult;
use apex_edit::{Document, SelectionSet};

use crate::app::ApexApp;
use crate::command::Command;
use crate::view::ViewId;
use crate::vim::{Env, Key, Mode, VimHost};

/// [`VimHost`] implementation over the application for one view.
///
/// Created per key (or per text event) and dropped straight after; it never
/// outlives the frame. `vid` is the active view the engine is editing.
struct AppHost<'a> {
    app: &'a mut ApexApp,
    /// The view whose document and selections the engine operates on.
    vid: ViewId,
    /// Needed for clipboard writes and the system-paste viewport command.
    ctx: &'a egui::Context,
}

impl VimHost for AppHost<'_> {
    /// The document behind the active view.
    fn doc(&self) -> &Document {
        let d = self.app.views[&self.vid].doc;
        &self.app.docs[&d]
    }
    /// A copy of the active view's selections (the engine never holds a borrow).
    fn sels(&self) -> SelectionSet {
        self.app.views[&self.vid].sels.clone()
    }
    /// Replace the view's selections and ask the editor to scroll the caret
    /// into view; a no-op if `:q` has already closed the view this frame.
    fn set_sels(&mut self, sels: SelectionSet) {
        if let Some(v) = self.app.views.get_mut(&self.vid) {
            v.set_selections(sels);
            v.want_cursor_visible = true;
        }
    }
    /// Run an edit through the app's normal path so history, LSP and
    /// modified flags are updated exactly as for a keyboard edit.
    fn edit(&mut self, f: &mut dyn FnMut(&Document, &SelectionSet) -> Option<EditResult>) -> bool {
        self.app.run_edit(self.vid, |d, s| f(d, s))
    }
    /// Dispatch an application command (`:w`, `u`, `zz`, ...) through the
    /// same entry point menus and key bindings use.
    fn command(&mut self, cmd: Command, arg: Option<String>) {
        self.app.execute(self.ctx, cmd, arg);
    }
    /// Show a message in the status bar.
    fn status(&mut self, text: String, error: bool) {
        self.app.set_status(text, error);
    }
    /// Most recent text copied from within Apex (see the note below).
    fn clipboard_get(&mut self) -> Option<String> {
        // ponytail: egui exposes no synchronous clipboard read; the most recent
        // copy made in Apex stands in. `"+p` with text copied elsewhere uses the
        // system paste path instead (see `system_paste`).
        self.app.clipboard_history.front().cloned()
    }
    /// Copy to the system clipboard and record it in the in-app clipboard
    /// history (deduplicated, newest first, capped at 32 entries).
    fn clipboard_set(&mut self, text: String) {
        self.ctx.copy_text(text.clone());
        self.app.clipboard_history.retain(|t| *t != text);
        self.app.clipboard_history.push_front(text);
        self.app.clipboard_history.truncate(32);
    }
    /// Ask the window system for a paste; the text arrives as an
    /// `egui::Event::Paste` on a later frame and goes through the editor.
    fn system_paste(&mut self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    /// Close the current undo group so an insert session undoes as one step.
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
        // 20 is only a fallback for a view that has not been laid out yet.
        let page_lines = self.views.get(&vid).map(|v| v.lines_per_page.max(1)).unwrap_or(20);
        let env = Env { tab_width: self.settings.tab_width, page_lines };
        // Clone the events: the engine needs `&mut self` while we iterate, and
        // the consumed ones are removed from the input state afterwards.
        let events = ctx.input(|i| i.events.clone());
        let mut consumed = vec![false; events.len()];
        for (idx, ev) in events.iter().enumerate() {
            // Re-read per event: the mode can change mid-frame (e.g. `i` then text).
            let insert = self.vim.mode == Mode::Insert;
            match ev {
                egui::Event::Text(t) => {
                    // A Text event may carry several characters (IME, paste-like input);
                    // the event is consumed if the engine took any of them.
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
                    // Cmd on macOS is treated like Ctrl so `Cmd+R` etc. reach the engine.
                    let ctrl = modifiers.ctrl || modifiers.command;
                    let k = if ctrl && !modifiers.alt {
                        // Only single letters map to `Key::Ctrl`; the engine itself decides
                        // which of those it handles and lets the rest fall through to the key map.
                        let name = key.symbol_or_name().to_ascii_lowercase();
                        let mut cs = name.chars();
                        match (cs.next(), cs.next()) {
                            (Some(c), None) if c.is_ascii_alphabetic() => Some(Key::Ctrl(c)),
                            _ => None,
                        }
                    } else if modifiers.alt {
                        // Alt combinations are never Vim keys; leave them to the key map.
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
            // Drop consumed events in place so the editor widget and key map, which run
            // later this frame, never see them. `retain` visits in order, so a running
            // index lines up with `consumed`.
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

    /// Whether the editor should paint a block caret: Vim mode is on and the
    /// engine is in a mode where the caret sits on a character rather than
    /// between characters.
    pub(crate) fn vim_block_caret(&self) -> bool {
        self.settings.vim_mode && !matches!(self.vim.mode, Mode::Insert | Mode::CmdLine)
    }

    /// Handle the Vim-related application commands. Returns `true` if `cmd`
    /// was one of them. `ToggleVimMode` flips the setting for this session
    /// only (it is not written to `settings.toml`), rebuilds the engine and,
    /// when turning on, clamps the caret into normal-mode position.
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
