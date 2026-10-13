//! Built-in Vim-style editing mode: a modal key interpreter over the editor's
//! own document, selection and command model. Not a Vim emulator; the
//! supported subset is listed in `docs/user-guide/vim-mode.md` and verified by
//! the tests at the bottom of this file.
//!
//! The engine never touches egui: keys arrive as [`Key`], effects go through
//! [`VimHost`], so the whole thing is unit-tested headlessly.

use std::collections::HashMap;

use apex_buffer::Rope;
use apex_buffer::motion::{self, CharClass};
use apex_buffer::position::{char_col_to_display_col, display_col_to_char_col, line_end_char};
use apex_edit::ops::{self, EditResult, InsertOptions};
use apex_edit::search::{Query, SearchOptions};
use apex_edit::{Assoc, Change, Document, EditKind, Selection, SelectionSet, Transaction};

use crate::command::Command;

/// Current editing mode of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Keys pass through to the ordinary editor until Esc.
    Insert,
    /// Characterwise visual selection (`v`).
    Visual,
    /// Linewise visual selection (`V`).
    VisualLine,
    /// `:` or `/` command line is being typed.
    CmdLine,
}

/// A key as the engine sees it. Printable text arrives as `Char`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    /// Ctrl + letter (lower case).
    Ctrl(char),
}

/// Per-key environment the host supplies.
#[derive(Debug, Clone, Copy)]
pub struct Env {
    /// Tab width in columns, for display-column (goal column) calculations.
    pub tab_width: usize,
    /// Rows visible in the editor; used by Ctrl+F/B/D/U.
    pub page_lines: usize,
}

/// What the engine needs from the application.
pub trait VimHost {
    /// The document being edited.
    fn doc(&self) -> &Document;
    /// Current selections (char indices).
    fn sels(&self) -> SelectionSet;
    /// Replace the selections without editing text.
    fn set_sels(&mut self, sels: SelectionSet);
    /// Apply an edit produced from the current document and selections.
    /// Returns whether an edit was applied (`f` returning `None` means no edit).
    fn edit(&mut self, f: &mut dyn FnMut(&Document, &SelectionSet) -> Option<EditResult>) -> bool;
    /// Run an application command, with an optional string argument (e.g. a path or `"force"`).
    fn command(&mut self, cmd: Command, arg: Option<String>);
    /// Show a status-bar message; `error` styles it as an error.
    fn status(&mut self, text: String, error: bool);
    /// Read the system clipboard.
    fn clipboard_get(&mut self) -> Option<String>;
    /// Write the system clipboard.
    fn clipboard_set(&mut self, text: String);
    /// Start a new undo group so an insert session undoes as one step.
    fn undo_boundary(&mut self);
    /// Paste from the system clipboard through the editor's own paste path.
    fn system_paste(&mut self) {}
}

/// Contents of one named register.
#[derive(Debug, Clone, Default)]
struct Register {
    text: String,
    /// Yanked as whole lines: pastes go above/below the line rather than at the caret.
    linewise: bool,
}

/// The `:`, `/` or `?` command line being typed.
#[derive(Debug, Clone)]
pub struct CmdLine {
    /// Prompt character: `:`, `/` or `?`.
    pub prompt: char,
    /// Text typed after the prompt.
    pub text: String,
}

/// Operator that acts on a motion, text object or visual range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `d`
    Delete,
    /// `c`: delete then enter insert mode.
    Change,
    /// `y`
    Yank,
    /// `>`
    Indent,
    /// `<`
    Outdent,
    /// `gU`
    Upper,
    /// `gu`
    Lower,
    /// `g~`
    ToggleCase,
    /// `=`: re-indent.
    Format,
}

/// Single-character find on the current line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindKind {
    /// `f`: forward, onto the char.
    F,
    /// `F`: backward, onto the char.
    BackF,
    /// `t`: forward, stop before the char.
    T,
    /// `T`: backward, stop after the char.
    BackT,
}

/// A motion's result: new cursor and how the covered range behaves under an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    /// Exclusive character motion: `[from, to)`.
    Exclusive(usize),
    /// Inclusive character motion: `[from, to]`.
    Inclusive(usize),
    /// Whole lines from the cursor line to this line.
    Linewise(usize),
    /// Explicit range (text objects): `[a, b)`, linewise or not.
    Range(usize, usize, bool),
}

/// The Vim engine: mode, pending keys, registers and repeat state. One per
/// editor; it reads and edits the document only through a [`VimHost`].
pub struct Vim {
    /// Current mode.
    pub mode: Mode,
    /// Command line being typed; `Some` only in [`Mode::CmdLine`].
    pub cmdline: Option<CmdLine>,
    /// Keys of the command being typed (counts, register, operator, pending motion key).
    pending: Vec<Key>,
    /// Registers by name (`"`, `0`-`9`, `-`, `a`-`z`).
    registers: HashMap<char, Register>,
    /// Keys of the last change, for `.`.
    last_change: Vec<Key>,
    /// Keys since the last completed command; becomes `last_change` if the command edited text.
    recording: Vec<Key>,
    /// True while `.` replays `last_change`, so the replay is not itself recorded.
    replaying: bool,
    /// Last search pattern and whether it searched forward, for `n`/`N`.
    last_search: Option<(String, bool)>,
    /// Last `f`/`F`/`t`/`T` target, for `;` and `,`.
    last_find: Option<(char, FindKind)>,
    /// Anchor of the visual selection as a char offset.
    visual_anchor: usize,
    /// Display column that `j`/`k` try to keep; cleared by horizontal motions.
    goal_col: Option<usize>,
    /// Pending `i`/`a` count: the inserted text is repeated when leaving insert mode.
    insert_repeat: usize,
    /// Char index where the current insert session began (for `insert_repeat`).
    insert_start: usize,
    /// Operator waiting for a `/pattern<CR>` target.
    pending_op: Option<(Op, Option<char>)>,
    /// `R` turned the editor's overwrite mode on; Esc turns it off again.
    replace_mode: bool,
}

impl Default for Vim {
    /// Same as [`Vim::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl Vim {
    /// A new engine in normal mode with empty registers.
    pub fn new() -> Self {
        Vim {
            mode: Mode::Normal,
            cmdline: None,
            pending: Vec::new(),
            registers: HashMap::new(),
            last_change: Vec::new(),
            recording: Vec::new(),
            replaying: false,
            last_search: None,
            last_find: None,
            visual_anchor: 0,
            goal_col: None,
            insert_repeat: 1,
            insert_start: 0,
            pending_op: None,
            replace_mode: false,
        }
    }

    /// Short mode label for the status bar.
    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::CmdLine => "COMMAND",
        }
    }

    /// The partially typed command, for the status bar.
    pub fn pending_text(&self) -> String {
        self.pending.iter().filter_map(|k| if let Key::Char(c) = k { Some(*c) } else { None }).collect()
    }

    /// Enter normal mode from outside (e.g. when the mode is switched on).
    pub fn reset(&mut self, host: &mut dyn VimHost) {
        self.mode = Mode::Normal;
        self.pending.clear();
        self.cmdline = None;
        self.clamp_normal(host);
    }

    /// Feed one key. Returns true when the key was consumed by the engine
    /// (in insert mode most keys pass through to the ordinary editor).
    pub fn key(&mut self, k: Key, env: &Env, host: &mut dyn VimHost) -> bool {
        if let Key::Ctrl(c) = k
            && self.mode != Mode::Insert
            && !matches!(c, 'r' | 'e' | 'y' | 'f' | 'b' | 'd' | 'u' | 'v' | 'o' | 'i')
        {
            return false; // leave Ctrl shortcuts (save, palette, …) to the key map
        }
        if self.mode == Mode::Normal && self.pending.is_empty() && !host.sels().primary().is_empty() {
            // A mouse or quick-add selection becomes a visual selection.
            self.start_visual(Mode::Visual, host);
        }
        // Command-line keys are not recorded: `.` repeats edits, not ex commands.
        if !self.replaying && self.mode != Mode::CmdLine {
            self.recording.push(k);
        }
        match self.mode {
            Mode::Insert => self.insert_key(k, env, host),
            Mode::CmdLine => {
                self.cmdline_key(k, env, host);
                true
            }
            Mode::Normal | Mode::Visual | Mode::VisualLine => {
                // Re-parse the whole pending sequence on every key; an
                // incomplete parse just stashes it again.
                self.pending.push(k);
                let keys = std::mem::take(&mut self.pending);
                match self.run(&keys, env, host) {
                    Parse::Incomplete => self.pending = keys,
                    Parse::Done { change } => {
                        // A change that entered insert mode (e.g. `cw`) keeps
                        // recording until Esc, so `.` replays the typed text too.
                        if change && !self.replaying && self.mode != Mode::Insert {
                            self.last_change = std::mem::take(&mut self.recording);
                        } else if self.mode != Mode::Insert {
                            self.recording.clear();
                        }
                    }
                    Parse::Invalid => {
                        self.recording.clear();
                    }
                }
                true
            }
        }
    }

    // ----- insert mode ---------------------------------------------------------------

    /// Handle a key in insert mode. Esc returns to normal mode (applying any
    /// count repeat and closing the undo group); other keys return false so the
    /// editor handles them, except during `.` replay where they are applied here.
    fn insert_key(&mut self, k: Key, _env: &Env, host: &mut dyn VimHost) -> bool {
        match k {
            Key::Esc => {
                // Repeat the typed text for `3ihello<Esc>`.
                if self.insert_repeat > 1 {
                    let head = self.cursor(host);
                    let start = self.insert_start.min(head);
                    let typed: String = host.doc().rope().slice(start..head).to_string();
                    if !typed.is_empty() {
                        let n = self.insert_repeat - 1;
                        let text = typed.repeat(n);
                        host.edit(&mut |d, s| ops::insert_text(d, s, &text, InsertOptions { auto_pair: false, auto_indent: false, overwrite: false }));
                    }
                }
                self.insert_repeat = 1;
                self.mode = Mode::Normal;
                if self.replace_mode {
                    self.replace_mode = false;
                    host.command(Command::ToggleOverwrite, None);
                }
                host.undo_boundary();
                // Vim steps the caret back one char when leaving insert mode.
                self.move_all(host, |_, p| p.saturating_sub(1), false, _env);
                self.clamp_normal(host);
                if !self.replaying {
                    self.last_change = std::mem::take(&mut self.recording);
                }
                true
            }
            _ if self.replaying => {
                // During `.` the editor is not in the loop, so apply the typed keys here.
                let opts = InsertOptions { auto_pair: false, auto_indent: false, overwrite: false };
                match k {
                    Key::Char(c) => {
                        let t = c.to_string();
                        host.edit(&mut |d, s| ops::insert_text(d, s, &t, opts));
                    }
                    Key::Enter => {
                        host.edit(&mut |d, s| ops::newline(d, s, d.indent, true));
                    }
                    Key::Backspace => {
                        host.edit(&mut |d, s| ops::delete_backward(d, s, d.indent, false));
                    }
                    Key::Tab => {
                        host.edit(&mut |d, s| ops::tab(d, s, d.indent));
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    // ----- command line --------------------------------------------------------------

    /// Edit the command line, or run it on Enter: `:` runs an ex command, `/`
    /// and `?` search (or complete a pending `d/pattern` style operator).
    /// Backspace on an empty line cancels.
    fn cmdline_key(&mut self, k: Key, env: &Env, host: &mut dyn VimHost) {
        let Some(cl) = &mut self.cmdline else {
            self.mode = Mode::Normal;
            return;
        };
        match k {
            Key::Esc => {
                self.cmdline = None;
                self.mode = Mode::Normal;
            }
            Key::Backspace => {
                if cl.text.pop().is_none() {
                    self.cmdline = None;
                    self.mode = Mode::Normal;
                }
            }
            Key::Char(c) => cl.text.push(c),
            Key::Enter => {
                let cl = self.cmdline.take().expect("checked");
                self.mode = Mode::Normal;
                match cl.prompt {
                    ':' => self.ex_command(&cl.text, env, host),
                    '/' | '?' => {
                        let forward = cl.prompt == '/';
                        // An empty pattern reuses the previous search.
                        if !cl.text.is_empty() {
                            self.last_search = Some((cl.text.clone(), forward));
                        }
                        if let Some((op, register)) = self.pending_op.take() {
                            let Some((pat, fwd)) = self.last_search.clone() else { return };
                            let rope = host.doc().rope();
                            let cur = self.cursor(host).min(rope.len_chars());
                            match search_from(rope, &pat, cur, fwd, 1) {
                                // Search motions are exclusive, in either direction.
                                Some(p) => self.apply_operator(op, cur.min(p), cur.max(p), false, register, host, env),
                                None => host.status(format!("Pattern not found: {pat}"), true),
                            }
                        } else {
                            self.search_step(1, true, host);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Run a `:` command (without the colon). Most map to an application
    /// [`Command`]; unknown commands report an error on the status bar.
    fn ex_command(&mut self, text: &str, env: &Env, host: &mut dyn VimHost) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // Ranges: a bare number jumps; `%` and `'<,'>` prefix a substitution.
        if let Ok(n) = text.parse::<usize>() {
            self.goto_line(n, host);
            return;
        }
        let (range, rest) = if let Some(r) = text.strip_prefix('%') {
            (ExRange::All, r)
        } else if let Some(r) = text.strip_prefix("'<,'>") {
            (ExRange::Visual, r)
        } else {
            (ExRange::Line, text)
        };
        // Split command name from argument; a `!` stays on the name (`q!`), a
        // `/` starts the argument (`s/a/b/`).
        let (cmd, arg) = match rest.find(|c: char| c.is_whitespace() || c == '/' || c == '!') {
            Some(i) if rest[i..].starts_with('!') => (&rest[..=i], rest[i + 1..].trim()),
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, ""),
        };
        match cmd {
            "w" | "write" => host.command(Command::Save, if arg.is_empty() { None } else { Some(arg.to_string()) }),
            "wa" | "wall" => host.command(Command::SaveAll, None),
            "q" | "quit" => host.command(Command::CloseTab, None),
            "q!" | "quit!" => host.command(Command::CloseTab, Some("force".into())),
            "qa" | "qall" => host.command(Command::Quit, None),
            "qa!" | "qall!" => host.command(Command::Quit, Some("force".into())),
            "wq" | "x" | "xit" => {
                host.command(Command::Save, None);
                host.command(Command::CloseTab, None);
            }
            "e" | "edit" => host.command(Command::OpenFile, if arg.is_empty() { None } else { Some(arg.to_string()) }),
            "noh" | "nohlsearch" => host.command(Command::CloseFind, None),
            "s" | "substitute" => self.ex_substitute(range, arg, host),
            "set" => match arg {
                "wrap" | "nowrap" => host.command(Command::ToggleWordWrap, None),
                "nu" | "number" | "nonu" | "nonumber" => host.command(Command::ToggleLineNumbers, None),
                "rnu" | "relativenumber" | "nornu" => host.command(Command::ToggleRelativeLineNumbers, None),
                "spell" | "nospell" => host.command(Command::ToggleSpellCheck, None),
                _ => host.status(format!(":set {arg} is not supported"), true),
            },
            "sp" | "split" => host.command(Command::SplitHorizontal, None),
            "vs" | "vsplit" => host.command(Command::SplitVertical, None),
            "bn" | "bnext" | "tabn" => host.command(Command::NextTab, None),
            "bp" | "bprev" | "bprevious" | "tabp" => host.command(Command::PrevTab, None),
            "only" | "on" => host.command(Command::LayoutSingle, None),
            "reg" | "registers" => {
                let mut names: Vec<&char> = self.registers.keys().collect();
                names.sort();
                let summary: Vec<String> =
                    names.iter().map(|n| format!("\"{n}: {}", self.registers[n].text.chars().take(30).collect::<String>().replace('\n', "^J"))).collect();
                host.status(if summary.is_empty() { "No registers".to_string() } else { summary.join("   ") }, false);
            }
            _ => host.status(format!("Not an editor command: {text}"), true),
        }
        let _ = env;
    }

    /// `:s/pattern/replacement/flags` over `range`. The pattern is a regex;
    /// flags `g` (all matches per line) and `i` (ignore case) are honoured. Applies
    /// one transaction, leaves the caret at the start of the last changed line
    /// and returns to normal mode.
    fn ex_substitute(&mut self, range: ExRange, arg: &str, host: &mut dyn VimHost) {
        // s/pat/rep/flags with `/` as the only delimiter; `\/` escapes it.
        let Some(body) = arg.strip_prefix('/') else {
            host.status("Usage: :s/pattern/replacement/[g]".into(), true);
            return;
        };
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek() == Some(&'/') {
                cur.push('/');
                chars.next();
            } else if c == '/' {
                parts.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        parts.push(cur);
        let pattern = parts.first().cloned().unwrap_or_default();
        let replacement = parts.get(1).cloned().unwrap_or_default();
        let flags = parts.get(2).cloned().unwrap_or_default();
        let global = flags.contains('g');
        let case_insensitive = flags.contains('i');
        if pattern.is_empty() {
            host.status("Empty pattern".into(), true);
            return;
        }
        let query = match Query::new(&pattern, SearchOptions { case_sensitive: !case_insensitive, whole_word: false, regex: true, in_range: None }) {
            Ok(q) => q,
            Err(e) => {
                host.status(format!("Pattern error: {e}"), true);
                return;
            }
        };
        let sels = host.sels();
        let doc = host.doc();
        let rope = doc.rope();
        let (start, end) = match range {
            ExRange::All => (0, rope.len_chars()),
            ExRange::Line => {
                let line = rope.char_to_line(sels.primary().head.min(rope.len_chars()));
                (rope.line_to_char(line), line_end_char(rope, line))
            }
            ExRange::Visual => {
                let p = sels.primary();
                // Visual mode may have been left already; include the stored anchor.
                let (a, b) = (p.start(), p.end().max(self.visual_anchor.min(rope.len_chars())));
                let la = rope.char_to_line(a);
                let lb = rope.char_to_line(b.saturating_sub(1).max(a));
                (rope.line_to_char(la), line_end_char(rope, lb))
            }
        };
        // At most 100,000 matches are considered; later ones are left unchanged.
        let mut matches = query.find_all(rope, 100_000);
        matches.retain(|m| m.range.start >= start && m.range.end <= end);
        if !global {
            // First match per line only.
            let mut seen_line = None;
            matches.retain(|m| {
                let l = rope.char_to_line(m.range.start);
                if seen_line == Some(l) {
                    false
                } else {
                    seen_line = Some(l);
                    true
                }
            });
        }
        if matches.is_empty() {
            host.status(format!("Pattern not found: {pattern}"), true);
            return;
        }
        let n = matches.len();
        let changes: Vec<Change> =
            matches.iter().map(|m| Change::replace(m.range.start, m.range.end, query.expand_replacement(rope, m, &replacement))).collect();
        let last_line = rope.char_to_line(matches.last().map(|m| m.range.start).unwrap_or(0));
        host.edit(&mut |d, s| {
            let tx = Transaction::new(changes.clone());
            let mut ns = s.clone();
            ns.map(&tx, Assoc::Before);
            let rope = d.rope();
            let line_start = rope.line_to_char(last_line.min(rope.len_lines().saturating_sub(1)));
            let p = tx.map_pos(line_start, Assoc::Before);
            ns.set_single(Selection::point(p));
            Some(EditResult { tx, sels: ns, kind: EditKind::Other })
        });
        self.mode = Mode::Normal;
        self.clamp_normal(host);
        host.status(format!("{n} substitution{}", if n == 1 { "" } else { "s" }), false);
    }

    // ----- normal / visual parsing ----------------------------------------------------

    /// Parse and, if complete, execute a normal/visual-mode key sequence:
    /// `["x][count]operator[count]target` or a simple command. Returns
    /// `Incomplete` to wait for more keys.
    fn run(&mut self, keys: &[Key], env: &Env, host: &mut dyn VimHost) -> Parse {
        let mut i = 0;
        // Register prefix.
        let mut register: Option<char> = None;
        if keys.first() == Some(&Key::Char('"')) {
            match keys.get(1) {
                Some(Key::Char(c)) => {
                    register = Some(*c);
                    i = 2;
                }
                Some(_) => return Parse::Invalid,
                None => return Parse::Incomplete,
            }
        }
        // Count.
        let (count, ni) = parse_count(keys, i);
        i = ni;
        let Some(&k) = keys.get(i) else { return Parse::Incomplete };
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        // Operator?
        let op = match k {
            Key::Char('d') => Some(Op::Delete),
            Key::Char('c') => Some(Op::Change),
            Key::Char('y') => Some(Op::Yank),
            Key::Char('>') => Some(Op::Indent),
            Key::Char('<') => Some(Op::Outdent),
            Key::Char('=') => Some(Op::Format),
            Key::Char('g') if keys.get(i + 1) == Some(&Key::Char('u')) && !visual => Some(Op::Lower),
            Key::Char('g') if keys.get(i + 1) == Some(&Key::Char('U')) && !visual => Some(Op::Upper),
            Key::Char('g') if keys.get(i + 1) == Some(&Key::Char('~')) && !visual => Some(Op::ToggleCase),
            _ => None,
        };
        if let Some(op) = op {
            let op_len = if matches!(op, Op::Lower | Op::Upper | Op::ToggleCase) { 2 } else { 1 };
            i += op_len;
            if visual {
                let (a, b, linewise) = self.visual_range(host);
                self.apply_operator(op, a, b, linewise, register, host, env);
                return Parse::Done { change: op != Op::Yank };
            }
            // Doubled operator = current line(s).
            let doubled = match op {
                Op::Lower | Op::Upper | Op::ToggleCase => {
                    keys.get(i)
                        == Some(&Key::Char(match op {
                            Op::Lower => 'u',
                            Op::Upper => 'U',
                            _ => '~',
                        }))
                }
                _ => keys.get(i) == Some(&k),
            };
            // A count may also sit between the operator and its target (`d3w`, `d3d`).
            let (count2, ni) = parse_count(keys, i);
            let count2 = count2.unwrap_or(1);
            if doubled || (keys.get(ni) == Some(&k) && count2 != 1) {
                let total = count.unwrap_or(1) * count2;
                let cur = self.cursor(host);
                let rope = host.doc().rope();
                let line = rope.char_to_line(cur.min(rope.len_chars()));
                let last = (line + total - 1).min(rope.len_lines().saturating_sub(1));
                let (a, b) = line_range(rope, line, last);
                self.apply_operator(op, a, b, true, register, host, env);
                return Parse::Done { change: op != Op::Yank };
            }
            let Some(&target_key) = keys.get(ni) else { return Parse::Incomplete };
            if let Key::Char(c @ ('/' | '?')) = target_key {
                // `d/pattern<CR>`: finish on the command line.
                self.pending_op = Some((op, register));
                self.cmdline = Some(CmdLine { prompt: c, text: String::new() });
                self.mode = Mode::CmdLine;
                return Parse::Done { change: false };
            }
            // Text object or motion.
            let n = count.unwrap_or(1) * count2;
            let mut target_keys: Vec<Key> = keys[ni..].to_vec();
            if op == Op::Change && matches!(target_key, Key::Char('w') | Key::Char('W')) {
                // `cw` on a word behaves like `ce` (Vim's special case).
                let rope = host.doc().rope();
                let cur = self.cursor(host);
                if cur < rope.len_chars() && !rope.char(cur).is_whitespace() {
                    target_keys[0] = if target_key == Key::Char('w') { Key::Char('e') } else { Key::Char('E') };
                }
            }
            match self.parse_target(&target_keys, n, env, host, true) {
                Target::Incomplete => Parse::Incomplete,
                Target::Invalid => Parse::Invalid,
                Target::Span(span) => {
                    let cur = self.cursor(host);
                    let rope = host.doc().rope();
                    let (a, b, linewise) = span_range(rope, cur, span);
                    // Empty range: nothing to operate on, and nothing to repeat.
                    if a == b && !linewise {
                        return Parse::Done { change: false };
                    }
                    self.apply_operator(op, a, b, linewise, register, host, env);
                    Parse::Done { change: op != Op::Yank }
                }
            }
        } else {
            self.simple_command(&keys[i..], count, register, env, host)
        }
    }

    /// Commands that are not operator + target: mode switches, single-key
    /// edits, put, undo, `.` and so on. `keys` starts after any register and
    /// count; anything unrecognised is parsed as a plain motion.
    fn simple_command(&mut self, keys: &[Key], count: Option<usize>, register: Option<char>, env: &Env, host: &mut dyn VimHost) -> Parse {
        let n = count.unwrap_or(1);
        let Some(&k) = keys.first() else { return Parse::Incomplete };
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        let change = |b: bool| Parse::Done { change: b };
        match k {
            Key::Esc => {
                self.mode = Mode::Normal;
                let p = self.cursor(host);
                host.set_sels(SelectionSet::single(Selection::point(p)));
                self.clamp_normal(host);
                change(false)
            }
            Key::Char('i') if !visual => {
                self.insert_repeat = n;
                self.enter_insert(host);
                change(false)
            }
            Key::Char('a') if !visual => {
                self.insert_repeat = n;
                self.move_all(host, |d, p| if at_line_end(d.rope(), p) { p } else { p + 1 }, false, env);
                self.enter_insert(host);
                change(false)
            }
            Key::Char('I') if !visual => {
                self.insert_repeat = n;
                self.move_all(host, |d, p| first_non_blank_of_line(d.rope(), p), false, env);
                self.enter_insert(host);
                change(false)
            }
            Key::Char('A') if !visual => {
                self.insert_repeat = n;
                self.move_all(host, |d, p| line_end_of(d.rope(), p), false, env);
                self.enter_insert(host);
                change(false)
            }
            Key::Char('o') if !visual => {
                self.move_all(host, |d, p| line_end_of(d.rope(), p), false, env);
                host.edit(&mut |d, s| ops::newline(d, s, d.indent, true));
                self.enter_insert(host);
                change(false)
            }
            Key::Char('O') if !visual => {
                // Open a line above: insert the current line's indentation plus a newline at its start.
                host.edit(&mut |d, s| {
                    let rope = d.rope();
                    let mut changes = Vec::new();
                    let mut sels = Vec::new();
                    let mut delta = 0usize;
                    for sel in s.iter() {
                        let line = rope.char_to_line(sel.head.min(rope.len_chars()));
                        let ls = rope.line_to_char(line);
                        let indent: String = rope.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                        let ins = format!("{indent}\n");
                        sels.push(Selection::point(ls + delta + indent.chars().count()));
                        delta += ins.chars().count();
                        changes.push(Change::insert(ls, ins));
                    }
                    Some(EditResult { tx: Transaction::new(changes), sels: SelectionSet::from_vec(sels, s.primary_index()), kind: EditKind::Other })
                });
                self.enter_insert(host);
                change(false)
            }
            Key::Char('v') => {
                if self.mode == Mode::Visual {
                    self.mode = Mode::Normal;
                    let p = self.cursor(host);
                    host.set_sels(SelectionSet::single(Selection::point(p)));
                } else {
                    self.start_visual(Mode::Visual, host);
                }
                change(false)
            }
            Key::Char('V') => {
                if self.mode == Mode::VisualLine {
                    self.mode = Mode::Normal;
                    let p = self.cursor(host);
                    host.set_sels(SelectionSet::single(Selection::point(p)));
                } else {
                    self.start_visual(Mode::VisualLine, host);
                }
                change(false)
            }
            Key::Char('o') if visual => {
                let p = self.cursor(host);
                let a = self.visual_anchor;
                self.visual_anchor = p;
                self.set_visual_cursor(a, host);
                change(false)
            }
            Key::Char(':') => {
                self.cmdline = Some(CmdLine { prompt: ':', text: if visual { "'<,'>".into() } else { String::new() } });
                self.mode = Mode::CmdLine;
                change(false)
            }
            Key::Char(c @ ('/' | '?')) => {
                self.cmdline = Some(CmdLine { prompt: c, text: String::new() });
                self.mode = Mode::CmdLine;
                change(false)
            }
            Key::Char('n') => {
                self.search_step(n as i64, true, host);
                change(false)
            }
            Key::Char('N') => {
                self.search_step(n as i64, false, host);
                change(false)
            }
            Key::Char(c @ ('*' | '#')) => {
                let p = self.cursor(host);
                let (ws, we) = motion::word_at(host.doc().slice(), p);
                if ws < we {
                    let w: String = host.doc().rope().slice(ws..we).to_string();
                    // The direction is stored with the pattern (`#` searches
                    // backward), so step "in the same direction" below.
                    self.last_search = Some((format!("\\b{}\\b", regex_escape(&w)), c == '*'));
                    self.search_step(n as i64, true, host);
                }
                change(false)
            }
            Key::Char('x') | Key::Delete => {
                if visual {
                    let (a, b, lw) = self.visual_range(host);
                    self.apply_operator(Op::Delete, a, b, lw, register, host, env);
                } else {
                    // `x` never deletes past the end of the line.
                    let cur = self.cursor(host);
                    let rope = host.doc().rope();
                    let end = (cur + n).min(line_end_of(rope, cur));
                    if end > cur {
                        self.apply_operator(Op::Delete, cur, end, false, register, host, env);
                    }
                }
                change(true)
            }
            Key::Char('X') if !visual => {
                let cur = self.cursor(host);
                let rope = host.doc().rope();
                let start = cur.saturating_sub(n).max(rope.line_to_char(rope.char_to_line(cur.min(rope.len_chars()))));
                if start < cur {
                    self.apply_operator(Op::Delete, start, cur, false, register, host, env);
                }
                change(true)
            }
            Key::Char('s') => {
                if visual {
                    let (a, b, lw) = self.visual_range(host);
                    self.apply_operator(Op::Change, a, b, lw, register, host, env);
                } else {
                    let cur = self.cursor(host);
                    let end = (cur + n).min(line_end_of(host.doc().rope(), cur));
                    self.apply_operator(Op::Change, cur, end.max(cur), false, register, host, env);
                }
                change(false)
            }
            Key::Char('R') if !visual => {
                // Replace mode: the editor's overwrite insertion until Esc.
                self.insert_repeat = 1;
                self.enter_insert(host);
                self.replace_mode = true;
                host.command(Command::ToggleOverwrite, None);
                change(false)
            }
            Key::Char('S') if !visual => {
                let cur = self.cursor(host);
                let rope = host.doc().rope();
                let line = rope.char_to_line(cur.min(rope.len_chars()));
                let last = (line + n - 1).min(rope.len_lines().saturating_sub(1));
                let (a, b) = line_range(rope, line, last);
                self.apply_operator(Op::Change, a, b, true, register, host, env);
                change(false)
            }
            Key::Char('D') if !visual => {
                let cur = self.cursor(host);
                let rope = host.doc().rope();
                let line = rope.char_to_line(cur.min(rope.len_chars()));
                let last = (line + n - 1).min(rope.len_lines().saturating_sub(1));
                let end = line_end_char(rope, last);
                self.apply_operator(Op::Delete, cur, end.max(cur), false, register, host, env);
                change(true)
            }
            Key::Char('C') if !visual => {
                let cur = self.cursor(host);
                let rope = host.doc().rope();
                let line = rope.char_to_line(cur.min(rope.len_chars()));
                let last = (line + n - 1).min(rope.len_lines().saturating_sub(1));
                let end = line_end_char(rope, last);
                self.apply_operator(Op::Change, cur, end.max(cur), false, register, host, env);
                change(false)
            }
            Key::Char('Y') => {
                if visual {
                    let (a, b, _) = self.visual_range(host);
                    let rope = host.doc().rope();
                    let (la, lb) = (rope.char_to_line(a), rope.char_to_line(b.saturating_sub(1).max(a)));
                    let (a, b) = line_range(rope, la, lb);
                    self.apply_operator(Op::Yank, a, b, true, register, host, env);
                } else {
                    let cur = self.cursor(host);
                    let rope = host.doc().rope();
                    let line = rope.char_to_line(cur.min(rope.len_chars()));
                    let last = (line + n - 1).min(rope.len_lines().saturating_sub(1));
                    let (a, b) = line_range(rope, line, last);
                    self.apply_operator(Op::Yank, a, b, true, register, host, env);
                }
                change(false)
            }
            Key::Char('p') | Key::Char('P') => {
                self.put(k == Key::Char('p'), n, register, visual, host, env);
                change(true)
            }
            Key::Char('J') => {
                let cur = self.cursor(host);
                let vr = if visual { Some(self.visual_range(host)) } else { None };
                let rope = host.doc().rope();
                let (la, lb) = if let Some((a, b, _)) = vr {
                    (rope.char_to_line(a), rope.char_to_line(b.saturating_sub(1).max(a)))
                } else {
                    let l = rope.char_to_line(cur.min(rope.len_chars()));
                    // `J` and `2J` both join two lines; `nJ` joins n lines.
                    (l, (l + n.max(2) - 1).min(rope.len_lines().saturating_sub(1)))
                };
                let joins = lb.saturating_sub(la).max(1);
                let start = rope.line_to_char(la);
                host.set_sels(SelectionSet::single(Selection::point(start)));
                for _ in 0..joins {
                    host.edit(&mut |d, s| ops::join_lines(d, s));
                }
                self.mode = Mode::Normal;
                self.clamp_normal(host);
                change(true)
            }
            Key::Char('r') => match keys.get(1) {
                None => Parse::Incomplete,
                Some(Key::Char(c)) => {
                    let c = *c;
                    if visual {
                        let (a, b, _) = self.visual_range(host);
                        host.edit(&mut |d, s| {
                            // Keep newlines so the line structure is unchanged.
                            let text: String = d.rope().slice(a..b).chars().map(|ch| if ch == '\n' { '\n' } else { c }).collect();
                            let tx = Transaction::single(Change::replace(a, b, text));
                            let mut ns = s.clone();
                            ns.set_single(Selection::point(a));
                            Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                        });
                        self.mode = Mode::Normal;
                    } else {
                        let cur = self.cursor(host);
                        let end = (cur + n).min(line_end_of(host.doc().rope(), cur));
                        // As in Vim, `3rx` does nothing if fewer than 3 chars remain on the line.
                        if end - cur == n {
                            let text: String = std::iter::repeat_n(c, n).collect();
                            host.edit(&mut |_, s| {
                                let tx = Transaction::single(Change::replace(cur, end, text.clone()));
                                let mut ns = s.clone();
                                ns.set_single(Selection::point(end - 1));
                                Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                            });
                        }
                    }
                    change(true)
                }
                Some(Key::Enter) => {
                    let cur = self.cursor(host);
                    host.edit(&mut |_, s| {
                        let tx = Transaction::single(Change::replace(cur, cur + 1, "\n"));
                        let mut ns = s.clone();
                        ns.set_single(Selection::point(cur + 1));
                        Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                    });
                    change(true)
                }
                Some(_) => Parse::Invalid,
            },
            Key::Char('~') => {
                if visual {
                    let (a, b, lw) = self.visual_range(host);
                    self.apply_operator(Op::ToggleCase, a, b, lw, register, host, env);
                } else {
                    let cur = self.cursor(host);
                    let end = (cur + n).min(line_end_of(host.doc().rope(), cur));
                    if end > cur {
                        self.apply_operator(Op::ToggleCase, cur, end, false, register, host, env);
                        host.set_sels(SelectionSet::single(Selection::point(end)));
                        self.clamp_normal(host);
                    }
                }
                change(true)
            }
            Key::Char('u') if visual => {
                let (a, b, lw) = self.visual_range(host);
                self.apply_operator(Op::Lower, a, b, lw, register, host, env);
                change(true)
            }
            Key::Char('U') if visual => {
                let (a, b, lw) = self.visual_range(host);
                self.apply_operator(Op::Upper, a, b, lw, register, host, env);
                change(true)
            }
            Key::Char('u') => {
                for _ in 0..n {
                    host.command(Command::Undo, None);
                }
                self.clamp_normal(host);
                change(false)
            }
            Key::Ctrl('r') => {
                for _ in 0..n {
                    host.command(Command::Redo, None);
                }
                change(false)
            }
            Key::Char('.') if !visual => {
                if self.last_change.is_empty() {
                    return change(false);
                }
                let mut keys = self.last_change.clone();
                if count.is_some() {
                    // A count given to `.` replaces the original count.
                    let (_, skip) = parse_count(&keys, 0);
                    keys.drain(..skip);
                    let mut prefixed: Vec<Key> = n.to_string().chars().map(Key::Char).collect();
                    prefixed.append(&mut keys);
                    keys = prefixed;
                }
                // The replay feeds keys back through `key`, which owns `pending`;
                // stash it so the `.` sequence itself is not mixed in.
                self.replaying = true;
                let saved_pending = std::mem::take(&mut self.pending);
                for key in keys {
                    self.key(key, env, host);
                }
                if self.mode == Mode::Insert {
                    // A replayed insert ends where the original did.
                    self.key(Key::Esc, env, host);
                }
                self.replaying = false;
                self.pending = saved_pending;
                change(false)
            }
            Key::Char('m') => match keys.get(1) {
                None => Parse::Incomplete,
                Some(Key::Char(c)) if c.is_ascii_alphabetic() => {
                    let p = self.cursor(host);
                    let _ = p;
                    // Marks live in the document (prefixed `vim:`); the host sets
                    // the mark at the current caret.
                    host.command(Command::SetMark, Some(format!("vim:{c}")));
                    change(false)
                }
                Some(_) => Parse::Invalid,
            },
            Key::Char('Z') => match keys.get(1) {
                None => Parse::Incomplete,
                Some(Key::Char('Z')) => {
                    host.command(Command::Save, None);
                    host.command(Command::CloseTab, None);
                    change(false)
                }
                Some(Key::Char('Q')) => {
                    host.command(Command::CloseTab, Some("force".into()));
                    change(false)
                }
                Some(_) => Parse::Invalid,
            },
            Key::Ctrl('e') => {
                host.command(Command::ScrollLineDown, None);
                change(false)
            }
            Key::Ctrl('y') => {
                host.command(Command::ScrollLineUp, None);
                change(false)
            }
            Key::Char('z') => match keys.get(1) {
                None => Parse::Incomplete,
                Some(Key::Char('z')) | Some(Key::Char('.')) => {
                    host.command(Command::CentreCursor, None);
                    change(false)
                }
                Some(Key::Char('c')) => {
                    host.command(Command::FoldCurrent, None);
                    change(false)
                }
                Some(Key::Char('o')) => {
                    host.command(Command::UnfoldCurrent, None);
                    change(false)
                }
                Some(Key::Char('a')) => {
                    host.command(Command::ToggleFold, None);
                    change(false)
                }
                Some(Key::Char('M')) => {
                    host.command(Command::FoldAll, None);
                    change(false)
                }
                Some(Key::Char('R')) => {
                    host.command(Command::UnfoldAll, None);
                    change(false)
                }
                Some(_) => Parse::Invalid,
            },
            Key::Ctrl('v') => {
                host.status("Visual block mode is not supported; use column selection (Ctrl+Shift+Up/Down)".into(), false);
                change(false)
            }
            Key::Char('q') | Key::Char('@') => {
                // Macros are the editor's own (Tools > Macros); keep the key from doing nothing silently.
                host.status("Use Tools > Macros (Ctrl+Alt+R) to record and play macros".into(), false);
                change(false)
            }
            Key::Ctrl('o') => {
                host.command(Command::NavigateBack, None);
                change(false)
            }
            Key::Ctrl('i') => {
                host.command(Command::NavigateForward, None);
                change(false)
            }
            _ => {
                // Plain motion (moves the cursor or extends the visual selection).
                match self.parse_target(keys, n, env, host, false) {
                    Target::Incomplete => Parse::Incomplete,
                    Target::Invalid => Parse::Invalid,
                    Target::Span(span) => {
                        self.apply_motion(span, host, env);
                        change(false)
                    }
                }
            }
        }
    }

    // ----- motions and text objects --------------------------------------------------

    /// Parse a motion or (after an operator / in visual mode) a text object.
    /// `n` is the effective count; `after_op` is true when an operator is waiting,
    /// which changes some motions (`l`, `w`) and enables `i`/`a` objects and `n`/`N`.
    /// `g` commands that act directly on the host return `Target::Invalid`
    /// after running, so no motion follows.
    fn parse_target(&mut self, keys: &[Key], n: usize, env: &Env, host: &mut dyn VimHost, after_op: bool) -> Target {
        let Some(&k) = keys.first() else { return Target::Incomplete };
        // Keys that act on the host rather than yield a span.
        if k == Key::Char('g') {
            let cmd = match keys.get(1) {
                None => return Target::Incomplete,
                Some(Key::Char('d')) => Some(Command::GotoDefinition),
                Some(Key::Char('J')) => Some(Command::JoinLines),
                Some(Key::Char('t')) => Some(Command::NextTab),
                Some(Key::Char('T')) => Some(Command::PrevTab),
                Some(Key::Char('v')) => {
                    self.start_visual(Mode::Visual, host);
                    return Target::Invalid;
                }
                _ => None,
            };
            if let Some(c) = cmd {
                host.command(c, None);
                return Target::Invalid;
            }
        }
        let mark_pos = if matches!(k, Key::Char('\'') | Key::Char('`')) {
            match keys.get(1) {
                None => return Target::Incomplete,
                Some(Key::Char(m)) => match host.doc().mark(&format!("vim:{m}")) {
                    Some(p) => Some(p),
                    None => {
                        host.status(format!("Mark not set: {m}"), true);
                        return Target::Invalid;
                    }
                },
                Some(_) => return Target::Invalid,
            }
        } else {
            None
        };
        let doc = host.doc();
        let rope = doc.rope();
        let cur = self.cursor(host).min(rope.len_chars());
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        let line = rope.char_to_line(cur);
        let last_line = rope.len_lines().saturating_sub(1);
        let span = match k {
            Key::Char('h') | Key::Left | Key::Backspace => {
                let ls = rope.line_to_char(line);
                let mut p = cur;
                for _ in 0..n {
                    if p > ls {
                        p = motion::prev_grapheme_boundary(doc.slice(), p);
                    }
                }
                Span::Exclusive(p)
            }
            Key::Char('l') | Key::Right | Key::Char(' ') => {
                let le = line_end_char(rope, line);
                let mut p = cur;
                for _ in 0..n {
                    if p < le {
                        p = motion::next_grapheme_boundary(doc.slice(), p);
                    }
                }
                // `l` under an operator may reach the line end; as a motion it stops before it.
                if !after_op && !visual && p >= le && le > ls_of(rope, line) {
                    p = motion::prev_grapheme_boundary(doc.slice(), le);
                }
                Span::Exclusive(p)
            }
            // Vertical motions yield the target line; `apply_motion` applies the goal column.
            Key::Char('j') | Key::Down | Key::Ctrl('n') | Key::Ctrl('j') => Span::Linewise((line + n).min(last_line)),
            Key::Char('k') | Key::Up | Key::Ctrl('p') => Span::Linewise(line.saturating_sub(n)),
            Key::Char('0') | Key::Home => Span::Exclusive(rope.line_to_char(line)),
            Key::Char('^') => Span::Exclusive(first_non_blank_of_line(rope, cur)),
            Key::Char('$') | Key::End => {
                let l = (line + n - 1).min(last_line);
                let e = line_end_char(rope, l);
                // The newline itself is never part of `$`.
                if e > rope.line_to_char(l) { Span::Inclusive(e - 1) } else { Span::Exclusive(e) }
            }
            Key::Char('w') | Key::Char('W') => {
                let big = k == Key::Char('W');
                let mut p = cur;
                for _ in 0..n {
                    p = if big { next_big_word_start(rope, p) } else { motion::next_word_start(doc.slice(), p) };
                }
                // `cw` behaves like `ce`; `dw` on the last word of a line stops at the line end.
                if after_op {
                    let le = line_end_char(rope, line);
                    if p > le && line < last_line {
                        p = le;
                    }
                }
                Span::Exclusive(p)
            }
            Key::Char('b') | Key::Char('B') => {
                let big = k == Key::Char('B');
                let mut p = cur;
                for _ in 0..n {
                    p = if big { prev_big_word_start(rope, p) } else { motion::prev_word_start(doc.slice(), p) };
                }
                Span::Exclusive(p)
            }
            Key::Char('e') | Key::Char('E') => {
                let big = k == Key::Char('E');
                let mut p = cur;
                for _ in 0..n {
                    // Vim's `e` moves at least one char before looking for an end.
                    let from = (p + 1).min(rope.len_chars());
                    let e = if big { next_big_word_end(rope, from) } else { motion::next_word_end(doc.slice(), from) };
                    // `next_word_end` returns an exclusive end; the caret sits on the last char.
                    p = e.saturating_sub(1).max(p);
                    if e == from {
                        break;
                    }
                }
                Span::Inclusive(p)
            }
            Key::Char('g') => match keys.get(1) {
                None => return Target::Incomplete,
                // `gg`/`G` with a count go to that (1-based) line.
                Some(Key::Char('g')) => Span::Linewise(if n > 1 { (n - 1).min(last_line) } else { 0 }),
                Some(Key::Char('e')) => {
                    let mut p = cur;
                    for _ in 0..n {
                        p = motion::prev_word_start(doc.slice(), p).saturating_sub(1);
                    }
                    Span::Inclusive(p)
                }
                Some(Key::Char('_')) => {
                    let l = (line + n - 1).min(last_line);
                    let e = line_end_char(rope, l);
                    let mut p = e;
                    // `g_`: last non-blank char of the line.
                    while p > rope.line_to_char(l) && rope.char(p - 1).is_whitespace() {
                        p -= 1;
                    }
                    Span::Inclusive(p.saturating_sub(1).max(rope.line_to_char(l)))
                }
                Some(_) => return Target::Invalid,
            },
            Key::Char('G') => Span::Linewise(if n > 1 { (n - 1).min(last_line) } else { last_line }),
            Key::Char('{') => {
                let mut l = line;
                for _ in 0..n {
                    l = prev_paragraph(rope, l);
                }
                Span::Exclusive(rope.line_to_char(l))
            }
            Key::Char('}') => {
                let mut l = line;
                for _ in 0..n {
                    l = next_paragraph(rope, l);
                }
                Span::Exclusive(if l >= last_line { rope.len_chars() } else { rope.line_to_char(l) })
            }
            Key::Char('%') => match apex_edit::brackets::matching_bracket(rope, cur).or_else(|| {
                // No bracket under the cursor: use the first one after it on the line.
                let le = line_end_char(rope, line);
                (cur..le).find(|&p| "([{}])".contains(rope.char(p))).and_then(|p| apex_edit::brackets::matching_bracket(rope, p))
            }) {
                // From the opener (or before the pair) go to the closer, otherwise to the opener.
                Some((a, b)) => Span::Inclusive(if cur == a || (cur != b && cur < a) { b } else { a }),
                None => return Target::Invalid,
            },
            Key::Char(c @ ('f' | 'F' | 't' | 'T')) => match keys.get(1) {
                None => return Target::Incomplete,
                Some(Key::Char(target)) => {
                    let kind = match c {
                        'f' => FindKind::F,
                        'F' => FindKind::BackF,
                        't' => FindKind::T,
                        _ => FindKind::BackT,
                    };
                    self.last_find = Some((*target, kind));
                    match find_char(rope, cur, *target, kind, n) {
                        // Backward finds exclude the char under the caret, as in Vim.
                        Some(p) => {
                            if matches!(kind, FindKind::F | FindKind::T) {
                                Span::Inclusive(p)
                            } else {
                                Span::Exclusive(p)
                            }
                        }
                        None => return Target::Invalid,
                    }
                }
                Some(_) => return Target::Invalid,
            },
            Key::Char(c @ (';' | ',')) => {
                let Some((target, kind)) = self.last_find else { return Target::Invalid };
                let kind = if c == ',' {
                    match kind {
                        FindKind::F => FindKind::BackF,
                        FindKind::BackF => FindKind::F,
                        FindKind::T => FindKind::BackT,
                        FindKind::BackT => FindKind::T,
                    }
                } else {
                    kind
                };
                // Repeating `t` from directly before the target would not move: start one further.
                let from = match kind {
                    FindKind::T if rope.get_char(cur + 1) == Some(target) => cur + 1,
                    FindKind::BackT if cur > 0 && rope.get_char(cur - 1) == Some(target) => cur - 1,
                    _ => cur,
                };
                match find_char(rope, from, target, kind, n) {
                    Some(p) => {
                        if matches!(kind, FindKind::F | FindKind::T) {
                            Span::Inclusive(p)
                        } else {
                            Span::Exclusive(p)
                        }
                    }
                    None => return Target::Invalid,
                }
            }
            Key::Char('\'') | Key::Char('`') => {
                let p = mark_pos.unwrap_or(cur).min(rope.len_chars());
                if k == Key::Char('\'') { Span::Linewise(rope.char_to_line(p)) } else { Span::Exclusive(p) }
            }
            // `H`/`M`/`L` are relative to `top_line_hint`, an approximation (see there).
            Key::Char('H') => Span::Linewise(self.top_line_hint(host).min(last_line)),
            Key::Char('L') => Span::Linewise((self.top_line_hint(host) + env.page_lines.saturating_sub(1)).min(last_line)),
            Key::Char('M') => Span::Linewise((self.top_line_hint(host) + env.page_lines / 2).min(last_line)),
            Key::Ctrl('f') | Key::PageDown => Span::Linewise((line + env.page_lines.max(1) * n).min(last_line)),
            Key::Ctrl('b') | Key::PageUp => Span::Linewise(line.saturating_sub(env.page_lines.max(1) * n)),
            Key::Ctrl('d') => Span::Linewise((line + (env.page_lines / 2).max(1) * n).min(last_line)),
            Key::Ctrl('u') => Span::Linewise(line.saturating_sub((env.page_lines / 2).max(1) * n)),
            Key::Enter | Key::Char('+') => Span::Linewise((line + n).min(last_line)),
            Key::Char('-') => Span::Linewise(line.saturating_sub(n)),
            Key::Char('_') => Span::Linewise((line + n - 1).min(last_line)),
            // `n|`: display column n (1-based) of the current line, clamped to its end.
            Key::Char('|') => {
                let l = rope.line(line);
                Span::Exclusive(
                    rope.line_to_char(line) + display_col_to_char_col(l, n - 1, env.tab_width).min(line_end_char(rope, line) - rope.line_to_char(line)),
                )
            }
            Key::Char(c @ ('i' | 'a')) if after_op || visual => match keys.get(1) {
                None => return Target::Incomplete,
                Some(Key::Char(obj)) => match text_object(rope, cur, *obj, c == 'a', n) {
                    Some((a, b, lw)) => Span::Range(a, b, lw),
                    None => return Target::Invalid,
                },
                Some(_) => return Target::Invalid,
            },
            Key::Char('n') | Key::Char('N') if after_op => {
                let Some((pat, fwd)) = self.last_search.clone() else { return Target::Invalid };
                // `N` reverses the stored direction.
                let forward = (k == Key::Char('n')) == fwd;
                match search_from(rope, &pat, cur, forward, n) {
                    Some(p) => Span::Exclusive(p),
                    None => return Target::Invalid,
                }
            }
            _ => return Target::Invalid,
        };
        Target::Span(span)
    }

    /// Line used as the top of the screen for `H`/`M`/`L`. The engine has no
    /// view, so this is the caret line.
    fn top_line_hint(&self, host: &mut dyn VimHost) -> usize {
        // Without the view we approximate H/M/L from the cursor line (the host scrolls to it).
        let rope = host.doc().rope();
        rope.char_to_line(self.cursor(host).min(rope.len_chars()))
    }

    /// Move the caret (or the visual cursor) to a motion's target. Linewise
    /// targets keep `goal_col`; other motions clear it. In visual mode a text
    /// object replaces the selection.
    fn apply_motion(&mut self, span: Span, host: &mut dyn VimHost, env: &Env) {
        let rope = host.doc().rope();
        let cur = self.cursor(host).min(rope.len_chars());
        let vertical = matches!(span, Span::Linewise(_));
        let target = match span {
            Span::Exclusive(p) | Span::Inclusive(p) => p,
            Span::Linewise(l) => {
                let goal = self.goal_col.unwrap_or_else(|| {
                    char_col_to_display_col(rope.line(rope.char_to_line(cur)), cur - rope.line_to_char(rope.char_to_line(cur)), env.tab_width)
                });
                self.goal_col = Some(goal);
                let ls = rope.line_to_char(l);
                let le = line_end_char(rope, l);
                let col = display_col_to_char_col(rope.line(l), goal, env.tab_width);
                let p = (ls + col).min(le);
                // Normal mode never rests on the newline (except on an empty line).
                if !matches!(self.mode, Mode::Visual | Mode::VisualLine) && p == le && le > ls { le - 1 } else { p }
            }
            Span::Range(a, b, _) => {
                // Text object in visual mode: select it.
                if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                    self.visual_anchor = a;
                    self.set_visual_cursor(b.saturating_sub(1).max(a), host);
                }
                return;
            }
        };
        if !vertical {
            self.goal_col = None;
        }
        match self.mode {
            Mode::Visual | Mode::VisualLine => self.set_visual_cursor(target, host),
            _ => {
                let sels = host.sels();
                if sels.len() > 1 {
                    // ponytail: secondary carets move by the primary's delta; exact per-caret
                    // motions would need the motion re-run for each caret.
                    let primary = sels.primary_index();
                    let delta = target as i64 - cur as i64;
                    let n = rope.len_chars() as i64;
                    let v: Vec<Selection> = sels
                        .iter()
                        .enumerate()
                        .map(|(i, s)| Selection::point(if i == primary { target } else { (s.head as i64 + delta).clamp(0, n) as usize }))
                        .collect();
                    host.set_sels(SelectionSet::from_vec(v, primary));
                } else {
                    host.set_sels(SelectionSet::single(Selection::point(target)));
                }
                self.clamp_normal(host);
            }
        }
    }

    // ----- visual helpers ------------------------------------------------------------

    /// Enter `mode` (`Visual` or `VisualLine`) from the current primary selection;
    /// collapses to a single selection.
    fn start_visual(&mut self, mode: Mode, host: &mut dyn VimHost) {
        let s = host.sels().primary();
        self.mode = mode;
        if s.is_empty() {
            self.visual_anchor = s.head;
            self.set_visual_cursor(s.head, host);
        } else {
            // A mouse or Ctrl+D selection becomes the visual selection.
            self.visual_anchor = s.anchor.min(s.head);
            self.set_visual_cursor(s.end().saturating_sub(1).max(s.start()), host);
        }
    }

    /// Set the cursor in visual mode and derive the (inclusive) editor selection.
    fn set_visual_cursor(&mut self, cursor: usize, host: &mut dyn VimHost) {
        let rope = host.doc().rope();
        let n = rope.len_chars();
        let cursor = cursor.min(n);
        let anchor = self.visual_anchor.min(n);
        let sel = match self.mode {
            // Keep the editor selection's direction matching anchor -> cursor.
            Mode::VisualLine => {
                let (la, lb) = (rope.char_to_line(anchor), rope.char_to_line(cursor));
                let (a, b) = line_range(rope, la.min(lb), la.max(lb));
                if cursor >= anchor { Selection::new(a, b) } else { Selection::new(b, a) }
            }
            _ => {
                if cursor >= anchor {
                    Selection::new(anchor, motion::next_grapheme_boundary(host.doc().slice(), cursor).max(cursor).min(n))
                } else {
                    Selection::new(motion::next_grapheme_boundary(host.doc().slice(), anchor).min(n), cursor)
                }
            }
        };
        host.set_sels(SelectionSet::single(sel));
    }

    /// Char range `[start, end)` covered by the visual selection, plus whether it is linewise.
    fn visual_range(&self, host: &mut dyn VimHost) -> (usize, usize, bool) {
        let s = host.sels().primary();
        (s.start(), s.end(), self.mode == Mode::VisualLine)
    }

    // ----- operators -----------------------------------------------------------------

    /// Apply `op` to the char range `[a, b)`. Deleted or yanked text goes to
    /// `register` (and the unnamed/numbered registers); `Change` then enters
    /// insert mode, everything else returns to normal mode with the caret near `a`.
    #[allow(clippy::too_many_arguments)]
    fn apply_operator(&mut self, op: Op, a: usize, b: usize, linewise: bool, register: Option<char>, host: &mut dyn VimHost, env: &Env) {
        let rope = host.doc().rope();
        let n = rope.len_chars();
        let (a, b) = (a.min(n), b.min(n));
        let text: String = rope.slice(a..b).to_string();
        match op {
            Op::Yank => {
                self.store_register(register, text, linewise, host, true);
                self.mode = Mode::Normal;
                host.set_sels(SelectionSet::single(Selection::point(a)));
                self.clamp_normal(host);
            }
            Op::Delete | Op::Change => {
                // An empty `c` (e.g. `cl` at line end) must not clobber the registers.
                if op == Op::Delete || !text.is_empty() {
                    self.store_register(register, text.clone(), linewise, host, false);
                }
                let keep_line = op == Op::Change && linewise;
                let rope = host.doc().rope();
                let (da, db) = if keep_line {
                    // `cc`: keep the indentation of the first line, drop the rest.
                    let first = rope.char_to_line(a);
                    let indent_end = first_non_blank_of_line(rope, rope.line_to_char(first));
                    let last_nl = if b > a && rope.char(b - 1) == '\n' { b - 1 } else { b };
                    (indent_end.min(last_nl), last_nl)
                } else {
                    (a, b)
                };
                host.edit(&mut |_, s| {
                    let tx = Transaction::single(Change::delete(da, db));
                    let mut ns = s.clone();
                    ns.set_single(Selection::point(da));
                    Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                });
                self.mode = Mode::Normal;
                if op == Op::Change {
                    self.insert_repeat = 1;
                    self.enter_insert(host);
                } else {
                    if linewise {
                        self.move_all(host, |d, p| first_non_blank_of_line(d.rope(), p), false, env);
                    }
                    self.clamp_normal(host);
                }
            }
            Op::Indent | Op::Outdent => {
                let (la, lb) = (rope.char_to_line(a), rope.char_to_line(b.saturating_sub(1).max(a)));
                let (ra, rb) = line_range(rope, la, lb);
                // Select the lines (without the final newline) so the editor's
                // indent/outdent ops act on exactly those lines.
                host.set_sels(SelectionSet::single(Selection::new(ra, rb.saturating_sub(1).max(ra))));
                host.edit(&mut |d, s| if op == Op::Indent { ops::indent_lines(d, s, d.indent) } else { ops::outdent_lines(d, s, d.indent) });
                self.mode = Mode::Normal;
                let rope = host.doc().rope();
                let p = first_non_blank_of_line(rope, rope.line_to_char(la.min(rope.len_lines().saturating_sub(1))));
                host.set_sels(SelectionSet::single(Selection::point(p)));
            }
            Op::Format => {
                // `Reindent` works on the current selection, so select the range first.
                host.set_sels(SelectionSet::single(Selection::new(a, b)));
                host.command(Command::Reindent, None);
                self.mode = Mode::Normal;
                host.set_sels(SelectionSet::single(Selection::point(a)));
                self.clamp_normal(host);
            }
            Op::Upper | Op::Lower | Op::ToggleCase => {
                let new: String = match op {
                    Op::Upper => text.to_uppercase(),
                    Op::Lower => text.to_lowercase(),
                    _ => text.chars().map(|c| if c.is_uppercase() { c.to_lowercase().collect::<String>() } else { c.to_uppercase().collect() }).collect(),
                };
                if new != text {
                    host.edit(&mut |_, s| {
                        let tx = Transaction::single(Change::replace(a, b, new.clone()));
                        let mut ns = s.clone();
                        ns.set_single(Selection::point(a));
                        Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                    });
                }
                self.mode = Mode::Normal;
                host.set_sels(SelectionSet::single(Selection::point(a)));
                self.clamp_normal(host);
            }
        }
    }

    /// Store yanked (`yank`) or deleted text, following Vim's register rules:
    /// `_` discards, `+`/`*` also set the system clipboard, `A`-`Z` append to
    /// `a`-`z`. The unnamed register always updates; yanks also set `0`,
    /// multi-line deletes shift `1`-`9`, small deletes set `-`.
    fn store_register(&mut self, register: Option<char>, text: String, linewise: bool, host: &mut dyn VimHost, yank: bool) {
        let reg = Register { text: text.clone(), linewise };
        match register {
            Some('_') => return,
            Some(c @ ('+' | '*')) => {
                host.clipboard_set(text.clone());
                self.registers.insert(c, reg.clone());
            }
            Some(c) if c.is_ascii_uppercase() => {
                let lower = c.to_ascii_lowercase();
                let e = self.registers.entry(lower).or_default();
                e.text.push_str(&text);
                e.linewise |= linewise;
            }
            Some(c) => {
                self.registers.insert(c, reg.clone());
            }
            None => {}
        }
        self.registers.insert('"', reg.clone());
        if yank {
            self.registers.insert('0', reg);
        } else if text.contains('\n') || linewise {
            // Shift the numbered delete registers.
            for i in (1..9).rev() {
                if let Some(r) = self.registers.get(&char::from(b'0' + i)).cloned() {
                    self.registers.insert(char::from(b'0' + i + 1), r);
                }
            }
            self.registers.insert('1', reg);
        } else {
            self.registers.insert('-', reg);
        }
    }

    /// `p` (`after`) or `P`: paste register contents `n` times. Linewise text
    /// goes below/above the current line; otherwise after/at the caret. With no
    /// register and an empty unnamed register, falls back to the system clipboard.
    /// In visual mode replaces the selection.
    fn put(&mut self, after: bool, n: usize, register: Option<char>, visual: bool, host: &mut dyn VimHost, env: &Env) {
        let reg = match register {
            Some('+' | '*') => {
                // The system clipboard is read asynchronously by the window system.
                host.system_paste();
                return;
            }
            Some(c) => self.registers.get(&c.to_ascii_lowercase()).cloned(),
            None => self.registers.get(&'"').cloned().or_else(|| host.clipboard_get().map(|t| Register { linewise: t.ends_with('\n'), text: t })),
        };
        let Some(reg) = reg else {
            host.status("Nothing in register".into(), true);
            return;
        };
        if reg.text.is_empty() {
            return;
        }
        let text = reg.text.repeat(n);
        if visual {
            // Replace the selection; the replaced text goes to the unnamed register.
            let (a, b, lw) = self.visual_range(host);
            let old: String = host.doc().rope().slice(a..b).to_string();
            let mut ins = text.clone();
            // Bridge linewise/characterwise mismatches with an extra newline.
            if reg.linewise && !lw {
                ins = format!("\n{ins}");
            } else if !reg.linewise && lw {
                ins.push('\n');
            }
            host.edit(&mut |_, s| {
                let tx = Transaction::single(Change::replace(a, b, ins.clone()));
                let mut ns = s.clone();
                ns.set_single(Selection::point(a + ins.chars().count().saturating_sub(1)));
                Some(EditResult { tx, sels: ns, kind: EditKind::Other })
            });
            self.registers.insert('"', Register { text: old, linewise: lw });
            self.mode = Mode::Normal;
            self.clamp_normal(host);
            return;
        }
        let rope = host.doc().rope();
        let cur = self.cursor(host).min(rope.len_chars());
        if reg.linewise {
            let line = rope.char_to_line(cur);
            let at = if after { if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() } } else { rope.line_to_char(line) };
            let mut ins = text;
            // Pasting below a last line that has no newline: add one first.
            let needs_nl_before = after && line + 1 >= rope.len_lines() && !rope.to_string().ends_with('\n') && rope.len_chars() > 0;
            if needs_nl_before {
                ins = format!("\n{ins}");
            }
            if !ins.ends_with('\n') {
                ins.push('\n');
            }
            let first_line_start = at + if needs_nl_before { 1 } else { 0 };
            host.edit(&mut |_, s| {
                let tx = Transaction::single(Change::insert(at, ins.clone()));
                let mut ns = s.clone();
                let p = first_non_blank_of_line_text(&ins, first_line_start);
                ns.set_single(Selection::point(p));
                Some(EditResult { tx, sels: ns, kind: EditKind::Other })
            });
        } else {
            let at = if after && !at_line_end(rope, cur) { cur + 1 } else { cur };
            let len = text.chars().count();
            host.edit(&mut |_, s| {
                let tx = Transaction::single(Change::insert(at, text.clone()));
                let mut ns = s.clone();
                ns.set_single(Selection::point(at + len.saturating_sub(1)));
                Some(EditResult { tx, sels: ns, kind: EditKind::Other })
            });
        }
        let _ = env;
        self.clamp_normal(host);
    }

    // ----- search --------------------------------------------------------------------

    /// `n`/`N`: jump to the `count`th match of the last search, in its stored
    /// direction (`same_direction`) or the reverse. Also hands the pattern to
    /// the editor's find so matches are highlighted.
    fn search_step(&mut self, count: i64, same_direction: bool, host: &mut dyn VimHost) {
        let Some((pat, fwd)) = self.last_search.clone() else {
            host.status("No previous search".into(), true);
            return;
        };
        let forward = fwd == same_direction;
        let rope = host.doc().rope();
        let cur = self.cursor(host).min(rope.len_chars());
        match search_from(rope, &pat, cur, forward, count.unsigned_abs() as usize) {
            Some(p) => {
                match self.mode {
                    Mode::Visual | Mode::VisualLine => self.set_visual_cursor(p, host),
                    _ => host.set_sels(SelectionSet::single(Selection::point(p))),
                }
                host.command(Command::UseSelectionForFind, Some(pat));
            }
            None => host.status(format!("Pattern not found: {pat}"), true),
        }
    }

    // ----- misc helpers --------------------------------------------------------------

    /// Switch to insert mode: open an undo group, remember where insertion
    /// started and collapse selections to their heads.
    fn enter_insert(&mut self, host: &mut dyn VimHost) {
        host.undo_boundary();
        self.mode = Mode::Insert;
        self.insert_start = self.cursor(host);
        let mut sels = host.sels();
        sels.transform(|s| Selection::point(s.head));
        host.set_sels(sels);
    }

    /// `:n`: put the caret on the first non-blank of 1-based line `n` (clamped).
    fn goto_line(&mut self, n: usize, host: &mut dyn VimHost) {
        let rope = host.doc().rope();
        let line = n.saturating_sub(1).min(rope.len_lines().saturating_sub(1));
        let p = first_non_blank_of_line(rope, rope.line_to_char(line));
        host.set_sels(SelectionSet::single(Selection::point(p)));
    }

    /// Move every caret to `f(doc, head)` (char index, clamped to the document).
    fn move_all(&mut self, host: &mut dyn VimHost, f: impl Fn(&Document, usize) -> usize, extend: bool, _env: &Env) {
        let mut sels = host.sels();
        let doc = host.doc();
        let n = doc.len_chars();
        sels.transform(|s| s.moved_to(f(doc, s.head.min(n)).min(n), extend));
        host.set_sels(sels);
    }

    /// The Vim cursor: in visual mode the editor selection's head is exclusive,
    /// so the cursor is the character before it when the selection runs forward.
    fn cursor(&self, host: &dyn VimHost) -> usize {
        let s = host.sels().primary();
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) && s.head > s.anchor {
            motion::prev_grapheme_boundary(host.doc().slice(), s.head)
        } else {
            s.head
        }
    }

    /// In normal mode the caret may not sit on a line's newline.
    fn clamp_normal(&mut self, host: &mut dyn VimHost) {
        if self.mode != Mode::Normal {
            return;
        }
        let mut sels = host.sels();
        let rope = host.doc().rope();
        let n = rope.len_chars();
        sels.transform(|s| {
            let p = s.head.min(n);
            let line = rope.char_to_line(p);
            let ls = rope.line_to_char(line);
            let le = line_end_char(rope, line);
            let p = if p >= le && le > ls { motion::prev_grapheme_boundary(rope.slice(..), le) } else { p };
            Selection::point(p)
        });
        host.set_sels(sels);
    }
}

/// Line range prefix of an ex command.
#[derive(Debug, Clone, Copy)]
enum ExRange {
    /// No prefix: the caret line.
    Line,
    /// `%`: the whole document.
    All,
    /// `'<,'>`: the lines of the visual selection.
    Visual,
}

/// Outcome of parsing a normal/visual key sequence.
enum Parse {
    /// Need more keys; keep them pending.
    Incomplete,
    /// Not a valid command; discard the keys.
    Invalid,
    /// Executed. `change` is true if it edited text and should be repeatable with `.`.
    Done { change: bool },
}

/// Outcome of parsing a motion or text object.
enum Target {
    /// Need more keys.
    Incomplete,
    /// Not a motion (or a `g` command already run); no span.
    Invalid,
    Span(Span),
}

/// Parse a decimal count starting at `keys[i]`. A leading `0` is not a count
/// (it is the `0` motion). Returns the count (capped at 1,000,000) and the
/// index after it.
fn parse_count(keys: &[Key], mut i: usize) -> (Option<usize>, usize) {
    let mut n: Option<usize> = None;
    while let Some(Key::Char(c)) = keys.get(i) {
        if let Some(d) = c.to_digit(10)
            && (n.is_some() || d != 0)
        {
            n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(1_000_000));
            i += 1;
        } else {
            break;
        }
    }
    (n, i)
}

/// Char index of the start of `line`.
fn ls_of(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line)
}

/// Whether char index `p` is at (or past) the end of its line's content.
fn at_line_end(rope: &Rope, p: usize) -> bool {
    let p = p.min(rope.len_chars());
    p >= line_end_char(rope, rope.char_to_line(p))
}

/// Char index of the end of the line containing `p` (before the newline).
fn line_end_of(rope: &Rope, p: usize) -> usize {
    line_end_char(rope, rope.char_to_line(p.min(rope.len_chars())))
}

/// Char index of the first non-blank on the line containing `p`, or the line
/// end if the line is blank.
fn first_non_blank_of_line(rope: &Rope, p: usize) -> usize {
    let line = rope.char_to_line(p.min(rope.len_chars()));
    let ls = rope.line_to_char(line);
    (ls + motion::first_non_blank(rope.line(line))).min(line_end_char(rope, line))
}

/// Char index of the first non-blank of the first line of `inserted`, given
/// that it will be placed at char index `start`.
fn first_non_blank_of_line_text(inserted: &str, start: usize) -> usize {
    let first = inserted.lines().next().unwrap_or("");
    start + first.chars().take_while(|c| c.is_whitespace()).count()
}

/// Char range `[a, b)` covering whole lines `la..=lb` including the trailing newline.
fn line_range(rope: &Rope, la: usize, lb: usize) -> (usize, usize) {
    let a = rope.line_to_char(la);
    let b = if lb + 1 < rope.len_lines() { rope.line_to_char(lb + 1) } else { rope.len_chars() };
    (a, b)
}

/// Convert a span relative to `cur` into a char range plus linewise flag.
fn span_range(rope: &Rope, cur: usize, span: Span) -> (usize, usize, bool) {
    let n = rope.len_chars();
    let cur = cur.min(n);
    match span {
        Span::Exclusive(p) => (cur.min(p), cur.max(p), false),
        Span::Inclusive(p) => {
            let (a, b) = (cur.min(p), cur.max(p));
            // Extend past the last grapheme so the end is exclusive.
            (a, motion::next_grapheme_boundary(rope.slice(..), b).max(b).min(n), false)
        }
        Span::Linewise(l) => {
            let cl = rope.char_to_line(cur);
            let (a, b) = line_range(rope, cl.min(l), cl.max(l));
            (a, b, true)
        }
        Span::Range(a, b, lw) => (a.min(n), b.min(n), lw),
    }
}

/// Whether `c` belongs to a WORD (`W`/`B`/`E`): any non-whitespace.
fn is_big_word_char(c: char) -> bool {
    !c.is_whitespace()
}

/// `W`: char index of the next WORD start after `p` (or the document end).
fn next_big_word_start(rope: &Rope, p: usize) -> usize {
    let n = rope.len_chars();
    let mut i = p.min(n);
    while i < n && is_big_word_char(rope.char(i)) {
        i += 1;
    }
    while i < n && rope.char(i).is_whitespace() {
        i += 1;
    }
    i
}

/// Exclusive end (char index) of the next WORD at or after `p`, skipping leading whitespace.
fn next_big_word_end(rope: &Rope, p: usize) -> usize {
    let n = rope.len_chars();
    let mut i = p.min(n);
    while i < n && rope.char(i).is_whitespace() {
        i += 1;
    }
    while i < n && is_big_word_char(rope.char(i)) {
        i += 1;
    }
    i
}

/// `B`: char index of the start of the WORD before `p`.
fn prev_big_word_start(rope: &Rope, p: usize) -> usize {
    let mut i = p.min(rope.len_chars());
    while i > 0 && rope.char(i - 1).is_whitespace() {
        i -= 1;
    }
    while i > 0 && is_big_word_char(rope.char(i - 1)) {
        i -= 1;
    }
    i
}

/// Whether line `l` is empty or whitespace only.
fn line_is_blank(rope: &Rope, l: usize) -> bool {
    rope.line(l).chars().all(char::is_whitespace)
}

/// `}`: the next blank line after the paragraph at `line` (skipping blank
/// lines first), or the last line.
fn next_paragraph(rope: &Rope, line: usize) -> usize {
    let last = rope.len_lines().saturating_sub(1);
    let mut l = line;
    while l < last && line_is_blank(rope, l) {
        l += 1;
    }
    while l < last && !line_is_blank(rope, l) {
        l += 1;
    }
    l
}

/// `{`: the previous blank line before the paragraph at `line`, or line 0.
fn prev_paragraph(rope: &Rope, line: usize) -> usize {
    let mut l = line;
    while l > 0 && line_is_blank(rope, l) {
        l -= 1;
    }
    while l > 0 && !line_is_blank(rope, l) {
        l -= 1;
    }
    l
}

/// `f`/`F`/`t`/`T`: char index of the `n`th `target` on the caret's line in
/// the given direction (adjusted by one for `t`/`T`), or `None` if absent.
fn find_char(rope: &Rope, cur: usize, target: char, kind: FindKind, n: usize) -> Option<usize> {
    let line = rope.char_to_line(cur.min(rope.len_chars()));
    let ls = rope.line_to_char(line);
    let le = line_end_char(rope, line);
    let mut p = cur;
    for _ in 0..n {
        p = match kind {
            FindKind::F | FindKind::T => ((p + 1)..le).find(|&i| rope.char(i) == target)?,
            FindKind::BackF | FindKind::BackT => (ls..p).rev().find(|&i| rope.char(i) == target)?,
        };
    }
    Some(match kind {
        FindKind::T => p.saturating_sub(1),
        FindKind::BackT => p + 1,
        _ => p,
    })
}

/// Backslash-escape regex metacharacters so `s` matches literally.
fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Position of the `n`th match of `pat` after/before `cur`, wrapping around.
/// `pat` is a regex, case-sensitive only if it contains an upper-case letter
/// (smart case). Returns the match start as a char index.
fn search_from(rope: &Rope, pat: &str, cur: usize, forward: bool, n: usize) -> Option<usize> {
    let q = Query::new(pat, SearchOptions { case_sensitive: pat.chars().any(char::is_uppercase), whole_word: false, regex: true, in_range: None }).ok()?;
    let matches = q.find_all(rope, 200_000);
    if matches.is_empty() {
        return None;
    }
    let mut idx = if forward {
        matches.iter().position(|m| m.range.start > cur).unwrap_or(0)
    } else {
        matches.iter().rposition(|m| m.range.start < cur).unwrap_or(matches.len() - 1)
    };
    let step = n.max(1) - 1;
    if forward {
        idx = (idx + step) % matches.len();
    } else {
        // `idx - step` modulo len, without underflow.
        idx = (idx + matches.len() * step - step) % matches.len();
    }
    Some(matches[idx].range.start)
}

/// Text objects: `(a, b, linewise)` or None.
///
/// `obj` is the object key (`w`, `W`, `p`, quotes, brackets, `t`, `s`),
/// `around` selects the `a` form rather than `i`, `n` is the count. The range
/// is `[a, b)` in char indices.
fn text_object(rope: &Rope, cur: usize, obj: char, around: bool, n: usize) -> Option<(usize, usize, bool)> {
    let len = rope.len_chars();
    let cur = cur.min(len);
    match obj {
        'w' | 'W' => {
            let slice = rope.slice(..);
            let class = |c: char| if obj == 'W' { if c.is_whitespace() { CharClass::Whitespace } else { CharClass::Word } } else { motion::char_class(c) };
            if cur >= len {
                return None;
            }
            // Grow over the run of the caret char's class, stopping at line breaks.
            let c0 = class(rope.char(cur));
            let mut a = cur;
            while a > 0 && class(rope.char(a - 1)) == c0 && rope.char(a - 1) != '\n' {
                a -= 1;
            }
            let mut b = cur;
            while b < len && class(rope.char(b)) == c0 && rope.char(b) != '\n' {
                b += 1;
            }
            // Further counts take following words (with their separating whitespace).
            for _ in 1..n {
                let mut e = b;
                while e < len && rope.char(e).is_whitespace() && rope.char(e) != '\n' {
                    e += 1;
                }
                if e < len && rope.char(e) != '\n' {
                    let c = class(rope.char(e));
                    while e < len && class(rope.char(e)) == c && rope.char(e) != '\n' {
                        e += 1;
                    }
                }
                b = e;
            }
            if around {
                // Include trailing whitespace, or leading whitespace when there is none trailing.
                let mut e = b;
                while e < len && rope.char(e) == ' ' || (e < len && rope.char(e) == '\t') {
                    e += 1;
                }
                if e > b {
                    b = e;
                } else {
                    while a > 0 && matches!(rope.char(a - 1), ' ' | '\t') {
                        a -= 1;
                    }
                }
            }
            let _ = slice;
            Some((a, b, false))
        }
        'p' => {
            // A paragraph is a run of non-blank lines, or a run of blank lines
            // if the caret is on one; `ap` adds the following run.
            let line = rope.char_to_line(cur);
            let last = rope.len_lines().saturating_sub(1);
            let blank = line_is_blank(rope, line);
            let mut la = line;
            while la > 0 && line_is_blank(rope, la - 1) == blank {
                la -= 1;
            }
            let mut lb = line;
            while lb < last && line_is_blank(rope, lb + 1) == blank {
                lb += 1;
            }
            for _ in 1..n {
                if lb < last {
                    let nb = line_is_blank(rope, lb + 1);
                    lb += 1;
                    while lb < last && line_is_blank(rope, lb + 1) == nb {
                        lb += 1;
                    }
                }
            }
            if around && lb < last {
                let nb = line_is_blank(rope, lb + 1);
                lb += 1;
                while lb < last && line_is_blank(rope, lb + 1) == nb {
                    lb += 1;
                }
            }
            let (a, b) = line_range(rope, la, lb);
            Some((a, b, true))
        }
        '"' | '\'' | '`' => {
            let line = rope.char_to_line(cur);
            let ls = rope.line_to_char(line);
            let le = line_end_char(rope, line);
            // Quotes are paired left to right on the caret's line; escaped ones are skipped.
            let quotes: Vec<usize> = (ls..le).filter(|&i| rope.char(i) == obj && (i == ls || rope.char(i - 1) != '\\')).collect();
            // Pair containing the cursor, else the first pair after it.
            let mut pair = None;
            for w in quotes.chunks(2) {
                if w.len() == 2 && ((w[0] <= cur && cur <= w[1]) || w[0] > cur) {
                    pair = Some((w[0], w[1]));
                    break;
                }
            }
            let (qa, qb) = pair?;
            if around {
                let mut b = qb + 1;
                while b < le && rope.char(b) == ' ' {
                    b += 1;
                }
                Some((qa, b, false))
            } else {
                Some((qa + 1, qb, false))
            }
        }
        '(' | ')' | 'b' | '{' | '}' | 'B' | '[' | ']' | '<' | '>' => {
            let (open, close) = match obj {
                '(' | ')' | 'b' => ('(', ')'),
                '{' | '}' | 'B' => ('{', '}'),
                '[' | ']' => ('[', ']'),
                _ => ('<', '>'),
            };
            // A count selects the nth enclosing pair outward.
            let mut level = n;
            let mut from = cur;
            let mut found = None;
            while level > 0 {
                let r = enclosing_pair(rope, from, open, close)?;
                found = Some(r);
                from = r.0.checked_sub(1)?;
                level -= 1;
                if level > 0 && r.0 == 0 {
                    return None;
                }
            }
            let (a, b) = found?;
            if around {
                Some((a, b + 1, false))
            } else {
                // Inner: when the brackets are on their own lines, exclude the newline after the open bracket.
                let mut ia = a + 1;
                let ib = b;
                if ia < ib
                    && rope.char(ia) == '\n'
                    && rope.line(rope.char_to_line(ib)).chars().take_while(|c| c.is_whitespace()).count() == ib - rope.line_to_char(rope.char_to_line(ib))
                {
                    ia += 1;
                    let lb_start = rope.line_to_char(rope.char_to_line(ib));
                    return Some((ia, lb_start, true));
                }
                Some((ia, ib, false))
            }
        }
        't' => {
            // Inner/around tag: nearest enclosing <x ...>...</x>.
            // Scans every opening tag in the document and matches its closing
            // tag by depth; works in byte offsets, converted to chars at the end.
            // `best` is (open start, open end, close start, close end); the
            // latest-starting tag containing the caret wins.
            let text = rope.to_string();
            let mut best: Option<(usize, usize, usize, usize)> = None;
            let bytes = text.as_bytes();
            let cur_b = rope.char_to_byte(cur);
            let mut i = 0;
            while let Some(off) = text[i..].find('<') {
                let s = i + off;
                i = s + 1;
                // Skip closing tags, comments and doctypes.
                if bytes.get(s + 1) == Some(&b'/') || bytes.get(s + 1) == Some(&b'!') {
                    continue;
                }
                let Some(end_rel) = text[s..].find('>') else { break };
                let open_end = s + end_rel + 1;
                if text[s..open_end].ends_with("/>") {
                    continue;
                }
                let name: String = text[s + 1..open_end - 1].chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == ':').collect();
                if name.is_empty() {
                    continue;
                }
                let close_tag = format!("</{name}>");
                let mut depth = 1;
                let mut j = open_end;
                let open_tag = format!("<{name}");
                while depth > 0 {
                    let next_close = text[j..].find(&close_tag).map(|x| j + x);
                    // Only count `<name>` / `<name ...>`, not `<name-other>`.
                    let next_open =
                        text[j..].find(&open_tag).map(|x| j + x).filter(|&p| text[p + open_tag.len()..].starts_with(|c: char| c == '>' || c.is_whitespace()));
                    match (next_open, next_close) {
                        (Some(o), Some(c)) if o < c => {
                            depth += 1;
                            j = o + open_tag.len();
                        }
                        (_, Some(c)) => {
                            depth -= 1;
                            j = c + close_tag.len();
                            if depth == 0 && s <= cur_b && cur_b < j {
                                let cand = (s, open_end, c, j);
                                best = match best {
                                    Some(b) if b.0 >= cand.0 => Some(b),
                                    _ => Some(cand),
                                };
                            }
                        }
                        _ => break,
                    }
                }
            }
            let (s, open_end, c, j) = best?;
            let to_char = |b: usize| rope.byte_to_char(b);
            if around { Some((to_char(s), to_char(j), false)) } else { Some((to_char(open_end), to_char(c), false)) }
        }
        's' => {
            // Sentence: from the previous sentence end to the next.
            let text: String = rope.slice(..).to_string();
            let chars: Vec<char> = text.chars().collect();
            let is_end = |i: usize| matches!(chars.get(i), Some('.' | '!' | '?')) && chars.get(i + 1).is_none_or(|c| c.is_whitespace());
            let mut a = cur.min(chars.len());
            while a > 0 && !is_end(a - 1) {
                a -= 1;
            }
            while a < chars.len() && chars[a].is_whitespace() {
                a += 1;
            }
            let mut b = cur.min(chars.len());
            while b < chars.len() && !is_end(b) {
                b += 1;
            }
            if b < chars.len() {
                b += 1;
            }
            if around {
                while b < chars.len() && chars[b].is_whitespace() {
                    b += 1;
                }
            }
            Some((a, b, false))
        }
        _ => None,
    }
}

/// Innermost `open`…`close` pair enclosing `cur` (positions of the brackets).
/// A caret on either bracket selects that bracket's pair.
fn enclosing_pair(rope: &Rope, cur: usize, open: char, close: char) -> Option<(usize, usize)> {
    let n = rope.len_chars();
    let cur = cur.min(n);
    if cur < n && rope.char(cur) == open {
        return apex_edit::brackets::matching_bracket(rope, cur);
    }
    if cur < n && rope.char(cur) == close {
        return apex_edit::brackets::matching_bracket(rope, cur);
    }
    // Walk backwards, skipping closed pairs, to the first unmatched opener.
    let mut depth = 0usize;
    let mut i = cur;
    while i > 0 {
        i -= 1;
        let c = rope.char(i);
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return apex_edit::brackets::matching_bracket(rope, i);
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory [`VimHost`] that records commands and status messages.
    struct Host {
        doc: Document,
        sels: SelectionSet,
        /// Fake system clipboard.
        clipboard: Option<String>,
        /// Every command sent, in order.
        commands: Vec<(Command, Option<String>)>,
        /// Every status message shown, in order.
        messages: Vec<String>,
    }

    impl Host {
        /// A host over `text` with the caret at 0.
        fn new(text: &str) -> Self {
            Host {
                doc: Document::from_text(text),
                sels: SelectionSet::single(Selection::point(0)),
                clipboard: None,
                commands: Vec::new(),
                messages: Vec::new(),
            }
        }
        /// Full document text.
        fn text(&self) -> String {
            self.doc.text()
        }
        /// Primary caret head (char index).
        fn cursor(&self) -> usize {
            self.sels.primary().head
        }
    }

    impl VimHost for Host {
        /// The test document.
        fn doc(&self) -> &Document {
            &self.doc
        }
        /// Current selections.
        fn sels(&self) -> SelectionSet {
            self.sels.clone()
        }
        /// Replace selections.
        fn set_sels(&mut self, sels: SelectionSet) {
            self.sels = sels;
        }
        /// Apply the edit to the document; an empty transaction only updates selections.
        fn edit(&mut self, f: &mut dyn FnMut(&Document, &SelectionSet) -> Option<EditResult>) -> bool {
            let Some(r) = f(&self.doc, &self.sels) else { return false };
            let before = self.sels.clone();
            if r.tx.is_empty() {
                self.sels = r.sels;
                return true;
            }
            if self.doc.apply(r.tx, &before, &r.sels, r.kind) {
                self.sels = r.sels;
                true
            } else {
                false
            }
        }
        /// Undo/redo are applied to the document; every command is recorded.
        fn command(&mut self, cmd: Command, arg: Option<String>) {
            match cmd {
                Command::Undo => {
                    if let Some(s) = self.doc.undo() {
                        self.sels = s;
                    }
                }
                Command::Redo => {
                    if let Some(s) = self.doc.redo() {
                        self.sels = s;
                    }
                }
                _ => {}
            }
            self.commands.push((cmd, arg));
        }
        /// Record the message.
        fn status(&mut self, text: String, _error: bool) {
            self.messages.push(text);
        }
        /// Read the fake clipboard.
        fn clipboard_get(&mut self) -> Option<String> {
            self.clipboard.clone()
        }
        /// Write the fake clipboard.
        fn clipboard_set(&mut self, text: String) {
            self.clipboard = Some(text);
        }
        /// Close the current undo group in the document history.
        fn undo_boundary(&mut self) {
            self.doc.history_mut().mark_boundary();
        }
    }

    /// Environment used by every test.
    const ENV: Env = Env { tab_width: 4, page_lines: 10 };

    /// Feed a key string; `<Esc>`, `<CR>`, `<BS>`, `<lt>`, `<C-x>` are recognised.
    fn feed(vim: &mut Vim, host: &mut Host, keys: &str) {
        let mut chars = keys.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '<' {
                let mut name = String::new();
                for d in chars.by_ref() {
                    if d == '>' {
                        break;
                    }
                    name.push(d);
                }
                let key = match name.as_str() {
                    "Esc" => Key::Esc,
                    "CR" => Key::Enter,
                    "BS" => Key::Backspace,
                    "lt" => Key::Char('<'),
                    s if s.starts_with("C-") => Key::Ctrl(s.chars().nth(2).unwrap()),
                    other => panic!("unknown key {other}"),
                };
                if key == Key::Char('<') {
                    vim_char(vim, host, '<');
                } else {
                    vim.key(key, &ENV, host);
                }
            } else {
                vim_char(vim, host, c);
            }
        }
    }

    /// Send one printable char, inserting it directly when the engine passes it through.
    fn vim_char(vim: &mut Vim, host: &mut Host, c: char) {
        if vim.mode == Mode::Insert {
            // Insert mode: text goes straight to the document, as the editor would do.
            if !vim.key(Key::Char(c), &ENV, host) {
                let t = c.to_string();
                host.edit(&mut |d, s| ops::insert_text(d, s, &t, InsertOptions { auto_pair: false, auto_indent: false, overwrite: false }));
            }
        } else {
            vim.key(Key::Char(c), &ENV, host);
        }
    }

    /// Run `keys` on a fresh engine over `text`; returns (final text, caret, engine, host).
    fn run(text: &str, keys: &str) -> (String, usize, Vim, Host) {
        let mut host = Host::new(text);
        let mut vim = Vim::new();
        feed(&mut vim, &mut host, keys);
        let t = host.text();
        let c = host.cursor();
        (t, c, vim, host)
    }

    /// Character, word, line, document, find and bracket motions.
    #[test]
    fn basic_motions() {
        let (_, c, ..) = run("hello world\nsecond line\n", "ll");
        assert_eq!(c, 2);
        let (_, c, ..) = run("hello world\nsecond line\n", "w");
        assert_eq!(c, 6);
        let (_, c, ..) = run("hello world\nsecond line\n", "$");
        assert_eq!(c, 10);
        let (_, c, ..) = run("hello world\nsecond line\n", "jk");
        assert_eq!(c, 0);
        let (_, c, ..) = run("hello world\nsecond line\n", "j$");
        assert_eq!(c, 22);
        let (_, c, ..) = run("hello world\nsecond line\n", "G");
        assert_eq!(c, 24); // empty last line
        let (_, c, ..) = run("hello world\nsecond line\nthird\n", "2G");
        assert_eq!(c, 12);
        let (_, c, ..) = run("hello world\nsecond line\nthird\n", "3Gk");
        assert_eq!(c, 12);
        let (_, c, ..) = run("hello world\n", "3l0");
        assert_eq!(c, 0);
        let (_, c, ..) = run("  hello\n", "$^");
        assert_eq!(c, 2);
        let (_, c, ..) = run("a.b c\n", "e");
        assert_eq!(c, 1);
        let (_, c, ..) = run("abc def\n", "e");
        assert_eq!(c, 2);
        let (_, c, ..) = run("abc def\n", "ee");
        assert_eq!(c, 6);
        let (_, c, ..) = run("abc def\n", "$b");
        assert_eq!(c, 4);
        let (_, c, ..) = run("foo(bar)\n", "f(%");
        assert_eq!(c, 7);
        let (_, c, ..) = run("one two three\n", "ft;");
        assert_eq!(c, 8);
        let (_, c, ..) = run("one two three\n", "tt");
        assert_eq!(c, 3);
    }

    /// Normal mode keeps the caret off the newline.
    #[test]
    fn cursor_cannot_rest_on_newline_in_normal_mode() {
        let (_, c, ..) = run("ab\ncd\n", "lll");
        assert_eq!(c, 1);
        let (_, c, ..) = run("ab\ncd\n", "$");
        assert_eq!(c, 1);
        let (t, c, ..) = run("ab\ncd\n", "A!<Esc>");
        assert_eq!(t, "ab!\ncd\n");
        assert_eq!(c, 2);
    }

    /// Operators with motions and text objects, and pasting the result.
    #[test]
    fn delete_change_yank_put() {
        let (t, c, ..) = run("hello world\n", "dw");
        assert_eq!(t, "world\n");
        assert_eq!(c, 0);
        let (t, ..) = run("hello world\n", "x");
        assert_eq!(t, "ello world\n");
        let (t, ..) = run("hello world\n", "3x");
        assert_eq!(t, "lo world\n");
        let (t, ..) = run("one\ntwo\nthree\n", "dd");
        assert_eq!(t, "two\nthree\n");
        let (t, ..) = run("one\ntwo\nthree\n", "2dd");
        assert_eq!(t, "three\n");
        let (t, ..) = run("one\ntwo\nthree\n", "d2j");
        assert_eq!(t, "");
        let (t, ..) = run("one\ntwo\nthree\n", "jdG");
        assert_eq!(t, "one\n");
        let (t, ..) = run("hello world\n", "cwbye<Esc>");
        assert_eq!(t, "bye world\n");
        let (t, ..) = run("hello world\n", "wD");
        assert_eq!(t, "hello \n");
        let (t, ..) = run("hello world\n", "wC!<Esc>");
        assert_eq!(t, "hello !\n");
        let (t, ..) = run("  indented\n", "ccnew<Esc>");
        assert_eq!(t, "  new\n");
        let (t, c, ..) = run("one\ntwo\n", "yyp");
        assert_eq!(t, "one\none\ntwo\n");
        assert_eq!(c, 4);
        let (t, ..) = run("one\ntwo\n", "jyyP");
        assert_eq!(t, "one\ntwo\ntwo\n");
        let (t, c, ..) = run("abc\n", "ylp");
        assert_eq!(t, "aabc\n");
        assert_eq!(c, 1);
        let (t, ..) = run("abc\n", "xp");
        assert_eq!(t, "bac\n");
        let (t, ..) = run("one two\n", "dwP");
        assert_eq!(t, "one two\n");
        let (t, ..) = run("a b c\n", "d$");
        assert_eq!(t, "\n");
        let (t, ..) = run("foo(bar baz)\n", "f(di(");
        assert_eq!(t, "foo()\n");
        let (t, ..) = run("foo(bar baz)\n", "fbda(");
        assert_eq!(t, "foo\n");
        let (t, ..) = run("say \"hi there\" now\n", "fhci\"yo<Esc>");
        assert_eq!(t, "say \"yo\" now\n");
        let (t, ..) = run("one two three\n", "wdaw");
        assert_eq!(t, "one three\n");
        let (t, ..) = run("one two three\n", "wdiw");
        assert_eq!(t, "one  three\n");
        let (t, ..) = run("p1\np1b\n\np2\n", "dap");
        assert_eq!(t, "p2\n");
        let (t, ..) = run("<b>bold</b>\n", "fodit");
        assert_eq!(t, "<b></b>\n");
        let (t, ..) = run("if x {\n    body\n}\n", "jdi{");
        assert_eq!(t, "if x {\n}\n");
    }

    /// Counted operators, appending registers and the black-hole register.
    #[test]
    fn counts_and_registers() {
        let (t, ..) = run("a b c d e\n", "3dw");
        assert_eq!(t, "d e\n");
        let (t, ..) = run("one\ntwo\n", "\"ayyj\"byy\"ap\"bp");
        assert_eq!(t, "one\ntwo\none\ntwo\n");
        let (t, ..) = run("one\ntwo\n", "\"Ayyj\"Ayy\"ap");
        assert_eq!(t, "one\ntwo\none\ntwo\n");
        let (_, _, vim, host) = run("abc\n", "\"+yy");
        assert_eq!(host.clipboard.as_deref(), Some("abc\n"));
        assert_eq!(vim.registers[&'0'].text, "abc\n");
        let (t, ..) = run("x\n", "\"_ddp");
        assert_eq!(t, "");
    }

    /// Characterwise and linewise visual selection and operators on them.
    #[test]
    fn visual_modes() {
        let (t, ..) = run("hello world\n", "vlld");
        assert_eq!(t, "lo world\n");
        let (t, ..) = run("hello world\n", "veed");
        assert_eq!(t, "\n");
        let (t, ..) = run("one\ntwo\nthree\n", "Vjd");
        assert_eq!(t, "three\n");
        let (t, ..) = run("one\ntwo\nthree\n", "jVkd");
        assert_eq!(t, "three\n");
        let (t, ..) = run("hello\n", "vllU");
        assert_eq!(t, "HELlo\n");
        let (t, ..) = run("hello world\n", "wviwcbye<Esc>");
        assert_eq!(t, "hello bye\n");
        let (t, ..) = run("hello world\n", "vey$p");
        assert_eq!(t, "hello worldhello\n");
        let (t, ..) = run("one\ntwo\n", "Vyjp");
        assert_eq!(t, "one\ntwo\none\n");
        let (t, ..) = run("a\nb\n", "VjJ");
        assert_eq!(t, "a b\n");
        let (t, ..) = run("abc\n", "lvo");
        assert_eq!(t, "abc\n");
        let (t, ..) = run("one\ntwo\n", "Vj>");
        assert_eq!(t, "    one\n    two\n");
        let (t, ..) = run("  a\n  b\n", "Vj<lt>");
        assert_eq!(t, "a\nb\n");
        let (_, _, vim, _) = run("abc\n", "v<Esc>");
        assert_eq!(vim.mode, Mode::Normal);
        let (t, ..) = run("abc def\n", "vwrx");
        assert_eq!(t, "xxxxxef\n");
    }

    /// Insert-entry commands plus `r`, `~`, `gu` and `s`.
    #[test]
    fn insert_variants_and_repeat() {
        let (t, c, ..) = run("abc\n", "ix<Esc>");
        assert_eq!(t, "xabc\n");
        assert_eq!(c, 0);
        let (t, ..) = run("abc\n", "ax<Esc>");
        assert_eq!(t, "axbc\n");
        let (t, ..) = run("abc\n", "Ax<Esc>");
        assert_eq!(t, "abcx\n");
        let (t, ..) = run("  abc\n", "Ix<Esc>");
        assert_eq!(t, "  xabc\n");
        let (t, ..) = run("abc\n", "onew<Esc>");
        assert_eq!(t, "abc\nnew\n");
        let (t, ..) = run("abc\n", "Onew<Esc>");
        assert_eq!(t, "new\nabc\n");
        let (t, ..) = run("abc\n", "3ihi<Esc>");
        assert_eq!(t, "hihihiabc\n");
        let (t, ..) = run("abc\n", "rx");
        assert_eq!(t, "xbc\n");
        let (t, ..) = run("abc\n", "2rx");
        assert_eq!(t, "xxc\n");
        let (t, ..) = run("abc\n", "~~");
        assert_eq!(t, "ABc\n");
        let (t, ..) = run("abc def\n", "gUw");
        assert_eq!(t, "ABC def\n");
        let (t, ..) = run("ABC\n", "guu");
        assert_eq!(t, "abc\n");
        let (t, ..) = run("a\nb\n", "J");
        assert_eq!(t, "a b\n");
        let (t, ..) = run("abc\n", "sX<Esc>");
        assert_eq!(t, "Xbc\n");
        let (t, ..) = run("  abc\n", "SX<Esc>");
        assert_eq!(t, "  X\n");
    }

    /// `.` repeats the last change, including an insert session.
    #[test]
    fn dot_repeat() {
        let (t, ..) = run("a b c d\n", "dw..");
        assert_eq!(t, "d\n");
        let (t, ..) = run("one two three\n", "cwX<Esc>w.");
        assert_eq!(t, "X X three\n");
        let (t, ..) = run("abc\n", "ix<Esc>..");
        assert_eq!(t, "xxxabc\n");
        let (t, ..) = run("a b c d e f\n", "dw3.");
        assert_eq!(t, "e f\n");
        let (t, ..) = run("one\ntwo\nthree\nfour\n", "ddj.");
        assert_eq!(t, "two\nfour\n");
        let (t, ..) = run("abc abc abc\n", "xw.w.");
        assert_eq!(t, "bc bc bc\n");
    }

    /// `u` goes through the host; an insert session undoes as one step.
    #[test]
    fn undo_redo_through_host() {
        let (t, _, _, host) = run("abc\n", "xu");
        assert_eq!(t, "abc\n");
        assert!(host.commands.iter().any(|(c, _)| *c == Command::Undo));
        let (t, ..) = run("abc\n", "xu<C-r>");
        assert_eq!(t, "bc\n");
        // An insert session undoes as one unit.
        let (t, ..) = run("abc\n", "ihello<Esc>u");
        assert_eq!(t, "abc\n");
    }

    /// `/`, `?`, `n` and a search as an operator target.
    #[test]
    fn search() {
        let (_, c, ..) = run("foo bar foo baz\n", "/foo<CR>");
        assert_eq!(c, 8);
        let (_, c, ..) = run("foo bar foo baz\n", "/foo<CR>n");
        assert_eq!(c, 0); // wrapped
        let (_, c, ..) = run("foo bar foo baz\n", "/foo<CR>N");
        assert_eq!(c, 0);
        let (_, c, ..) = run("foo bar foo baz\n", "?baz<CR>");
        assert_eq!(c, 12);
        let (_, c, ..) = run("foo bar foo baz\n", "*");
        assert_eq!(c, 8);
        let (t, ..) = run("foo bar foo baz\n", "d/bar<CR>");
        assert_eq!(t, "bar foo baz\n");
        let (_, _, _, host) = run("abc\n", "/zzz<CR>");
        assert!(host.messages.iter().any(|m| m.contains("not found")));
    }

    /// `:` commands map to host commands; `:s` substitutes; unknown ones report an error.
    #[test]
    fn ex_commands() {
        let (_, _, _, host) = run("abc\n", ":w<CR>");
        assert_eq!(host.commands[0].0, Command::Save);
        let (_, _, _, host) = run("abc\n", ":wq<CR>");
        assert_eq!(host.commands.iter().map(|c| c.0).collect::<Vec<_>>(), vec![Command::Save, Command::CloseTab]);
        let (_, _, _, host) = run("abc\n", ":q!<CR>");
        assert_eq!(host.commands[0], (Command::CloseTab, Some("force".into())));
        let (_, c, ..) = run("a\nb\nc\n", ":3<CR>");
        assert_eq!(c, 4);
        let (t, ..) = run("foo foo\nfoo\n", ":s/foo/bar/<CR>");
        assert_eq!(t, "bar foo\nfoo\n");
        let (t, ..) = run("foo foo\nfoo\n", ":%s/foo/bar/g<CR>");
        assert_eq!(t, "bar bar\nbar\n");
        let (t, ..) = run("foo foo\nfoo\n", "Vj:s/foo/bar/<CR>");
        assert_eq!(t, "bar foo\nbar\n");
        let (t, ..) = run("a/b\n", ":s/\\//-/<CR>");
        assert_eq!(t, "a-b\n");
        let (_, _, _, host) = run("abc\n", ":bogus<CR>");
        assert!(host.messages[0].contains("Not an editor command"));
        let (_, _, vim, _) = run("abc\n", ":w<Esc>");
        assert_eq!(vim.mode, Mode::Normal);
    }

    /// Marks, paragraph motion, `|` and cancelling a pending operator.
    #[test]
    fn marks_and_misc() {
        let (_, _, _, host) = run("abc\ndef\n", "jmaggk'a");
        // Marks go through the host command (the app stores them on the document).
        assert!(host.commands.iter().any(|(c, a)| *c == Command::SetMark && a.as_deref() == Some("vim:a")));
        let (_, c, ..) = run("a\n\nb\nc\n\nd\n", "}");
        assert_eq!(c, 2);
        let (_, c, ..) = run("a\n\nb\nc\n\nd\n", "}}");
        assert_eq!(c, 7);
        let (_, c, ..) = run("abc\n", "ll3|");
        assert_eq!(c, 2);
        let (_, c, ..) = run("a\tb\n", "$|");
        assert_eq!(c, 0);
        let (_, _, vim, _) = run("abc\n", "d");
        assert_eq!(vim.pending_text(), "d");
        let (_, _, vim, _) = run("abc\n", "d<Esc>");
        assert_eq!(vim.pending_text(), "");
    }

    /// Secondary carets follow the primary caret's motion.
    #[test]
    fn multiple_cursors_follow_motions() {
        let mut host = Host::new("ab\ncd\n");
        host.sels = SelectionSet::from_vec(vec![Selection::point(0), Selection::point(3)], 0);
        let mut vim = Vim::new();
        feed(&mut vim, &mut host, "l");
        assert_eq!(host.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![1, 4]);
    }
}
