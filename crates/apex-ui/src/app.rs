//! The application: documents, views, panes, menus, dialogs and command execution.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use apex_buffer::{Encoding, LineEnding};
use apex_edit::ops::{self, CaseOp, EditResult, InsertOptions};
use apex_edit::search::{Query, SearchOptions};
use apex_edit::{DocId, Document, EditKind, Selection, SelectionSet};
use apex_project::fileio::{self, FileError, SaveOptions};
use egui::{Color32, Rect, Ui, pos2, vec2};

use crate::command::Command;
use crate::editor::{self, Action, EditorStyle};
use crate::keymap::Keymap;
use crate::settings::{Settings, SettingsIssue};
use crate::theme::Theme;
use crate::view::{Motion, View, ViewId};
use crate::workspace::{Axis, Layout, Pane, PaneId};
use apex_syntax::SyntaxState;

/// Options passed from the command line.
#[derive(Clone, Default)]
pub struct StartupArgs {
    pub files: Vec<(PathBuf, Option<usize>, Option<usize>)>,
    pub folders: Vec<PathBuf>,
    /// Files and folders arriving after start-up (Finder "Open With", a second
    /// `apex` invocation). The sender must call `Context::request_repaint`.
    pub inbox: Option<crossbeam_channel::Receiver<OpenRequest>>,
    /// Called when the last view of a file closes (used for `apex --wait`).
    pub on_view_closed: Option<ViewClosedHook>,
    /// `apex --diff old new` at start-up.
    pub diff: Option<(PathBuf, PathBuf)>,
}

impl std::fmt::Debug for StartupArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupArgs").field("files", &self.files).field("folders", &self.folders).field("diff", &self.diff).finish_non_exhaustive()
    }
}

/// Callback for `apex --wait`: the last view of this path was closed.
pub type ViewClosedHook = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;

/// A request to open paths in the running editor.
#[derive(Debug, Clone, Default)]
pub struct OpenRequest {
    pub files: Vec<(PathBuf, Option<usize>, Option<usize>)>,
    pub folders: Vec<PathBuf>,
    /// Show a unified diff of two files.
    pub diff: Option<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone)]
pub(crate) enum Dialog {
    /// Ask about unsaved changes before running `then`.
    ConfirmClose {
        views: Vec<ViewId>,
        then: AfterClose,
        index: usize,
    },
    /// File changed on disk while we have edits.
    ExternalChange {
        doc: DocId,
    },
    /// Save was refused because the file changed since it was loaded.
    SaveConflict {
        view: ViewId,
    },
    GotoLine {
        text: String,
    },
    About,
    Issues(Vec<SettingsIssue>),
    ConfirmDelete(PathBuf),
    ConfirmReplaceInFiles,
    Info {
        title: String,
        body: String,
    },
    ConfirmRevertHunk {
        view: ViewId,
        hunk_start: usize,
        hunk_end: usize,
        old_start: usize,
        old_end: usize,
    },
    Trust {
        root: PathBuf,
        then: Option<(Command, Option<String>)>,
    },
    ConfirmWorkspaceEdit {
        label: String,
        edit: Box<apex_project::lsp::WorkspaceEdit>,
    },
    Prompt {
        title: String,
        text: String,
        kind: crate::workspace_ui::PromptKind,
    },
    Recovery(Vec<apex_project::session::RecoveryEntry>),
    /// Confirm a destructive command, then run `then` again (it checks
    /// `git.ops.confirmed`).
    Confirm {
        title: String,
        body: String,
        button: String,
        then: (Command, Option<String>),
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterClose {
    CloseViews,
    Quit,
}

#[derive(Debug, Default)]
pub(crate) struct FindState {
    open: bool,
    replace_open: bool,
    needle: String,
    replacement: String,
    options: SearchOptions,
    matches: Vec<std::ops::Range<usize>>,
    current: Option<usize>,
    error: Option<String>,
    /// Document revision the matches were computed for.
    rev: Option<(DocId, u64, String, SearchOptions)>,
    focus_needle: bool,
    history: VecDeque<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ClosedTab {
    path: PathBuf,
    sels: SelectionSet,
    scroll: egui::Vec2,
}

pub struct ApexApp {
    pub settings: Settings,
    pub theme: Theme,
    pub(crate) themes: Vec<Theme>,
    pub keymap: Keymap,
    pub(crate) docs: HashMap<DocId, Document>,
    pub(crate) views: HashMap<ViewId, View>,
    pub(crate) panes: HashMap<PaneId, Pane>,
    pub(crate) layout: Layout,
    pub(crate) active_pane: PaneId,
    pub(crate) next_pane_id: u64,
    pub(crate) recent_files: VecDeque<PathBuf>,
    pub(crate) closed_tabs: Vec<ClosedTab>,
    pub(crate) status: Option<(String, Instant, bool)>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) queue: VecDeque<(Command, Option<String>)>,
    pub(crate) untitled_counter: u32,
    pub(crate) clipboard_history: VecDeque<String>,
    /// Text last copied via a whole-line copy (no selection), for line-paste.
    pub(crate) line_clipboard: Option<String>,
    pub(crate) pending_paste_mode: Option<String>,
    pub(crate) find: FindState,
    pub(crate) font_zoom: i32,
    pub(crate) quit_confirmed: bool,
    pub(crate) settings_issues: Vec<SettingsIssue>,
    pub(crate) fonts_applied: bool,
    pub(crate) config_dir: PathBuf,
    pub(crate) context_menu_at: Option<(ViewId, egui::Pos2)>,
    pub(crate) last_focus_check: Instant,
    pub fullscreen: bool,
    pub(crate) distraction_free: bool,
    pub(crate) syntax: HashMap<DocId, SyntaxState>,
    pub(crate) pending_repaint: bool,
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) project_file: Option<PathBuf>,
    pub(crate) sidebar: crate::sidebar::Sidebar,
    pub(crate) index: apex_project::index::FileIndex,
    pub(crate) index_scan: Option<apex_project::index::IndexScan>,
    pub(crate) watcher: Option<apex_project::watch::FsWatcher>,
    pub(crate) rescan_at: Option<Instant>,
    pub(crate) palette: crate::palette::Palette,
    pub(crate) palette_preview: Option<ViewId>,
    pub(crate) fif: crate::find_in_files::FindInFiles,
    pub(crate) panel_open: bool,
    pub(crate) recent_commands: VecDeque<Command>,
    pub(crate) recent_projects: VecDeque<PathBuf>,
    pub(crate) jumps: Vec<(DocId, SelectionSet)>,
    pub(crate) jump_pos: usize,
    pub(crate) recovery_ids: HashMap<DocId, (String, u64)>,
    pub(crate) last_recovery: Instant,
    pub(crate) last_session_save: Instant,
    pub(crate) macro_recording: Option<Vec<crate::workspace_ui::MacroStep>>,
    pub(crate) last_macro: Vec<crate::workspace_ui::MacroStep>,
    pub(crate) macros: std::collections::BTreeMap<String, Vec<crate::workspace_ui::MacroStep>>,
    pub(crate) symbols: crate::symbol_index::ProjectSymbols,
    pub(crate) symbol_scan: Option<crate::symbol_index::SymbolScan>,
    pub(crate) dragging_tab: Option<(ViewId, PaneId)>,
    pub(crate) fold_cache: HashMap<DocId, (u64, Vec<(usize, usize)>)>,
    pub(crate) hover_pos: Option<(ViewId, usize)>,
    pub(crate) git: crate::git_ui::GitUi,
    pub(crate) tasks: crate::tasks_ui::TasksUi,
    pub(crate) trust: crate::tasks_ui::TrustStore,
    pub(crate) panel_tab: crate::tasks_ui::PanelTab,
    pub(crate) last_git_tick: Instant,
    pub(crate) lsp: crate::lsp_ui::LspUi,
    pub(crate) last_pointer: Option<egui::Pos2>,
    pub(crate) snippets: Vec<apex_edit::snippet::SnippetDef>,
    pub(crate) snippet_session: Option<crate::snippets_ui::SnippetSession>,
    pub(crate) spell: crate::spell_ui::SpellUi,
    pub(crate) vim: crate::vim::Vim,
    pub(crate) settings_panel: crate::settings_ui::SettingsPanel,
    /// The native file dialog in progress, if any (see `file_dialogs.rs`).
    pub(crate) file_dialog: Option<crate::file_dialogs::PendingFileDialog>,
    pub(crate) inbox: Option<crossbeam_channel::Receiver<OpenRequest>>,
    pub(crate) on_view_closed: Option<ViewClosedHook>,
    /// `APEX_FRAME_TIME=1`: show the frame interval in the status bar.
    pub(crate) show_frame_time: bool,
}

const MAX_RECENT: usize = 30;

impl ApexApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: StartupArgs) -> Self {
        let config_dir = apex_project::paths::config_dir();
        let (settings, issues) = Settings::load_file(&Settings::default(), &config_dir.join("settings.toml"));
        let themes = Theme::builtins();
        let theme = Self::pick_theme(&themes, &settings.theme, &config_dir);
        let mut keymap = Keymap::defaults();
        let user_keymap = config_dir.join("keymap.toml");
        if let Ok(src) = std::fs::read_to_string(&user_keymap) {
            let (km, errors) = Keymap::parse(&src);
            for e in errors {
                tracing::warn!("user keymap: {e}");
            }
            keymap.merge(km);
        }
        theme.apply_to_egui(&cc.egui_ctx);
        let pane_id = PaneId(1);
        let mut app = Self {
            settings,
            theme,
            themes,
            keymap,
            docs: HashMap::new(),
            views: HashMap::new(),
            panes: HashMap::from([(pane_id, Pane::default())]),
            layout: Layout::Leaf(pane_id),
            active_pane: pane_id,
            next_pane_id: 2,
            recent_files: VecDeque::new(),
            closed_tabs: Vec::new(),
            status: None,
            dialog: None,
            queue: VecDeque::new(),
            untitled_counter: 0,
            clipboard_history: VecDeque::new(),
            line_clipboard: None,
            pending_paste_mode: None,
            find: FindState::default(),
            font_zoom: 0,
            quit_confirmed: false,
            settings_issues: issues,
            fonts_applied: false,
            config_dir,
            context_menu_at: None,
            last_focus_check: Instant::now(),
            fullscreen: false,
            distraction_free: false,
            syntax: HashMap::new(),
            pending_repaint: false,
            roots: Vec::new(),
            project_file: None,
            sidebar: crate::sidebar::Sidebar::new(Default::default()),
            index: Default::default(),
            index_scan: None,
            watcher: None,
            rescan_at: None,
            palette: Default::default(),
            palette_preview: None,
            fif: Default::default(),
            panel_open: false,
            recent_commands: VecDeque::new(),
            recent_projects: VecDeque::new(),
            jumps: Vec::new(),
            jump_pos: 0,
            recovery_ids: HashMap::new(),
            last_recovery: Instant::now(),
            last_session_save: Instant::now(),
            macro_recording: None,
            last_macro: Vec::new(),
            macros: Default::default(),
            symbols: Default::default(),
            symbol_scan: None,
            dragging_tab: None,
            fold_cache: HashMap::new(),
            hover_pos: None,
            git: Default::default(),
            tasks: Default::default(),
            trust: Default::default(),
            panel_tab: Default::default(),
            last_git_tick: Instant::now(),
            lsp: Default::default(),
            last_pointer: None,
            snippets: Vec::new(),
            snippet_session: None,
            spell: Default::default(),
            vim: crate::vim::Vim::new(),
            settings_panel: Default::default(),
            file_dialog: None,
            inbox: args.inbox.clone(),
            on_view_closed: args.on_view_closed.clone(),
            show_frame_time: std::env::var_os("APEX_FRAME_TIME").is_some(),
        };
        app.load_recent();
        app.load_macros();
        app.load_snippets();
        if app.settings.restore_session
            && let Some(session) = apex_project::session::Session::load()
        {
            app.restore_session(session);
        }
        app.open_request(&cc.egui_ctx, &OpenRequest { files: args.files.clone(), folders: args.folders.clone(), diff: args.diff.clone() });
        if app.views.is_empty() {
            app.new_untitled();
        }
        if !app.settings_issues.is_empty() {
            app.dialog = Some(Dialog::Issues(app.settings_issues.clone()));
        } else {
            app.startup_recovery_check();
        }
        app
    }

    /// `theme_mode = "auto"`: switch between the dark and light variant of the
    /// chosen theme family when the system preference changes.
    pub(crate) fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if self.settings.theme_mode != "auto" {
            return;
        }
        let Some(sys) = ctx.system_theme() else { return };
        let want_dark = sys == egui::Theme::Dark;
        if self.theme.dark == want_dark {
            return;
        }
        // Prefer the counterpart in the same family ("Solar Dark" ↔ "Solar Light"), else the defaults.
        let family = self.theme.name.trim_end_matches(" Dark").trim_end_matches(" Light").to_string();
        let counterpart = self
            .themes
            .iter()
            .find(|t| t.dark == want_dark && t.name.starts_with(&family))
            .or_else(|| self.themes.iter().find(|t| t.name == if want_dark { "Apex Dark" } else { "Apex Light" }));
        if let Some(t) = counterpart.cloned() {
            self.theme = t;
            self.theme.apply_to_egui(ctx);
            self.invalidate_all_layouts();
        }
    }

    pub(crate) fn pick_theme(themes: &[Theme], name: &str, config_dir: &Path) -> Theme {
        // User themes in <config>/themes/*.toml may add or override.
        let mut all: Vec<Theme> = themes.to_vec();
        if let Ok(rd) = std::fs::read_dir(config_dir.join("themes")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "toml") {
                    match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Theme::parse(&s).map_err(|e| e.to_string())) {
                        Ok(t) => {
                            all.retain(|x| x.name != t.name);
                            all.push(t);
                        }
                        Err(e) => tracing::warn!("theme {}: {e}", p.display()),
                    }
                }
            }
        }
        all.iter().find(|t| t.name.eq_ignore_ascii_case(name)).cloned().unwrap_or_else(|| {
            tracing::warn!("theme {name:?} not found; using Apex Dark");
            Theme::default_dark()
        })
    }

    // ----- documents and views ------------------------------------------

    pub(crate) fn active_view_id(&self) -> Option<ViewId> {
        self.panes.get(&self.active_pane)?.active_view()
    }

    pub(crate) fn active_ids(&self) -> Option<(ViewId, DocId)> {
        let v = self.active_view_id()?;
        Some((v, self.views.get(&v)?.doc))
    }

    /// Open the folders and files of a request; files with a line jump there.
    pub(crate) fn open_request(&mut self, ctx: &egui::Context, req: &OpenRequest) {
        for folder in &req.folders {
            self.add_root(folder);
        }
        for (path, line, col) in &req.files {
            if let Some(v) = self.open_path(path)
                && let Some(l) = line
            {
                self.goto_line_col(v, *l, col.unwrap_or(1));
            }
        }
        if let Some((old, new)) = &req.diff {
            self.open_diff(ctx, old, new);
        }
    }

    /// Open a read-only unified diff of two files (`apex --diff old new`).
    pub(crate) fn open_diff(&mut self, ctx: &egui::Context, old: &Path, new: &Path) {
        let fallback = apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252);
        let load = |p: &Path| apex_project::fileio::load(p, None, fallback).map(|l| l.text);
        let (a, b) = match (load(old), load(new)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) => {
                self.set_status(format!("Could not read {}: {e}", old.display()), true);
                return;
            }
            (_, Err(e)) => {
                self.set_status(format!("Could not read {}: {e}", new.display()), true);
                return;
            }
        };
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
        if a == b {
            self.set_status(format!("{} and {} are identical", name(old), name(new)), false);
            return;
        }
        let diff = apex_project::git::unified_diff(&a, &b, &old.display().to_string(), &new.display().to_string());
        self.open_scratch_doc(&format!("{} vs {}.diff", name(old), name(new)), &diff, Some("diff"), true, ctx);
    }

    /// Requests queued by the platform or a second invocation since the last frame.
    pub(crate) fn poll_inbox(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.inbox.clone() else { return };
        let mut any = false;
        while let Ok(req) = rx.try_recv() {
            self.open_request(ctx, &req);
            any = true;
        }
        if any {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_active_editor(ctx);
        }
    }

    pub(crate) fn new_untitled(&mut self) -> ViewId {
        self.untitled_counter += 1;
        let mut doc = Document::empty();
        doc.untitled_index = Some(self.untitled_counter);
        doc.indent = self.settings.indent();
        doc.line_ending = match self.settings.default_line_ending.as_str() {
            "lf" => LineEnding::Lf,
            "crlf" => LineEnding::CrLf,
            "cr" => LineEnding::Cr,
            _ => LineEnding::native(),
        };
        doc.encoding = Encoding::from_label(&self.settings.default_encoding).unwrap_or_default();
        self.add_document(doc)
    }

    pub(crate) fn add_document(&mut self, doc: Document) -> ViewId {
        let id = doc.id;
        let rev = doc.revision();
        self.docs.insert(id, doc);
        let view = View::new(id, rev);
        let vid = view.id;
        self.views.insert(vid, view);
        let pane = self.panes.entry(self.active_pane).or_default();
        pane.insert_after_active(vid);
        vid
    }

    /// Open (or focus) a file. Returns the view id.
    pub fn open_path(&mut self, path: &Path) -> Option<ViewId> {
        let path = normalise_path(path);
        if path.is_dir() {
            self.add_root(&path);
            self.settings.sidebar = true;
            return None;
        }
        // Already open?
        if let Some((vid, _)) = self.views.iter().find(|(_, v)| self.docs.get(&v.doc).and_then(|d| d.path.as_ref()) == Some(&path)) {
            let vid = *vid;
            self.focus_view(vid);
            return Some(vid);
        }
        let fallback = Encoding::from_label(&self.settings.fallback_encoding).unwrap_or(Encoding::Legacy(encoding_rs::WINDOWS_1252));
        let loaded = match fileio::load(&path, None, fallback) {
            Ok(l) => l,
            Err(FileError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                // Sublime behaviour: a new buffer that will be created on save.
                let mut doc = Document::empty();
                doc.path = Some(path.clone());
                doc.indent = self.settings.indent();
                let v = self.add_document(doc);
                self.set_status(format!("{} does not exist yet; it will be created on save", path.display()), false);
                self.push_recent(&path);
                return Some(v);
            }
            Err(e) => {
                self.set_status(format!("Could not open {}: {e}", path.display()), true);
                return None;
            }
        };
        let encoding = loaded.encoding;
        let mut doc = if let Some(bytes) = loaded.binary {
            let mut d = Document::from_text(hex_preview(&bytes));
            d.read_only = true;
            d.language_override = Some("hex".into());
            d
        } else {
            Document::from_text(&loaded.text)
        };
        doc.path = Some(path.clone());
        doc.encoding = loaded.encoding;
        doc.line_ending = loaded.line_ending;
        doc.mixed_line_endings = loaded.mixed_line_endings;
        doc.had_decode_errors = loaded.had_decode_errors;
        doc.file_identity = Some(loaded.identity);
        doc.read_only |= loaded.read_only;
        if self.settings.detect_indentation {
            doc.detect_indent_once(self.settings.indent());
        } else {
            doc.indent = self.settings.indent();
        }
        let large = doc.large_file;
        let had_errors = doc.had_decode_errors;
        let v = self.add_document(doc);
        self.push_recent(&path);
        self.replace_pristine_untitled(v);
        if had_errors {
            self.set_status(
                format!("{} contained invalid {} sequences; they were replaced with U+FFFD. Saving will not restore them.", path.display(), encoding),
                true,
            );
        } else if large {
            self.set_status("Large file: syntax highlighting and minimap are reduced".into(), false);
        }
        Some(v)
    }

    /// Opening a file into a window whose only tab is an empty untitled
    /// document replaces that tab.
    pub(crate) fn replace_pristine_untitled(&mut self, keep: ViewId) {
        let pane = self.panes.get(&self.active_pane).cloned().unwrap_or_default();
        if pane.tabs.len() != 2 {
            return;
        }
        let other = pane.tabs.iter().copied().find(|v| *v != keep);
        if let Some(o) = other {
            let doc = self.views.get(&o).and_then(|v| self.docs.get(&v.doc));
            if doc.is_some_and(|d| d.path.is_none() && !d.is_dirty() && d.len_chars() == 0) {
                self.close_view_now(o);
            }
        }
    }

    pub(crate) fn focus_view(&mut self, vid: ViewId) {
        for (pid, pane) in &mut self.panes {
            if pane.tabs.contains(&vid) {
                pane.activate(vid);
                self.active_pane = *pid;
                break;
            }
        }
        if let Some(v) = self.views.get_mut(&vid) {
            v.want_cursor_visible = true;
        }
    }

    pub(crate) fn goto_line_col(&mut self, vid: ViewId, line: usize, col: usize) {
        let Some(view) = self.views.get_mut(&vid) else { return };
        let Some(doc) = self.docs.get(&view.doc) else { return };
        let rope = doc.rope();
        let line = line.saturating_sub(1).min(rope.len_lines().saturating_sub(1));
        let len = apex_buffer::position::line_len_chars(rope, line);
        let pos = rope.line_to_char(line) + col.saturating_sub(1).min(len);
        view.set_selections(SelectionSet::single(Selection::point(pos)));
        view.want_centre = true;
    }

    pub(crate) fn close_view_now(&mut self, vid: ViewId) {
        let Some(view) = self.views.remove(&vid) else { return };
        for pane in self.panes.values_mut() {
            pane.remove(vid);
        }
        let doc_still_viewed = self.views.values().any(|v| v.doc == view.doc);
        if !doc_still_viewed {
            self.syntax.remove(&view.doc);
            self.forget_recovery(view.doc);
        }
        if !doc_still_viewed
            && let Some(doc) = self.docs.remove(&view.doc)
            && let Some(p) = doc.path
        {
            if let Some(cb) = &self.on_view_closed {
                cb(&p);
            }
            self.closed_tabs.push(ClosedTab { path: p, sels: view.sels.clone(), scroll: view.scroll });
            if self.closed_tabs.len() > 20 {
                self.closed_tabs.remove(0);
            }
        }
    }

    /// Close views, prompting for each dirty document.
    pub(crate) fn request_close(&mut self, views: Vec<ViewId>, then: AfterClose) {
        let dirty: Vec<ViewId> = views
            .iter()
            .copied()
            .filter(|v| {
                let Some(view) = self.views.get(v) else { return false };
                let other_views = self.views.values().filter(|x| x.doc == view.doc).count();
                other_views == 1 && self.docs.get(&view.doc).is_some_and(|d| d.is_dirty())
            })
            .collect();
        if dirty.is_empty() {
            self.finish_close(views, then);
        } else {
            let mut ordered = dirty.clone();
            ordered.extend(views.into_iter().filter(|v| !dirty.contains(v)));
            if let Some(&first) = dirty.first() {
                self.focus_view(first);
            }
            self.dialog = Some(Dialog::ConfirmClose { views: ordered, then, index: 0 });
        }
    }

    pub(crate) fn finish_close(&mut self, views: Vec<ViewId>, then: AfterClose) {
        for v in views {
            self.close_view_now(v);
        }
        match then {
            AfterClose::CloseViews => {
                if self.views.is_empty() {
                    self.new_untitled();
                }
            }
            AfterClose::Quit => self.quit_confirmed = true,
        }
    }

    // ----- status / recent ------------------------------------------------

    pub fn set_status(&mut self, msg: String, is_error: bool) {
        if is_error {
            tracing::warn!("{msg}");
        } else {
            tracing::info!("{msg}");
        }
        self.status = Some((msg, Instant::now(), is_error));
    }

    pub(crate) fn recent_path(&self) -> PathBuf {
        apex_project::paths::data_dir().join("recent.txt")
    }

    pub(crate) fn load_recent(&mut self) {
        if let Ok(s) = std::fs::read_to_string(self.recent_path()) {
            self.recent_files = s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).take(MAX_RECENT).collect();
        }
    }

    pub(crate) fn push_recent(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.push_front(path.to_path_buf());
        self.recent_files.truncate(MAX_RECENT);
        let p = self.recent_path();
        let _ = apex_project::paths::ensure_private_dir(p.parent().unwrap_or(Path::new(".")));
        let body: String = self.recent_files.iter().map(|p| format!("{}\n", p.display())).collect();
        if let Err(e) = fileio::save_atomic(&p, body.as_bytes()) {
            tracing::warn!("could not write recent files list: {e}");
        }
    }

    // ----- saving ---------------------------------------------------------

    pub(crate) fn save_view(&mut self, vid: ViewId, force: bool) -> bool {
        let Some(view) = self.views.get(&vid) else { return false };
        let did = view.doc;
        let Some(doc) = self.docs.get(&did) else { return false };
        let Some(path) = doc.path.clone() else {
            // No path yet: the dialog answers asynchronously, so this save is not done yet.
            self.save_view_as_then(vid, None);
            return false;
        };
        if doc.read_only && doc.language_override.as_deref() == Some("hex") {
            self.set_status("Binary previews are read-only".into(), true);
            return false;
        }
        if self.settings.trim_trailing_whitespace_on_save {
            self.run_edit(vid, ops::trim_trailing_whitespace);
        }
        if self.settings.ensure_final_newline_on_save {
            self.run_edit(vid, ops::ensure_final_newline);
        }
        let doc = self.docs.get_mut(&did).expect("doc exists");
        let opts = SaveOptions { encoding: doc.encoding, line_ending: doc.line_ending, expected: doc.file_identity, force, in_place: false };
        match fileio::save(&path, &doc.text(), &opts) {
            Ok((identity, lossy)) => {
                doc.file_identity = Some(identity);
                doc.mark_saved();
                doc.read_only = false;
                let enc = doc.encoding;
                self.forget_recovery(did);
                self.git_mark_dirty();
                if path == self.settings_path() {
                    self.settings_panel.reload_pending = true;
                }
                let msg = if lossy {
                    format!("Saved {} (some characters could not be encoded as {enc} and were replaced)", path.display())
                } else {
                    format!("Saved {}", path.display())
                };
                self.set_status(msg, lossy);
                true
            }
            Err(FileError::ChangedOnDisk) => {
                self.dialog = Some(Dialog::SaveConflict { view: vid });
                false
            }
            Err(e) => {
                self.set_status(format!("Could not save {}: {e}", path.display()), true);
                false
            }
        }
    }

    /// Save As through the native dialog; completes later in `poll_file_dialog`.
    pub(crate) fn save_view_as(&mut self, vid: ViewId) {
        self.save_view_as_then(vid, None);
    }

    /// Save the view's document under a new path.
    pub(crate) fn save_view_to(&mut self, vid: ViewId, path: PathBuf) -> bool {
        let Some(view) = self.views.get(&vid) else { return false };
        let did = view.doc;
        // Saving over a file open in another tab would desynchronise it.
        if self.docs.values().any(|d| d.id != did && d.path.as_ref() == Some(&path)) {
            self.set_status(format!("{} is already open in another tab; close it first", path.display()), true);
            return false;
        }
        let doc = self.docs.get_mut(&did).expect("doc exists");
        doc.path = Some(path.clone());
        doc.file_identity = None;
        doc.read_only = false;
        doc.language_override = None;
        doc.untitled_index = None;
        self.push_recent(&path);
        self.save_view(vid, true)
    }

    // ----- editing ----------------------------------------------------------

    /// Run an editing operation on a view and apply its result.
    pub(crate) fn run_edit(&mut self, vid: ViewId, f: impl FnOnce(&Document, &SelectionSet) -> Option<EditResult>) -> bool {
        let Some(view) = self.views.get_mut(&vid) else { return false };
        let Some(doc) = self.docs.get_mut(&view.doc) else { return false };
        view.sync(doc);
        if doc.read_only {
            self.set_status("This file is read-only".into(), true);
            return false;
        }
        let Some(result) = f(doc, &view.sels) else { return false };
        let before = view.sels.clone();
        if result.tx.is_empty() {
            view.set_selections(result.sels);
            return true;
        }
        if doc.apply(result.tx, &before, &result.sels, result.kind) {
            view.sels = result.sels;
            view.seen_revision = doc.revision();
            view.cache.invalidate();
            view.want_cursor_visible = true;
            view.quick_add_word_mode = false;
            self.make_permanent(vid);
            true
        } else {
            false
        }
    }

    pub(crate) fn with_view<R>(&mut self, vid: ViewId, f: impl FnOnce(&mut View, &Document) -> R) -> Option<R> {
        let view = self.views.get_mut(&vid)?;
        let doc = self.docs.get(&view.doc)?;
        view.sync(doc);
        Some(f(view, doc))
    }

    pub(crate) fn insert_text(&mut self, vid: ViewId, text: &str) {
        self.record_step(crate::workspace_ui::MacroStep::Insert(text.to_string()));
        let opts = InsertOptions {
            auto_pair: self.settings.auto_pair,
            auto_indent: self.settings.auto_indent,
            overwrite: self.views.get(&vid).is_some_and(|v| v.overwrite),
        };
        if self.run_edit(vid, |d, s| ops::insert_text(d, s, text, opts)) {
            self.lsp_after_insert(vid, text);
        }
    }

    pub(crate) fn copy_selection(&mut self, ctx: &egui::Context, vid: ViewId, cut: bool) {
        let Some(view) = self.views.get(&vid) else { return };
        let Some(doc) = self.docs.get(&view.doc) else { return };
        let rope = doc.rope();
        let mut parts = Vec::new();
        let mut line_mode = false;
        for s in view.sels.iter() {
            if s.is_empty() {
                let line = rope.char_to_line(s.head.min(rope.len_chars()));
                let start = rope.line_to_char(line);
                let end = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
                let mut t: String = rope.slice(start..end).into();
                if !t.ends_with('\n') {
                    t.push('\n');
                }
                parts.push(t);
                line_mode = true;
            } else {
                parts.push(rope.slice(s.range()).into());
            }
        }
        let text = if line_mode && parts.iter().all(|p| p.ends_with('\n')) { parts.concat() } else { parts.join("\n") };
        if text.is_empty() {
            return;
        }
        ctx.copy_text(text.clone());
        self.line_clipboard = if line_mode { Some(text.clone()) } else { None };
        self.clipboard_history.retain(|t| *t != text);
        self.clipboard_history.push_front(text);
        self.clipboard_history.truncate(32);
        if cut {
            if line_mode {
                self.run_edit(vid, ops::delete_lines);
            } else {
                self.run_edit(vid, ops::delete_forward);
            }
        }
    }

    pub(crate) fn paste_text(&mut self, vid: ViewId, text: String, indent: bool) {
        let is_line = self.line_clipboard.as_deref() == Some(text.as_str());
        if is_line && self.views.get(&vid).is_some_and(|v| v.sels.iter().all(|s| s.is_empty())) {
            // Paste whole lines above the caret line, keeping the caret put.
            self.run_edit(vid, |d, s| {
                let rope = d.rope();
                let mut changes = Vec::new();
                for sel in s.iter() {
                    let line = rope.char_to_line(sel.head.min(rope.len_chars()));
                    changes.push(apex_edit::Change::insert(rope.line_to_char(line), text.clone()));
                }
                let tx = apex_edit::Transaction::new(changes);
                let mut ns = s.clone();
                ns.map(&tx, apex_edit::Assoc::After);
                Some(EditResult { tx, sels: ns, kind: EditKind::Other })
            });
            return;
        }
        if indent {
            let indent_settings = self.docs.get(&self.views[&vid].doc).map(|d| d.indent).unwrap_or_default();
            self.run_edit(vid, |d, s| {
                let rope = d.rope();
                let mut changes = Vec::new();
                let mut new_sels = Vec::new();
                let mut delta: isize = 0;
                let normalised = apex_buffer::line_ending::normalise(&text, LineEnding::Lf);
                let lines: Vec<&str> = normalised.split('\n').collect();
                let min_indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| indent_settings.width_of(l)).min().unwrap_or(0);
                for sel in s.iter() {
                    let line = rope.char_to_line(sel.start());
                    let base = {
                        let l = rope.line(line);
                        let n = apex_buffer::motion::first_non_blank(l);
                        l.slice(..n).to_string()
                    };
                    let mut out = String::new();
                    for (j, l) in lines.iter().enumerate() {
                        let body = l.trim_start_matches([' ', '\t']);
                        if j > 0 {
                            out.push('\n');
                            if !body.is_empty() {
                                out.push_str(&base);
                                out.push_str(&indent_settings.whitespace_for(indent_settings.width_of(l).saturating_sub(min_indent)));
                            }
                        }
                        out.push_str(body);
                    }
                    let n = out.chars().count();
                    new_sels.push(Selection::point((sel.start() as isize + delta) as usize + n));
                    delta += n as isize - sel.len() as isize;
                    changes.push(apex_edit::Change::replace(sel.start(), sel.end(), out));
                }
                Some(EditResult { tx: apex_edit::Transaction::new(changes), sels: SelectionSet::from_vec(new_sels, s.primary_index()), kind: EditKind::Other })
            });
            return;
        }
        self.run_edit(vid, |d, s| ops::paste(d, s, &text));
    }

    // ----- commands -------------------------------------------------------

    pub fn enqueue(&mut self, cmd: Command) {
        self.queue.push_back((cmd, None));
    }

    pub(crate) fn execute(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) {
        if self.vim_command(ctx, cmd) || self.settings_command(ctx, cmd) {
            return;
        }
        if cmd.is_recordable() {
            self.record_step(crate::workspace_ui::MacroStep::Command(cmd, arg.clone()));
        }
        if self.git_command(ctx, cmd)
            || self.git_ops_command(cmd, arg.clone())
            || self.git_commit_command(ctx, cmd, arg.clone())
            || self.tasks_command(ctx, cmd, arg.clone())
            || self.lsp_command(ctx, cmd, arg.clone())
            || self.spell_command(cmd)
            || self.execute_workspace(ctx, cmd, arg.clone())
        {
            return;
        }
        let active = self.active_ids();
        let tab_width = self.settings.tab_width;
        macro_rules! view {
            () => {
                match active {
                    Some((v, _)) => v,
                    None => return,
                }
            };
        }
        macro_rules! motion {
            ($m:expr, $extend:expr) => {{
                let v = view!();
                self.with_view(v, |view, doc| view.move_cursors(doc, $m, $extend, tab_width));
            }};
        }
        match cmd {
            // File
            Command::NewFile => {
                let v = self.new_untitled();
                self.focus_view(v);
            }
            Command::OpenFile if arg.is_some() => {
                let raw = arg.clone().unwrap_or_default();
                let p = std::path::PathBuf::from(&raw);
                let p = if p.is_absolute() { p } else { self.current_dir().unwrap_or_default().join(p) };
                self.open_path(&normalise_path(&p));
            }
            Command::OpenFile => {
                let spec = crate::file_dialogs::FileDialogSpec::open_files("Open File").directory(self.current_dir());
                self.show_file_dialog(spec, crate::file_dialogs::FileDialogKind::OpenFiles);
            }
            Command::OpenRecent => {
                if let Some(a) = arg {
                    self.open_path(Path::new(&a));
                }
            }
            Command::ReopenClosed => {
                if let Some(t) = self.closed_tabs.pop()
                    && let Some(v) = self.open_path(&t.path)
                    && let Some(view) = self.views.get_mut(&v)
                {
                    view.sels = t.sels;
                    view.scroll = t.scroll;
                    if let Some(d) = self.docs.get(&view.doc) {
                        view.sels.clamp(d.rope());
                    }
                }
            }
            Command::Save => {
                let v = view!();
                match arg.clone() {
                    Some(raw) => {
                        let p = std::path::PathBuf::from(&raw);
                        let p = if p.is_absolute() { p } else { self.current_dir().unwrap_or_default().join(p) };
                        self.save_view_to(v, normalise_path(&p));
                    }
                    None => {
                        self.save_view(v, false);
                    }
                }
            }
            Command::SaveAs => {
                let v = view!();
                self.save_view_as(v);
            }
            Command::SaveAll => {
                let ids: Vec<ViewId> = self.views.keys().copied().collect();
                let mut seen = std::collections::HashSet::new();
                for v in ids {
                    let did = self.views[&v].doc;
                    if seen.insert(did) && self.docs.get(&did).is_some_and(|d| d.is_dirty() && d.path.is_some()) {
                        self.save_view(v, false);
                    }
                }
            }
            Command::Revert | Command::ReloadFromDisk => {
                let v = view!();
                self.reload_view(v, None);
            }
            Command::ReloadWithEncoding => {
                let v = view!();
                if let Some(enc) = arg.as_deref().and_then(Encoding::from_label) {
                    self.reload_view(v, Some(enc));
                }
            }
            Command::SetEncoding => {
                if let (Some((_, d)), Some(enc)) = (active, arg.as_deref().and_then(Encoding::from_label))
                    && let Some(doc) = self.docs.get_mut(&d)
                {
                    doc.encoding = enc;
                    self.set_status(format!("Encoding set to {enc}; the file will be re-encoded on save"), false);
                }
            }
            Command::SetLineEnding => {
                if let (Some((v, d)), Some(le)) = (active, arg.as_deref().and_then(LineEnding::from_label)) {
                    if let Some(doc) = self.docs.get_mut(&d) {
                        doc.line_ending = le;
                        doc.mixed_line_endings = false;
                    }
                    // Normalise existing terminators in the buffer as an undoable edit.
                    self.run_edit(v, |d, s| {
                        let text = d.text();
                        let norm = apex_buffer::line_ending::normalise(&text, le);
                        if norm == text {
                            return None;
                        }
                        let tx = apex_edit::Transaction::single(apex_edit::Change::replace(0, d.len_chars(), norm));
                        let mut ns = s.clone();
                        ns.map(&tx, apex_edit::Assoc::Before);
                        Some(EditResult { tx, sels: ns, kind: EditKind::Other })
                    });
                }
            }
            Command::CloseTab => {
                let v = view!();
                if arg.as_deref() == Some("force") {
                    self.finish_close(vec![v], AfterClose::CloseViews);
                } else {
                    self.request_close(vec![v], AfterClose::CloseViews);
                }
            }
            Command::CloseOtherTabs => {
                let v = view!();
                let others: Vec<ViewId> = self.panes[&self.active_pane].tabs.iter().copied().filter(|x| *x != v).collect();
                self.request_close(others, AfterClose::CloseViews);
            }
            Command::CloseTabsToRight => {
                let pane = &self.panes[&self.active_pane];
                let right: Vec<ViewId> = pane.tabs.iter().skip(pane.active + 1).copied().collect();
                self.request_close(right, AfterClose::CloseViews);
            }
            Command::CloseAllTabs => {
                let all: Vec<ViewId> = self.views.keys().copied().collect();
                self.request_close(all, AfterClose::CloseViews);
            }
            Command::NewWindow => {
                // One OS window per process (see ARCHITECTURE.md): a new window is a
                // new instance that shares settings, themes, key maps and snippets
                // through the configuration directory and keeps its own tabs.
                match std::env::current_exe() {
                    Ok(exe) => match std::process::Command::new(exe).arg("--new-window").spawn() {
                        Ok(_) => self.set_status("Opened a new window".into(), false),
                        Err(e) => self.set_status(format!("Could not open a new window: {e}"), true),
                    },
                    Err(e) => self.set_status(format!("Could not open a new window: {e}"), true),
                }
            }
            Command::Quit | Command::CloseWindow => {
                let all: Vec<ViewId> = self.views.keys().copied().collect();
                if arg.as_deref() == Some("force") {
                    self.finish_close(all, AfterClose::Quit);
                } else {
                    self.request_close(all, AfterClose::Quit);
                }
            }
            Command::Print => {
                // Print support where practical: the buffer as plain text through the
                // platform's default printer (lp/lpr on Unix and macOS, Out-Printer on Windows).
                if let Some((_, d)) = active
                    && let Some(doc) = self.docs.get(&d)
                {
                    let title = doc.title();
                    match print_text(&title, &doc.text()) {
                        Ok(via) => self.set_status(format!("Sent {title} to the default printer via {via}"), false),
                        Err(e) => self.set_status(format!("Could not print: {e}"), true),
                    }
                }
            }
            Command::CopyFilePath => {
                if let Some(p) = active.and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    ctx.copy_text(p.display().to_string());
                    self.set_status("Path copied".into(), false);
                }
            }
            Command::RevealInFileManager => {
                if let Some(p) = active.and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    reveal_in_file_manager(&p);
                }
            }

            // Edit
            Command::Undo | Command::Redo => {
                let v = view!();
                let view = self.views.get_mut(&v).expect("view");
                if let Some(doc) = self.docs.get_mut(&view.doc) {
                    view.sync(doc);
                    let result = if cmd == Command::Undo { doc.undo() } else { doc.redo() };
                    if let Some(sels) = result {
                        view.sels = sels;
                        view.sels.clamp(doc.rope());
                        view.seen_revision = doc.revision();
                        view.cache.invalidate();
                        view.want_cursor_visible = true;
                    } else {
                        self.set_status(if cmd == Command::Undo { "Nothing to undo" } else { "Nothing to redo" }.into(), false);
                    }
                }
            }
            Command::Cut => {
                let v = view!();
                self.copy_selection(ctx, v, true);
            }
            Command::Copy => {
                let v = view!();
                self.copy_selection(ctx, v, false);
            }
            Command::Paste | Command::PasteAndIndent => {
                // Ask the platform for clipboard contents; it arrives as Event::Paste.
                self.pending_paste_mode = Some(if cmd == Command::PasteAndIndent { "indent".into() } else { arg.unwrap_or_default() });
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            Command::PasteFromHistory => {
                if let (Some(v), Some(i)) = (active.map(|a| a.0), arg.and_then(|a| a.parse::<usize>().ok()))
                    && let Some(t) = self.clipboard_history.get(i).cloned()
                {
                    self.paste_text(v, t, false);
                }
            }
            Command::DuplicateLine => {
                let v = view!();
                self.run_edit(v, ops::duplicate);
            }
            Command::DeleteLine => {
                let v = view!();
                self.run_edit(v, ops::delete_lines);
            }
            Command::MoveLineUp => {
                let v = view!();
                self.run_edit(v, |d, s| ops::move_lines(d, s, false));
            }
            Command::MoveLineDown => {
                let v = view!();
                self.run_edit(v, |d, s| ops::move_lines(d, s, true));
            }
            Command::JoinLines => {
                let v = view!();
                self.run_edit(v, ops::join_lines);
            }
            Command::SplitLines | Command::SplitSelectionIntoLines => {
                let v = view!();
                self.with_view(v, |view, doc| {
                    view.push_selection_history();
                    let ns = ops::split_into_lines(doc, &view.sels);
                    view.set_selections(ns);
                });
            }
            Command::TransposeChars => {
                let v = view!();
                self.run_edit(v, ops::transpose_chars);
            }
            Command::SortLines => {
                let v = view!();
                self.run_edit(v, |d, s| ops::sort_lines(d, s, true, false));
            }
            Command::SortLinesCaseInsensitive => {
                let v = view!();
                self.run_edit(v, |d, s| ops::sort_lines(d, s, false, false));
            }
            Command::SortLinesReverse => {
                let v = view!();
                self.run_edit(v, |d, s| ops::sort_lines(d, s, true, true));
            }
            Command::UniqueLines => {
                let v = view!();
                self.run_edit(v, ops::unique_lines);
            }
            Command::ReverseLines => {
                let v = view!();
                self.run_edit(v, ops::reverse_lines);
            }
            Command::ShuffleLines => {
                let v = view!();
                let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
                self.run_edit(v, |d, s| ops::shuffle_lines(d, s, seed));
            }
            Command::TrimTrailingWhitespace => {
                let v = view!();
                self.run_edit(v, ops::trim_trailing_whitespace);
            }
            Command::ConvertIndentToSpaces | Command::ConvertIndentToTabs => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                let mut target = self.docs.get(&d).map(|doc| doc.indent).unwrap_or_default();
                target.style = if cmd == Command::ConvertIndentToTabs { apex_edit::IndentStyle::Tabs } else { apex_edit::IndentStyle::Spaces };
                self.run_edit(v, |doc, s| ops::convert_indentation(doc, s, target));
                if let Some(doc) = self.docs.get_mut(&d) {
                    doc.indent = target;
                }
            }
            Command::ToggleComment => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                let prefix = self.comment_prefix(d);
                self.run_edit(v, |doc, s| ops::toggle_line_comment(doc, s, &prefix));
            }
            Command::ToggleBlockComment => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                let (open, close) = self.block_comment(d);
                self.run_edit(v, |doc, s| ops::toggle_block_comment(doc, s, &open, &close));
            }
            Command::InsertDateTime => {
                let v = view!();
                let now = local_datetime_string();
                self.run_edit(v, |d, s| ops::insert_text(d, s, &now, InsertOptions { auto_pair: false, auto_indent: false, overwrite: false }));
            }
            Command::InsertSequence => {
                let v = view!();
                let (start, step) = arg
                    .as_deref()
                    .and_then(|a| {
                        let mut it = a.split(',');
                        Some((it.next()?.trim().parse().ok()?, it.next().unwrap_or("1").trim().parse().ok()?))
                    })
                    .unwrap_or((1, 1));
                self.run_edit(v, |d, s| ops::insert_sequence(d, s, start, step));
            }
            Command::UpperCase | Command::LowerCase | Command::TitleCase | Command::SwapCase | Command::SnakeCase | Command::CamelCase | Command::KebabCase => {
                let v = view!();
                let op = match cmd {
                    Command::UpperCase => CaseOp::Upper,
                    Command::LowerCase => CaseOp::Lower,
                    Command::TitleCase => CaseOp::Title,
                    Command::SwapCase => CaseOp::Swap,
                    Command::SnakeCase => CaseOp::Snake,
                    Command::CamelCase => CaseOp::Camel,
                    _ => CaseOp::Kebab,
                };
                self.run_edit(v, |d, s| ops::change_case(d, s, op));
            }
            Command::Indent | Command::Outdent | Command::InsertTab | Command::InsertNewline | Command::DeleteBackward => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                let ind = self.doc_indent(d);
                let auto = self.settings.auto_indent;
                let smart = self.settings.smart_backspace;
                if cmd == Command::InsertTab && self.try_expand_trigger(v) {
                    return;
                }
                self.run_edit(v, |doc, s| match cmd {
                    Command::Indent => ops::indent_lines(doc, s, ind),
                    Command::Outdent => ops::outdent_lines(doc, s, ind),
                    Command::InsertTab => ops::tab(doc, s, ind),
                    Command::InsertNewline => ops::newline(doc, s, ind, auto),
                    _ => ops::delete_backward(doc, s, ind, smart),
                });
            }
            Command::InsertNewlineBelow | Command::InsertNewlineAbove => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                let ind = self.doc_indent(d);
                let auto = self.settings.auto_indent;
                if cmd == Command::InsertNewlineAbove {
                    // Go to the start of the line, insert a break, then end up on the new upper line.
                    self.with_view(v, |view, doc| {
                        let rope = doc.rope();
                        view.sels.transform(|s| Selection::point(rope.line_to_char(rope.char_to_line(s.head.min(rope.len_chars())))));
                    });
                    self.run_edit(v, |doc, s| {
                        let r = ops::newline(doc, s, ind, auto)?;
                        let tx = r.tx.clone();
                        let mut sels = s.clone();
                        sels.transform(|sel| Selection::point(tx.map_pos(sel.head, apex_edit::Assoc::Before)));
                        Some(EditResult { tx, sels, kind: EditKind::Other })
                    });
                    self.with_view(v, |view, doc| view.move_cursors(doc, Motion::LineEnd, false, tab_width));
                } else {
                    self.with_view(v, |view, doc| view.move_cursors(doc, Motion::LineEnd, false, tab_width));
                    self.run_edit(v, |doc, s| ops::newline(doc, s, ind, auto));
                }
            }
            Command::DeleteForward => {
                let v = view!();
                self.run_edit(v, ops::delete_forward);
            }
            Command::DeleteWordBackward => {
                let v = view!();
                self.run_edit(v, ops::delete_word_backward);
            }
            Command::DeleteWordForward => {
                let v = view!();
                self.run_edit(v, ops::delete_word_forward);
            }
            Command::DeleteToLineStart => {
                let v = view!();
                self.run_edit(v, ops::delete_to_line_start);
            }
            Command::DeleteToLineEnd => {
                let v = view!();
                self.run_edit(v, ops::delete_to_line_end);
            }
            Command::ToggleOverwrite => {
                let v = view!();
                if let Some(view) = self.views.get_mut(&v) {
                    view.overwrite = !view.overwrite;
                }
            }
            Command::HardWrap => {
                let v = view!();
                let width = self.settings.rulers.first().copied().unwrap_or(if self.settings.wrap_column > 0 { self.settings.wrap_column } else { 80 });
                self.run_edit(v, |d, s| ops::hard_wrap(d, s, width));
            }
            Command::SurroundWith => {
                let v = view!();
                let (open, close) = match arg.as_deref() {
                    Some(a) if !a.is_empty() => {
                        let mut cs = a.chars();
                        let o = cs.next().unwrap_or('"').to_string();
                        let c = cs.next().map(|c| c.to_string()).unwrap_or_else(|| o.clone());
                        (o, c)
                    }
                    _ => ("\"".into(), "\"".into()),
                };
                self.run_edit(v, |d, s| ops::surround(d, s, &open, &close));
            }
            Command::ToggleBookmark => {
                if let Some((v, d)) = active {
                    let line = self.views[&v].primary_line(&self.docs[&d]);
                    if let Some(doc) = self.docs.get_mut(&d) {
                        doc.toggle_bookmark(line);
                    }
                }
            }
            Command::NextBookmark | Command::PrevBookmark => {
                if let Some((v, d)) = active {
                    let lines = self.docs[&d].bookmarked_lines();
                    let cur = self.views[&v].primary_line(&self.docs[&d]);
                    let target = if cmd == Command::NextBookmark {
                        lines.iter().find(|&&l| l > cur).or(lines.first())
                    } else {
                        lines.iter().rev().find(|&&l| l < cur).or(lines.last())
                    };
                    if let Some(&t) = target {
                        self.goto_line_col(v, t + 1, 1);
                    }
                }
            }
            Command::ClearBookmarks => {
                if let Some((_, d)) = active
                    && let Some(doc) = self.docs.get_mut(&d)
                {
                    doc.clear_bookmarks();
                }
            }
            Command::SetMark => {
                if let (Some((v, d)), Some(name)) = (active, arg) {
                    let pos = self.views[&v].sels.primary().head;
                    if let Some(doc) = self.docs.get_mut(&d) {
                        doc.set_mark(name, pos);
                    }
                }
            }
            Command::GotoMark => {
                if let (Some((v, d)), Some(name)) = (active, arg)
                    && let Some(pos) = self.docs[&d].mark(&name)
                {
                    self.with_view(v, |view, _| view.set_selections(SelectionSet::single(Selection::point(pos))));
                }
            }

            // Movement
            Command::MoveLeft => motion!(Motion::Left, false),
            Command::MoveRight => motion!(Motion::Right, false),
            Command::MoveUp => motion!(Motion::Up(1), false),
            Command::MoveDown => motion!(Motion::Down(1), false),
            Command::MoveWordLeft => motion!(Motion::WordLeft, false),
            Command::MoveWordRight => motion!(Motion::WordRight, false),
            Command::MoveSubwordLeft => motion!(Motion::SubwordLeft, false),
            Command::MoveSubwordRight => motion!(Motion::SubwordRight, false),
            Command::MoveLineStart => motion!(Motion::LineStart, false),
            Command::MoveLineEnd => motion!(Motion::LineEnd, false),
            Command::MoveDocStart => motion!(Motion::DocStart, false),
            Command::MoveDocEnd => motion!(Motion::DocEnd, false),
            Command::MovePageUp => motion!(Motion::PageUp, false),
            Command::MovePageDown => motion!(Motion::PageDown, false),
            Command::MoveParagraphUp => motion!(Motion::ParagraphUp, false),
            Command::MoveParagraphDown => motion!(Motion::ParagraphDown, false),
            Command::SelectLeft => motion!(Motion::Left, true),
            Command::SelectRight => motion!(Motion::Right, true),
            Command::SelectUp => motion!(Motion::Up(1), true),
            Command::SelectDown => motion!(Motion::Down(1), true),
            Command::SelectWordLeft => motion!(Motion::WordLeft, true),
            Command::SelectWordRight => motion!(Motion::WordRight, true),
            Command::SelectSubwordLeft => motion!(Motion::SubwordLeft, true),
            Command::SelectSubwordRight => motion!(Motion::SubwordRight, true),
            Command::SelectLineStart => motion!(Motion::LineStart, true),
            Command::SelectLineEnd => motion!(Motion::LineEnd, true),
            Command::SelectDocStart => motion!(Motion::DocStart, true),
            Command::SelectDocEnd => motion!(Motion::DocEnd, true),
            Command::SelectPageUp => motion!(Motion::PageUp, true),
            Command::SelectPageDown => motion!(Motion::PageDown, true),
            Command::SelectParagraphUp => motion!(Motion::ParagraphUp, true),
            Command::SelectParagraphDown => motion!(Motion::ParagraphDown, true),
            Command::MoveToBracket => {
                let v = view!();
                self.with_view(v, |view, doc| view.jump_to_bracket(doc, false));
            }
            Command::SelectToBracket => {
                let v = view!();
                self.with_view(v, |view, doc| view.jump_to_bracket(doc, true));
            }
            Command::SelectAll => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_all(doc));
            }
            Command::SelectLine => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_lines(doc));
            }
            Command::SelectWord => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_words(doc));
            }
            Command::SelectBrackets => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_to_brackets(doc));
            }
            Command::SelectScope => {
                let (v, d) = match active {
                    Some(a) => a,
                    None => return,
                };
                self.ensure_syntax(d);
                let Some(doc) = self.docs.get(&d) else { return };
                let rope = doc.rope();
                let ranges_of = |st: &SyntaxState, sel: &Selection| -> Option<Selection> {
                    let byte = rope.char_to_byte(sel.head.min(rope.len_chars()));
                    for (a, b) in st.enclosing_ranges(byte) {
                        let (ca, cb) = (rope.byte_to_char(a), rope.byte_to_char(b));
                        if ca < sel.start() || cb > sel.end() {
                            return Some(Selection::new(ca, cb));
                        }
                    }
                    None
                };
                if let Some(st) = self.syntax.get(&d).filter(|st| st.has_tree()) {
                    let mut ns = self.views[&v].sels.clone();
                    ns.transform(|sel| ranges_of(st, &sel).unwrap_or(sel));
                    if let Some(view) = self.views.get_mut(&v) {
                        view.push_selection_history();
                        view.set_selections(ns);
                    }
                } else {
                    self.with_view(v, |view, doc| view.select_to_brackets(doc));
                }
            }
            Command::SetLanguage => {
                if let (Some((_, d)), Some(id)) = (active, arg) {
                    if let Some(doc) = self.docs.get_mut(&d) {
                        doc.language_override = if id == "auto" { None } else { Some(id) };
                    }
                    self.syntax.remove(&d);
                    for v in self.views.values_mut().filter(|v| v.doc == d) {
                        v.cache.invalidate();
                    }
                }
            }
            Command::SelectIndentation => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_indentation(doc));
            }
            Command::SelectParagraph => {
                let v = view!();
                self.with_view(v, |view, doc| view.select_paragraph(doc));
            }
            Command::ShrinkSelection | Command::UndoSelection => {
                let v = view!();
                self.with_view(v, |view, _| view.undo_selection());
            }
            Command::SwapAnchor => {
                let v = view!();
                self.with_view(v, |view, _| view.sels.transform(|s| s.swap_ends()));
            }
            Command::AddCursorAbove => {
                let v = view!();
                self.with_view(v, |view, doc| view.add_cursor_vertically(doc, false, tab_width));
            }
            Command::AddCursorBelow => {
                let v = view!();
                self.with_view(v, |view, doc| view.add_cursor_vertically(doc, true, tab_width));
            }
            Command::AddNextOccurrence => {
                let v = view!();
                self.with_view(v, |view, doc| view.add_next_occurrence(doc, false));
            }
            Command::SkipNextOccurrence => {
                let v = view!();
                self.with_view(v, |view, doc| view.add_next_occurrence(doc, true));
            }
            Command::AddAllOccurrences => {
                let v = view!();
                self.with_view(v, |view, doc| view.add_all_occurrences(doc));
            }
            Command::SingleSelection => {
                let v = view!();
                if self.find.open && !self.find_has_focus(ctx) {
                    self.find.open = false;
                }
                self.with_view(v, |view, _| view.single_selection());
            }
            Command::ColumnSelectUp => {
                let v = view!();
                self.with_view(v, |view, doc| view.column_select(doc, false, tab_width));
            }
            Command::ColumnSelectDown => {
                let v = view!();
                self.with_view(v, |view, doc| view.column_select(doc, true, tab_width));
            }
            Command::SelectNextCursor | Command::SelectPrevCursor => {
                let v = view!();
                let forward = cmd == Command::SelectNextCursor;
                self.with_view(v, |view, _| {
                    view.sels.rotate_primary(forward);
                    view.want_cursor_visible = true;
                });
            }
            Command::InvertSelection => {
                let v = view!();
                self.with_view(v, |view, doc| {
                    let n = doc.len_chars();
                    let mut out = Vec::new();
                    let mut cursor = 0;
                    for s in view.sels.iter() {
                        if s.start() > cursor {
                            out.push(Selection::new(cursor, s.start()));
                        }
                        cursor = s.end();
                    }
                    if cursor < n {
                        out.push(Selection::new(cursor, n));
                    }
                    if !out.is_empty() {
                        view.push_selection_history();
                        view.set_selections(SelectionSet::from_vec(out, 0));
                    }
                });
            }

            // Find
            Command::Find | Command::Replace => {
                self.find.open = true;
                self.find.replace_open = cmd == Command::Replace;
                self.find.focus_needle = true;
                if let Some((v, d)) = active {
                    let view = &self.views[&v];
                    let doc = &self.docs[&d];
                    let p = view.sels.primary();
                    if !p.is_empty() && doc.rope().char_to_line(p.start()) == doc.rope().char_to_line(p.end()) {
                        self.find.needle = doc.rope().slice(p.range()).to_string();
                    }
                }
            }
            Command::CloseFind => {
                self.find.open = false;
                self.focus_active_editor(ctx);
            }
            Command::FindNext => self.find_step(true),
            Command::FindPrev => self.find_step(false),
            Command::FindUnderCursor => {
                let v = view!();
                let needle = self.with_view(v, |view, doc| {
                    let p = view.sels.primary();
                    let (a, b) = if p.is_empty() { apex_buffer::motion::word_at(doc.slice(), p.head) } else { (p.start(), p.end()) };
                    doc.rope().slice(a..b).to_string()
                });
                if let Some(needle) = needle.filter(|n| !n.is_empty()) {
                    self.find.needle = needle;
                    self.find.open = true;
                    self.find_step(true);
                }
            }
            Command::UseSelectionForFind => {
                let v = view!();
                if let Some(pattern) = arg.clone() {
                    self.find.needle = pattern;
                    self.find.options.regex = true;
                } else if let Some(t) = self.with_view(v, |view, doc| doc.rope().slice(view.sels.primary().range()).to_string())
                    && !t.is_empty()
                {
                    self.find.needle = t;
                }
            }
            Command::FindInSelection => {
                let v = view!();
                let p = self.views[&v].sels.primary();
                self.find.options.in_range = if p.is_empty() { None } else { Some((p.start(), p.end())) };
                self.find.open = true;
                self.find.focus_needle = true;
            }
            Command::ToggleRegex => self.find.options.regex = !self.find.options.regex,
            Command::ToggleCaseSensitive => self.find.options.case_sensitive = !self.find.options.case_sensitive,
            Command::ToggleWholeWord => self.find.options.whole_word = !self.find.options.whole_word,
            Command::ReplaceNext => self.replace_next(),
            Command::ReplaceAll => self.replace_all(),

            // View
            Command::ZoomIn => self.set_zoom(self.font_zoom + 1),
            Command::ZoomOut => self.set_zoom(self.font_zoom - 1),
            Command::ZoomReset => self.set_zoom(0),
            Command::UiScaleUp => self.set_ui_scale(ctx, self.settings.ui_scale + 0.1),
            Command::UiScaleDown => self.set_ui_scale(ctx, self.settings.ui_scale - 0.1),
            Command::ToggleLineNumbers => self.settings.line_numbers = !self.settings.line_numbers,
            Command::ToggleRelativeLineNumbers => self.settings.relative_line_numbers = !self.settings.relative_line_numbers,
            Command::ToggleWordWrap => self.settings.word_wrap = !self.settings.word_wrap,
            Command::ToggleWhitespace => {
                use crate::settings::WhitespaceMode as W;
                self.settings.show_whitespace = match self.settings.show_whitespace {
                    W::None => W::Selection,
                    W::Selection => W::All,
                    W::All => W::None,
                };
            }
            Command::ToggleIndentGuides => self.settings.indent_guides = !self.settings.indent_guides,
            Command::ToggleStatusBar => self.settings.status_bar = !self.settings.status_bar,
            Command::ToggleTabs => self.settings.tabs = !self.settings.tabs,
            Command::ToggleMenuBar => self.settings.menu_bar = !self.settings.menu_bar,
            Command::ToggleMinimap => self.settings.minimap = !self.settings.minimap,
            Command::ToggleSidebar => self.settings.sidebar = !self.settings.sidebar,
            Command::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            Command::ToggleDistractionFree => self.distraction_free = !self.distraction_free,
            Command::ToggleZenMode => {
                self.distraction_free = !self.distraction_free;
                self.fullscreen = self.distraction_free;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            Command::ToggleLowDistraction => self.settings.low_distraction = !self.settings.low_distraction,
            Command::SelectTheme => {
                if let Some(name) = arg
                    && self.themes.iter().any(|t| t.name == name)
                {
                    self.settings.theme = name.clone();
                    self.theme = Self::pick_theme(&self.themes, &name, &self.config_dir);
                    self.theme.apply_to_egui(ctx);
                    self.invalidate_all_layouts();
                }
            }
            Command::ToggleLightDark => {
                let want_dark = !self.theme.dark;
                let name = self.themes.iter().find(|t| t.dark == want_dark).map(|t| t.name.clone());
                if let Some(n) = name {
                    self.execute(ctx, Command::SelectTheme, Some(n));
                }
            }
            Command::FoldCurrent
            | Command::UnfoldCurrent
            | Command::FoldAll
            | Command::UnfoldAll
            | Command::FoldLevel1
            | Command::FoldLevel2
            | Command::FoldLevel3
            | Command::FoldComments
            | Command::ToggleFold => {
                let line = arg.as_deref().and_then(|a| a.parse::<usize>().ok());
                self.fold_command(cmd, line);
            }
            Command::ScrollLineUp | Command::ScrollLineDown => {
                let v = view!();
                let lh = self.line_height(ctx);
                let dir = if cmd == Command::ScrollLineUp { -1.0 } else { 1.0 };
                if let Some(view) = self.views.get_mut(&v) {
                    view.scroll_lines(dir, lh);
                }
            }
            Command::ScrollToCursor => {
                let v = view!();
                if let Some(view) = self.views.get_mut(&v) {
                    view.want_cursor_visible = true;
                }
            }
            Command::CentreCursor => {
                let v = view!();
                if let Some(view) = self.views.get_mut(&v) {
                    view.want_centre = true;
                }
            }
            Command::SplitVertical => self.split_active(Axis::Horizontal),
            Command::SplitHorizontal => self.split_active(Axis::Vertical),
            Command::LayoutSingle => self.set_layout_count(1, Axis::Horizontal, false),
            Command::LayoutColumns2 => self.set_layout_count(2, Axis::Horizontal, false),
            Command::LayoutColumns3 => self.set_layout_count(3, Axis::Horizontal, false),
            Command::LayoutRows2 => self.set_layout_count(2, Axis::Vertical, false),
            Command::LayoutGrid => self.set_layout_count(4, Axis::Horizontal, true),
            Command::ClosePane => self.close_pane(self.active_pane),
            Command::FocusNextPane | Command::FocusPrevPane => {
                let ids = self.layout.pane_ids();
                if let Some(i) = ids.iter().position(|p| *p == self.active_pane) {
                    let n = ids.len();
                    let j = if cmd == Command::FocusNextPane { (i + 1) % n } else { (i + n - 1) % n };
                    self.active_pane = ids[j];
                    self.focus_active_editor(ctx);
                }
            }
            Command::FocusPane1 | Command::FocusPane2 | Command::FocusPane3 | Command::FocusPane4 => {
                let i = match cmd {
                    Command::FocusPane1 => 0,
                    Command::FocusPane2 => 1,
                    Command::FocusPane3 => 2,
                    _ => 3,
                };
                if let Some(p) = self.layout.pane_ids().get(i) {
                    self.active_pane = *p;
                    self.focus_active_editor(ctx);
                }
            }
            Command::MoveTabToNextPane | Command::MoveTabToPrevPane => {
                let v = view!();
                let ids = self.layout.pane_ids();
                if ids.len() > 1 {
                    let i = ids.iter().position(|p| *p == self.active_pane).unwrap_or(0);
                    let n = ids.len();
                    let target = if cmd == Command::MoveTabToNextPane { ids[(i + 1) % n] } else { ids[(i + n - 1) % n] };
                    self.move_view_to_pane(v, target);
                }
            }
            Command::DuplicateView => {
                let v = view!();
                let dup = self.views[&v].duplicate();
                let dup_id = dup.id;
                self.views.insert(dup_id, dup);
                let ids = self.layout.pane_ids();
                let target = if ids.len() > 1 {
                    let i = ids.iter().position(|p| *p == self.active_pane).unwrap_or(0);
                    ids[(i + 1) % ids.len()]
                } else {
                    let new = self.new_pane();
                    let active = self.active_pane;
                    self.layout.split_leaf(active, new, Axis::Horizontal);
                    new
                };
                self.panes.entry(target).or_default().insert_after_active(dup_id);
                self.active_pane = target;
            }

            // Goto
            Command::GotoLine => self.dialog = Some(Dialog::GotoLine { text: String::new() }),
            Command::NextTab | Command::PrevTab => {
                let pane = self.panes.entry(self.active_pane).or_default();
                let n = pane.tabs.len();
                if n > 0 {
                    pane.active = if cmd == Command::NextTab { (pane.active + 1) % n } else { (pane.active + n - 1) % n };
                }
                self.focus_active_editor(ctx);
            }
            Command::Tab1 | Command::Tab2 | Command::Tab3 | Command::Tab4 | Command::Tab5 | Command::Tab6 | Command::Tab7 | Command::Tab8 | Command::Tab9 => {
                let i = match cmd {
                    Command::Tab1 => 0,
                    Command::Tab2 => 1,
                    Command::Tab3 => 2,
                    Command::Tab4 => 3,
                    Command::Tab5 => 4,
                    Command::Tab6 => 5,
                    Command::Tab7 => 6,
                    Command::Tab8 => 7,
                    _ => usize::MAX,
                };
                let pane = self.panes.entry(self.active_pane).or_default();
                if !pane.tabs.is_empty() {
                    pane.active = i.min(pane.tabs.len() - 1);
                }
                self.focus_active_editor(ctx);
            }
            Command::MoveTabLeft => self.panes.entry(self.active_pane).or_default().move_active(-1),
            Command::MoveTabRight => self.panes.entry(self.active_pane).or_default().move_active(1),
            Command::PinTab => {
                let v = view!();
                let pane = self.panes.entry(self.active_pane).or_default();
                if !pane.pinned.remove(&v) {
                    pane.pinned.insert(v);
                    if let Some(i) = pane.tabs.iter().position(|x| *x == v) {
                        let t = pane.tabs.remove(i);
                        let at = pane.tabs.iter().take_while(|t| pane.pinned.contains(t)).count();
                        pane.tabs.insert(at, t);
                        pane.active = at;
                    }
                }
            }

            // Tools
            Command::OpenKeymapFile => {
                let p = self.config_dir.join("keymap.toml");
                if !p.exists() {
                    let _ = apex_project::paths::ensure_private_dir(&self.config_dir);
                    let body = "# User key bindings. These are layered over the defaults.\n# Example:\n# [[bindings]]\n# keys = \"ctrl+alt+j\"\n# command = \"edit.join_lines\"\n# context = \"editor\"\n";
                    if let Err(e) = fileio::save_atomic(&p, body.as_bytes()) {
                        self.set_status(format!("Could not create key map file: {e}"), true);
                        return;
                    }
                }
                self.open_path(&p);
            }
            Command::About => self.dialog = Some(Dialog::About),
            Command::WordCount => {
                if let Some((v, d)) = active {
                    let doc = &self.docs[&d];
                    let view = &self.views[&v];
                    let text: String = if view.sels.has_extent() {
                        view.sels.iter().map(|s| doc.rope().slice(s.range()).to_string()).collect::<Vec<_>>().join("\n")
                    } else {
                        doc.text()
                    };
                    let words = text.split_whitespace().count();
                    let chars = text.chars().count();
                    let lines = text.lines().count();
                    self.set_status(format!("{lines} lines, {words} words, {chars} characters"), false);
                }
            }
            other => {
                self.set_status(format!("{} is not available yet", other.name()), false);
            }
        }
    }

    pub(crate) fn doc_indent(&self, d: DocId) -> apex_edit::IndentSettings {
        self.docs.get(&d).map(|doc| doc.indent).unwrap_or_else(|| self.settings.indent())
    }

    /// The language for a document: explicit override, else detection.
    pub(crate) fn language_of(&self, d: DocId) -> &'static apex_syntax::LanguageDef {
        let Some(doc) = self.docs.get(&d) else { return apex_syntax::lookup("plain").expect("registered") };
        if let Some(l) = doc.language_override.as_deref().and_then(apex_syntax::lookup) {
            return l;
        }
        let first: String = doc.rope().line(0).chars().take(200).collect();
        apex_syntax::detect(doc.path.as_deref(), &first)
    }

    /// Create the syntax state for a document if it does not exist yet.
    pub(crate) fn ensure_syntax(&mut self, d: DocId) {
        if self.syntax.contains_key(&d) {
            return;
        }
        let lang = self.language_of(d);
        if let Some(doc) = self.docs.get_mut(&d) {
            let st = SyntaxState::new(lang, doc, std::time::Duration::from_millis(12));
            self.syntax.insert(d, st);
        }
    }

    pub(crate) fn comment_prefix(&self, d: DocId) -> String {
        let lang = self.language_of(d);
        lang.line_comment.map(str::to_string).unwrap_or_else(|| "// ".into())
    }

    pub(crate) fn block_comment(&self, d: DocId) -> (String, String) {
        let lang = self.language_of(d);
        let (o, c) = lang.block_comment.unwrap_or(("/*", "*/"));
        (o.into(), c.into())
    }

    /// Advance the syntax state for a document (apply pending edits, poll
    /// background parses) and invalidate view layouts if colours changed.
    pub(crate) fn syntax_tick(&mut self, d: DocId) {
        self.ensure_syntax(d);
        let Some(st) = self.syntax.get_mut(&d) else { return };
        let Some(doc) = self.docs.get_mut(&d) else { return };
        if st.update(doc, std::time::Duration::from_millis(8)) {
            for v in self.views.values_mut().filter(|v| v.doc == d) {
                v.cache.invalidate();
            }
            self.fold_cache.remove(&d);
        }
        if st.is_parsing_in_background() {
            self.pending_repaint = true;
        }
    }

    /// Fold regions for a document (syntax tree if available, else indentation).
    pub(crate) fn fold_regions_for(&mut self, d: DocId) -> Vec<(usize, usize)> {
        let Some(doc) = self.docs.get(&d) else { return Vec::new() };
        if let Some((rev, regions)) = self.fold_cache.get(&d)
            && *rev == doc.revision()
        {
            return regions.clone();
        }
        if doc.len_lines() > 200_000 {
            return Vec::new();
        }
        let regions: Vec<(usize, usize)> = match self.syntax.get(&d).filter(|st| st.has_tree()) {
            Some(st) => apex_syntax::fold::syntax_folds(st, doc.rope()).into_iter().map(|r| (r.start, r.end)).collect(),
            None => apex_syntax::fold::indent_folds(doc.rope(), self.settings.tab_width).into_iter().map(|r| (r.start, r.end)).collect(),
        };
        self.fold_cache.insert(d, (doc.revision(), regions.clone()));
        regions
    }

    /// Fold regions with their nesting depth.
    pub(crate) fn fold_regions_with_depth(&mut self, d: DocId) -> Vec<(usize, usize, u16)> {
        let Some(doc) = self.docs.get(&d) else { return Vec::new() };
        match self.syntax.get(&d).filter(|st| st.has_tree()) {
            Some(st) => apex_syntax::fold::syntax_folds(st, doc.rope()).into_iter().map(|r| (r.start, r.end, r.depth)).collect(),
            None => apex_syntax::fold::indent_folds(doc.rope(), self.settings.tab_width).into_iter().map(|r| (r.start, r.end, r.depth)).collect(),
        }
    }

    pub(crate) fn fold_command(&mut self, cmd: Command, arg: Option<usize>) {
        let Some((v, d)) = self.active_ids() else { return };
        self.ensure_syntax(d);
        let regions = self.fold_regions_for(d);
        let with_depth = self.fold_regions_with_depth(d);
        let line = arg.unwrap_or_else(|| self.views[&v].primary_line(&self.docs[&d]));
        let lang = self.language_of(d);
        let Some(doc) = self.docs.get(&d) else { return };
        let Some(view) = self.views.get_mut(&v) else { return };
        match cmd {
            Command::FoldCurrent => {
                // Innermost region containing the caret line.
                if let Some((a, b)) = regions.iter().filter(|(a, b)| line >= *a && line <= *b).min_by_key(|(a, b)| b - a) {
                    view.fold(doc, *a, *b);
                }
            }
            Command::UnfoldCurrent => {
                view.unfold(doc, line);
            }
            Command::ToggleFold => {
                if view.fold_at(doc, line).is_some() {
                    view.unfold(doc, line);
                } else if let Some((a, b)) = regions.iter().find(|(a, _)| *a == line) {
                    view.fold(doc, *a, *b);
                }
            }
            Command::FoldAll => {
                for (a, b, depth) in &with_depth {
                    if *depth == 0 {
                        view.fold(doc, *a, *b);
                    }
                }
            }
            Command::FoldLevel1 | Command::FoldLevel2 | Command::FoldLevel3 => {
                let level = match cmd {
                    Command::FoldLevel1 => 0,
                    Command::FoldLevel2 => 1,
                    _ => 2,
                };
                view.unfold_all();
                for (a, b, depth) in &with_depth {
                    if *depth == level {
                        view.fold(doc, *a, *b);
                    }
                }
            }
            Command::FoldComments => {
                let rope = doc.rope();
                for (a, b) in &regions {
                    let text = doc.line_text(*a);
                    let t = text.trim_start();
                    let is_comment =
                        lang.line_comment.is_some_and(|c| t.starts_with(c.trim_end())) || lang.block_comment.is_some_and(|(o, _)| t.starts_with(o));
                    if is_comment && *b < rope.len_lines() {
                        view.fold(doc, *a, *b);
                    }
                }
            }
            Command::UnfoldAll => view.unfold_all(),
            _ => {}
        }
        view.want_cursor_visible = true;
    }

    pub(crate) fn current_dir(&self) -> Option<PathBuf> {
        self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.as_ref()).and_then(|p| p.parent().map(Path::to_path_buf))
    }

    pub(crate) fn reload_view(&mut self, vid: ViewId, encoding: Option<Encoding>) {
        let Some(view) = self.views.get(&vid) else { return };
        let did = view.doc;
        let Some(path) = self.docs.get(&did).and_then(|d| d.path.clone()) else { return };
        let fallback = Encoding::from_label(&self.settings.fallback_encoding).unwrap_or(Encoding::Legacy(encoding_rs::WINDOWS_1252));
        match fileio::load(&path, encoding, fallback) {
            Ok(l) if l.binary.is_none() => {
                let sels = self.views[&vid].sels.clone();
                let doc = self.docs.get_mut(&did).expect("doc");
                doc.replace_all(&l.text, &sels);
                doc.encoding = l.encoding;
                doc.line_ending = l.line_ending;
                doc.mixed_line_endings = l.mixed_line_endings;
                doc.had_decode_errors = l.had_decode_errors;
                doc.file_identity = Some(l.identity);
                doc.read_only = l.read_only;
                doc.mark_clean();
                for v in self.views.values_mut().filter(|v| v.doc == did) {
                    v.cache.invalidate();
                }
                self.forget_recovery(did);
                self.set_status(format!("Reloaded {} as {}", path.display(), l.encoding), false);
            }
            Ok(_) => self.set_status("File is binary; reopen it to view the preview".into(), true),
            Err(e) => self.set_status(format!("Could not reload: {e}"), true),
        }
    }

    pub(crate) fn invalidate_all_layouts(&mut self) {
        for v in self.views.values_mut() {
            v.cache.invalidate();
            v.cache.clear();
        }
    }

    pub(crate) fn set_zoom(&mut self, steps: i32) {
        self.font_zoom = steps.clamp(-8, 40);
        self.invalidate_all_layouts();
    }

    pub(crate) fn set_ui_scale(&mut self, ctx: &egui::Context, scale: f32) {
        self.settings.ui_scale = scale.clamp(0.5, 3.0);
        ctx.set_zoom_factor(self.settings.ui_scale);
        self.invalidate_all_layouts();
    }

    pub(crate) fn line_height(&self, ctx: &egui::Context) -> f32 {
        let font = editor::font_for(&self.settings, self.font_zoom);
        editor::metrics(ctx, &font, self.settings.line_height).0
    }

    pub(crate) fn focus_active_editor(&self, ctx: &egui::Context) {
        if let Some(v) = self.active_view_id() {
            ctx.memory_mut(|m| m.request_focus(v.egui_id()));
        }
    }

    // ----- panes ---------------------------------------------------------

    pub(crate) fn new_pane(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        self.panes.insert(id, Pane::default());
        id
    }

    pub(crate) fn split_active(&mut self, axis: Axis) {
        let new = self.new_pane();
        let active = self.active_pane;
        if self.layout.split_leaf(active, new, axis) {
            // The new pane shows a second view of the active document.
            if let Some(v) = self.active_view_id() {
                let dup = self.views[&v].duplicate();
                let id = dup.id;
                self.views.insert(id, dup);
                self.panes.get_mut(&new).expect("pane").insert_after_active(id);
            }
            self.active_pane = new;
        }
    }

    pub(crate) fn set_layout_count(&mut self, count: usize, axis: Axis, grid: bool) {
        // Collapse to a single pane first, moving every tab into it.
        let ids = self.layout.pane_ids();
        let keep = ids[0];
        for p in ids.iter().skip(1) {
            let tabs = self.panes.remove(p).map(|p| p.tabs).unwrap_or_default();
            for t in tabs {
                self.panes.entry(keep).or_default().tabs.push(t);
            }
        }
        self.layout = Layout::Leaf(keep);
        self.active_pane = keep;
        if grid {
            let b = self.new_pane();
            let c = self.new_pane();
            let d = self.new_pane();
            self.layout = Layout::Split {
                axis: Axis::Vertical,
                children: vec![
                    Layout::Split { axis: Axis::Horizontal, children: vec![Layout::Leaf(keep), Layout::Leaf(b)], fractions: vec![0.5, 0.5] },
                    Layout::Split { axis: Axis::Horizontal, children: vec![Layout::Leaf(c), Layout::Leaf(d)], fractions: vec![0.5, 0.5] },
                ],
                fractions: vec![0.5, 0.5],
            };
        } else {
            let mut prev = keep;
            for _ in 1..count {
                let n = self.new_pane();
                self.layout.split_leaf(prev, n, axis);
                prev = n;
            }
        }
        // Distribute tabs round-robin if there are enough.
        let ids = self.layout.pane_ids();
        if ids.len() > 1 {
            let tabs = std::mem::take(&mut self.panes.entry(keep).or_default().tabs);
            if tabs.len() >= ids.len() {
                for (i, t) in tabs.into_iter().enumerate() {
                    self.panes.entry(ids[i % ids.len()]).or_default().tabs.push(t);
                }
            } else {
                self.panes.entry(keep).or_default().tabs = tabs;
            }
        }
        for p in self.panes.values_mut() {
            p.active = p.active.min(p.tabs.len().saturating_sub(1));
        }
    }

    pub(crate) fn close_pane(&mut self, pane: PaneId) {
        if self.layout.pane_ids().len() <= 1 {
            return;
        }
        let tabs = self.panes.get(&pane).map(|p| p.tabs.clone()).unwrap_or_default();
        let dirty_unique = tabs.iter().any(|v| {
            let d = self.views[v].doc;
            self.views.values().filter(|x| x.doc == d).count() == 1 && self.docs[&d].is_dirty()
        });
        if dirty_unique {
            self.request_close(tabs, AfterClose::CloseViews);
            return;
        }
        for v in tabs {
            self.close_view_now(v);
        }
        self.layout.remove_leaf(pane);
        self.panes.remove(&pane);
        self.active_pane = self.layout.pane_ids()[0];
        if self.views.is_empty() {
            self.new_untitled();
        }
    }

    pub(crate) fn move_view_to_pane(&mut self, v: ViewId, target: PaneId) {
        for p in self.panes.values_mut() {
            p.remove(v);
        }
        self.panes.entry(target).or_default().insert_after_active(v);
        self.active_pane = target;
    }

    // ----- find -----------------------------------------------------------

    pub(crate) fn find_has_focus(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.focused()).is_some_and(|id| id == egui::Id::new("apex-find-needle") || id == egui::Id::new("apex-find-replace"))
    }

    pub(crate) fn update_matches(&mut self) {
        let Some((_, d)) = self.active_ids() else { return };
        let Some(doc) = self.docs.get(&d) else { return };
        let key = (d, doc.revision(), self.find.needle.clone(), self.find.options);
        if self.find.rev.as_ref() == Some(&key) {
            return;
        }
        self.find.rev = Some(key);
        self.find.matches.clear();
        self.find.current = None;
        self.find.error = None;
        if self.find.needle.is_empty() {
            return;
        }
        match Query::new(&self.find.needle, self.find.options) {
            Ok(q) => self.find.matches = q.find_all(doc.rope(), 100_000).into_iter().map(|m| m.range).collect(),
            Err(e) => self.find.error = Some(e.to_string()),
        }
    }

    pub(crate) fn find_step(&mut self, forward: bool) {
        self.update_matches();
        let Some((v, _)) = self.active_ids() else { return };
        if self.find.matches.is_empty() {
            return;
        }
        if !self.find.needle.is_empty() {
            self.find.history.retain(|h| *h != self.find.needle);
            self.find.history.push_front(self.find.needle.clone());
            self.find.history.truncate(20);
        }
        let head = self.views[&v].sels.primary();
        let wrap = self.settings.find_wrap;
        let ms: Vec<apex_edit::search::Match> = self.find.matches.iter().map(|r| apex_edit::search::Match { range: r.clone() }).collect();
        let idx = if forward {
            apex_edit::search::next_match_index(&ms, head.start() + 1, wrap)
        } else {
            apex_edit::search::prev_match_index(&ms, head.start(), wrap)
        };
        if let Some(i) = idx {
            self.find.current = Some(i);
            let r = self.find.matches[i].clone();
            if let Some(view) = self.views.get_mut(&v) {
                view.set_selections(SelectionSet::single(Selection::new(r.start, r.end)));
            }
        }
    }

    pub(crate) fn replace_next(&mut self) {
        self.update_matches();
        let Some((v, _)) = self.active_ids() else { return };
        let sel = self.views[&v].sels.primary();
        let Some(i) = self.find.matches.iter().position(|m| m.start == sel.start() && m.end == sel.end()) else {
            self.find_step(true);
            return;
        };
        let r = self.find.matches[i].clone();
        let replacement = self.find.replacement.clone();
        let q = Query::new(&self.find.needle, self.find.options).ok();
        self.run_edit(v, |d, _| {
            let text = q
                .as_ref()
                .map(|q| q.expand_replacement(d.rope(), &apex_edit::search::Match { range: r.clone() }, &replacement))
                .unwrap_or_else(|| replacement.clone());
            let n = text.chars().count();
            let tx = apex_edit::Transaction::single(apex_edit::Change::replace(r.start, r.end, text));
            Some(EditResult { tx, sels: SelectionSet::single(Selection::point(r.start + n)), kind: EditKind::Other })
        });
        self.find_step(true);
    }

    pub(crate) fn replace_all(&mut self) {
        self.update_matches();
        let Some((v, _)) = self.active_ids() else { return };
        if self.find.matches.is_empty() {
            self.set_status("No matches".into(), false);
            return;
        }
        let matches = self.find.matches.clone();
        let replacement = self.find.replacement.clone();
        let q = Query::new(&self.find.needle, self.find.options).ok();
        let count = matches.len();
        self.run_edit(v, |d, s| {
            let changes: Vec<apex_edit::Change> = matches
                .iter()
                .map(|r| {
                    let text = q
                        .as_ref()
                        .map(|q| q.expand_replacement(d.rope(), &apex_edit::search::Match { range: r.clone() }, &replacement))
                        .unwrap_or_else(|| replacement.clone());
                    apex_edit::Change::replace(r.start, r.end, text)
                })
                .collect();
            let tx = apex_edit::Transaction::new(changes);
            let mut ns = s.clone();
            ns.map(&tx, apex_edit::Assoc::Before);
            Some(EditResult { tx, sels: ns, kind: EditKind::Other })
        });
        self.set_status(format!("Replaced {count} occurrence{}", if count == 1 { "" } else { "s" }), false);
    }

    // ----- external changes -------------------------------------------------

    pub(crate) fn check_external_changes(&mut self) {
        let ids: Vec<DocId> = self.docs.keys().copied().collect();
        for d in ids {
            let doc = self.docs.get_mut(&d).expect("doc");
            let (Some(path), Some(known)) = (doc.path.clone(), doc.file_identity) else { continue };
            match fileio::identity(&path) {
                Some(now) if !fileio::same_identity(&known, &now) => {
                    if doc.is_dirty() {
                        if !matches!(self.dialog, Some(Dialog::ExternalChange { .. })) {
                            self.dialog = Some(Dialog::ExternalChange { doc: d });
                        }
                    } else if let Some(v) = self.views.values().find(|v| v.doc == d).map(|v| v.id) {
                        self.reload_view(v, None);
                        self.set_status(format!("{} changed on disk and was reloaded", path.display()), false);
                    }
                }
                None => {
                    // Deleted externally: keep the buffer; Save will recreate it.
                    doc.file_identity = None;
                    if !doc.is_dirty() {
                        let sels = SelectionSet::default();
                        let text = doc.text();
                        doc.replace_all(&text, &sels);
                    }
                    self.set_status(format!("{} was deleted on disk; save to recreate it", path.display()), true);
                }
                _ => {}
            }
        }
    }

    // ----- UI ---------------------------------------------------------------

    pub(crate) fn show_menu_bar(&mut self, ui: &mut Ui) {
        let km = &self.keymap;
        let item = |ui: &mut Ui, queue: &mut VecDeque<(Command, Option<String>)>, cmd: Command| {
            let b = egui::Button::new(cmd.name()).shortcut_text(km.shortcut_for(cmd).unwrap_or_default());
            if ui.add(b).clicked() {
                queue.push_back((cmd, None));
                ui.close();
            }
        };
        let mut q = std::mem::take(&mut self.queue);
        let recent = self.recent_files.clone();
        let themes: Vec<(String, bool)> = self.themes.iter().map(|t| (t.name.clone(), t.name == self.theme.name)).collect();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                for c in [Command::NewFile, Command::OpenFile, Command::OpenFolder] {
                    item(ui, &mut q, c);
                }
                ui.menu_button("Open Recent", |ui| {
                    if recent.is_empty() {
                        ui.label("No recent files");
                    }
                    for p in recent.iter().take(15) {
                        if ui.button(p.display().to_string()).clicked() {
                            q.push_back((Command::OpenRecent, Some(p.display().to_string())));
                            ui.close();
                        }
                    }
                });
                item(ui, &mut q, Command::ReopenClosed);
                ui.separator();
                for c in [Command::Save, Command::SaveAs, Command::SaveAll, Command::Revert] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                ui.menu_button("Save with Encoding", |ui| {
                    for enc in Encoding::all() {
                        if ui.button(enc.label()).clicked() {
                            q.push_back((Command::SetEncoding, Some(enc.id())));
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Reload with Encoding", |ui| {
                    for enc in Encoding::all() {
                        if ui.button(enc.label()).clicked() {
                            q.push_back((Command::ReloadWithEncoding, Some(enc.id())));
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Line Endings", |ui| {
                    for le in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
                        if ui.button(le.label()).clicked() {
                            q.push_back((Command::SetLineEnding, Some(le.label().into())));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                for c in [Command::CopyFilePath, Command::RevealInFileManager] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::CloseTab, Command::CloseOtherTabs, Command::CloseTabsToRight, Command::CloseAllTabs] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                item(ui, &mut q, Command::Quit);
            });
            ui.menu_button("Edit", |ui| {
                for c in [Command::Undo, Command::Redo] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::Cut, Command::Copy, Command::Paste, Command::PasteAndIndent] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                ui.menu_button("Line", |ui| {
                    for c in [
                        Command::Indent,
                        Command::Outdent,
                        Command::DuplicateLine,
                        Command::DeleteLine,
                        Command::MoveLineUp,
                        Command::MoveLineDown,
                        Command::JoinLines,
                        Command::InsertNewlineBelow,
                        Command::InsertNewlineAbove,
                    ] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Comment", |ui| {
                    for c in [Command::ToggleComment, Command::ToggleBlockComment] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Text", |ui| {
                    for c in [
                        Command::TransposeChars,
                        Command::UpperCase,
                        Command::LowerCase,
                        Command::TitleCase,
                        Command::SwapCase,
                        Command::SnakeCase,
                        Command::CamelCase,
                        Command::KebabCase,
                        Command::HardWrap,
                        Command::InsertDateTime,
                    ] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Sort", |ui| {
                    for c in [
                        Command::SortLines,
                        Command::SortLinesCaseInsensitive,
                        Command::SortLinesReverse,
                        Command::UniqueLines,
                        Command::ReverseLines,
                        Command::ShuffleLines,
                    ] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Whitespace", |ui| {
                    for c in [Command::TrimTrailingWhitespace, Command::ConvertIndentToSpaces, Command::ConvertIndentToTabs] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Bookmarks", |ui| {
                    for c in [Command::ToggleBookmark, Command::NextBookmark, Command::PrevBookmark, Command::ClearBookmarks] {
                        item(ui, &mut q, c);
                    }
                });
                ui.separator();
                item(ui, &mut q, Command::ToggleOverwrite);
            });
            ui.menu_button("Selection", |ui| {
                for c in [
                    Command::SelectAll,
                    Command::SelectLine,
                    Command::SelectWord,
                    Command::SelectBrackets,
                    Command::SelectIndentation,
                    Command::SelectParagraph,
                    Command::ShrinkSelection,
                    Command::SwapAnchor,
                    Command::InvertSelection,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [
                    Command::AddCursorAbove,
                    Command::AddCursorBelow,
                    Command::AddNextOccurrence,
                    Command::SkipNextOccurrence,
                    Command::AddAllOccurrences,
                    Command::UndoSelection,
                    Command::SingleSelection,
                    Command::SplitSelectionIntoLines,
                    Command::ColumnSelectUp,
                    Command::ColumnSelectDown,
                ] {
                    item(ui, &mut q, c);
                }
            });
            ui.menu_button("Find", |ui| {
                for c in [
                    Command::Find,
                    Command::FindNext,
                    Command::FindPrev,
                    Command::FindUnderCursor,
                    Command::UseSelectionForFind,
                    Command::FindInSelection,
                    Command::Replace,
                    Command::ReplaceNext,
                    Command::ReplaceAll,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                item(ui, &mut q, Command::FindInFiles);
            });
            ui.menu_button("View", |ui| {
                ui.menu_button("Layout", |ui| {
                    for c in [
                        Command::LayoutSingle,
                        Command::LayoutColumns2,
                        Command::LayoutColumns3,
                        Command::LayoutRows2,
                        Command::LayoutGrid,
                        Command::SplitVertical,
                        Command::SplitHorizontal,
                        Command::ClosePane,
                        Command::FocusNextPane,
                        Command::DuplicateView,
                        Command::MoveTabToNextPane,
                    ] {
                        item(ui, &mut q, c);
                    }
                });
                ui.menu_button("Colour Scheme", |ui| {
                    for (name, selected) in &themes {
                        if ui.selectable_label(*selected, name).clicked() {
                            q.push_back((Command::SelectTheme, Some(name.clone())));
                            ui.close();
                        }
                    }
                });
                for c in [
                    Command::ToggleLineNumbers,
                    Command::ToggleRelativeLineNumbers,
                    Command::ToggleWordWrap,
                    Command::ToggleWhitespace,
                    Command::ToggleIndentGuides,
                    Command::ToggleMinimap,
                    Command::ToggleSidebar,
                    Command::ToggleTabs,
                    Command::ToggleStatusBar,
                    Command::ToggleMenuBar,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::ZoomIn, Command::ZoomOut, Command::ZoomReset, Command::UiScaleUp, Command::UiScaleDown] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in
                    [Command::ToggleFullscreen, Command::ToggleDistractionFree, Command::ToggleZenMode, Command::ToggleLowDistraction, Command::CentreCursor]
                {
                    item(ui, &mut q, c);
                }
            });
            ui.menu_button("Goto", |ui| {
                for c in [
                    Command::GotoAnything,
                    Command::GotoSymbol,
                    Command::GotoSymbolInProject,
                    Command::NavigateBack,
                    Command::NavigateForward,
                    Command::GotoLine,
                    Command::NextTab,
                    Command::PrevTab,
                    Command::MoveTabLeft,
                    Command::MoveTabRight,
                    Command::PinTab,
                    Command::MoveToBracket,
                ] {
                    item(ui, &mut q, c);
                }
            });
            ui.menu_button("Project", |ui| {
                for c in [
                    Command::OpenFolder,
                    Command::AddFolder,
                    Command::RemoveFolder,
                    Command::CloseProject,
                    Command::RefreshExplorer,
                    Command::RevealActiveFile,
                    Command::NewFileInFolder,
                    Command::NewFolder,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::OpenProject, Command::SaveProject, Command::RecentProjects, Command::OpenProjectSettings] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::RenameFile, Command::DuplicateFile, Command::DeleteFile] {
                    item(ui, &mut q, c);
                }
            });
            ui.menu_button("Git", |ui| {
                for c in [Command::GitToggleSourceControl, Command::GitStatusPanel, Command::GitShowOutput] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::GitCommit, Command::GitCommitAmend, Command::GitUndoCommit] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [
                    Command::GitStageFile,
                    Command::GitUnstageFile,
                    Command::GitStageHunk,
                    Command::GitStageLines,
                    Command::GitStageAll,
                    Command::GitUnstageAll,
                    Command::GitDiscardFile,
                    Command::GitOpenChanges,
                    Command::GitOpenStagedChanges,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::NextChange, Command::PrevChange, Command::ShowHunkDiff, Command::RevertHunk] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::CompareWithSaved, Command::CompareWithIndex, Command::CompareWithHead, Command::FileHistory] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                item(ui, &mut q, Command::GitCancel);
            });
            ui.menu_button("Tools", |ui| {
                for c in [
                    Command::CommandPalette,
                    Command::Settings,
                    Command::OpenKeymapFile,
                    Command::KeyboardShortcuts,
                    Command::WordCount,
                    Command::ShowRecovery,
                    Command::ShowClipboardHistory,
                ] {
                    item(ui, &mut q, c);
                }
                ui.separator();
                for c in [Command::RecordMacro, Command::PlayMacro, Command::SaveMacro, Command::RunNamedMacro] {
                    item(ui, &mut q, c);
                }
            });
            ui.menu_button("Help", |ui| {
                item(ui, &mut q, Command::About);
            });
        });
        self.queue = q;
    }

    pub(crate) fn show_tabs(&mut self, ui: &mut Ui, pane_id: PaneId) {
        let theme = self.theme.clone();
        let Some(pane) = self.panes.get(&pane_id) else { return };
        let tabs = pane.tabs.clone();
        let active = pane.active;
        let pinned = pane.pinned.clone();
        let is_active_pane = pane_id == self.active_pane;
        let mut to_activate = None;
        let mut to_close = None;
        let mut ctx_cmd = None;
        let bar_h = 30.0;
        let (bar_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), bar_h), egui::Sense::hover());
        let painter = ui.painter_at(bar_rect);
        painter.rect_filled(bar_rect, 0.0, theme.ui.surface);
        let font = egui::FontId::proportional(self.settings.ui_font_size);
        let scroll_id = ui.id().with(("tabscroll", pane_id.0));
        let mut tab_scroll: f32 = ui.ctx().data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
        let widths: Vec<f32> = tabs
            .iter()
            .map(|v| {
                let title = self.views.get(v).and_then(|v| self.docs.get(&v.doc)).map(|d| d.title()).unwrap_or_default();
                let g = ui.ctx().fonts_mut(|f| f.layout_no_wrap(title, font.clone(), Color32::WHITE));
                g.rect.width() + 44.0
            })
            .collect();
        let total: f32 = widths.iter().sum();
        if ui.rect_contains_pointer(bar_rect) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d != egui::Vec2::ZERO {
                tab_scroll = (tab_scroll - d.x - d.y).clamp(0.0, (total - bar_rect.width()).max(0.0));
            }
        }
        // Keep the active tab visible.
        if active < widths.len() {
            let start: f32 = widths.iter().take(active).sum();
            let end = start + widths[active];
            if start < tab_scroll {
                tab_scroll = start;
            } else if end > tab_scroll + bar_rect.width() {
                tab_scroll = end - bar_rect.width();
            }
        }
        tab_scroll = tab_scroll.clamp(0.0, (total - bar_rect.width()).max(0.0));
        ui.ctx().data_mut(|d| d.insert_temp(scroll_id, tab_scroll));
        let mut x = bar_rect.min.x - tab_scroll;
        for (i, vid) in tabs.iter().enumerate() {
            let Some(view) = self.views.get(vid) else { continue };
            let Some(doc) = self.docs.get(&view.doc) else { continue };
            let w = widths[i];
            let rect = Rect::from_min_size(pos2(x, bar_rect.min.y), vec2(w, bar_h));
            x += w;
            if rect.max.x < bar_rect.min.x || rect.min.x > bar_rect.max.x {
                continue;
            }
            let id = ui.id().with(("tab", vid.0));
            let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
            if resp.drag_started() {
                self.dragging_tab = Some((*vid, pane_id));
            }
            let is_active = i == active;
            let bg = if is_active {
                theme.ui.tab_active
            } else if resp.hovered() {
                theme.ui.hover
            } else {
                theme.ui.tab_inactive
            };
            painter.rect_filled(rect, 0.0, bg);
            if is_active && is_active_pane {
                painter.rect_filled(Rect::from_min_size(rect.min, vec2(w, 2.0)), 0.0, theme.ui.accent);
            }
            painter.vline(rect.max.x - 0.5, rect.y_range(), egui::Stroke::new(1.0, theme.ui.border));
            let preview = self.tab_is_preview(*vid);
            let colour = if preview {
                theme.ui.text_muted
            } else if is_active {
                theme.ui.tab_text_active
            } else {
                theme.ui.tab_text_inactive
            };
            let mut title = doc.title();
            if pinned.contains(vid) {
                title = format!("📌 {title}");
            }
            painter.text(pos2(rect.min.x + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, &title, font.clone(), colour);
            let close_rect = Rect::from_center_size(pos2(rect.max.x - 16.0, rect.center().y), vec2(16.0, 16.0));
            let close_resp = ui.interact(close_rect, id.with("close"), egui::Sense::click());
            if doc.is_dirty() && !close_resp.hovered() {
                painter.circle_filled(close_rect.center(), 4.0, if is_active { theme.ui.text } else { theme.ui.text_muted });
            } else if close_resp.hovered() || resp.hovered() || is_active {
                let c = if close_resp.hovered() { theme.ui.text } else { theme.ui.text_muted };
                if close_resp.hovered() {
                    painter.rect_filled(close_rect, 3.0, theme.ui.hover);
                }
                let o = 3.5;
                painter.line_segment([close_rect.center() + vec2(-o, -o), close_rect.center() + vec2(o, o)], egui::Stroke::new(1.2, c));
                painter.line_segment([close_rect.center() + vec2(-o, o), close_rect.center() + vec2(o, -o)], egui::Stroke::new(1.2, c));
            }
            if close_resp.clicked() {
                to_close = Some(*vid);
            } else if resp.clicked() {
                to_activate = Some(*vid);
            } else if resp.middle_clicked() {
                to_close = Some(*vid);
            }
            if let Some(p) = &doc.path {
                resp.clone().on_hover_text(p.display().to_string());
            }
            resp.context_menu(|ui| {
                for c in [
                    Command::CloseTab,
                    Command::CloseOtherTabs,
                    Command::CloseTabsToRight,
                    Command::PinTab,
                    Command::CopyFilePath,
                    Command::RevealInFileManager,
                    Command::DuplicateView,
                ] {
                    if ui.button(c.name()).clicked() {
                        ctx_cmd = Some((*vid, c));
                        ui.close();
                    }
                }
            });
        }
        // Drop a dragged tab onto this bar: reorder within the pane or move between panes.
        if let Some((dragged, from)) = self.dragging_tab {
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let released = ui.input(|i| i.pointer.primary_released());
            if let Some(p) = pointer {
                if bar_rect.contains(p) {
                    // Insertion marker.
                    let mut ix = bar_rect.min.x - tab_scroll;
                    let mut insert_at = tabs.len();
                    for (i, w) in widths.iter().enumerate() {
                        if p.x < ix + w / 2.0 {
                            insert_at = i;
                            break;
                        }
                        ix += w;
                    }
                    let marker_x = bar_rect.min.x - tab_scroll + widths.iter().take(insert_at).sum::<f32>();
                    painter.rect_filled(Rect::from_min_size(pos2(marker_x - 1.0, bar_rect.min.y + 4.0), vec2(2.0, bar_h - 8.0)), 1.0, theme.ui.accent);
                    if released {
                        let title = self.views.get(&dragged).and_then(|v| self.docs.get(&v.doc)).map(|d| d.title()).unwrap_or_default();
                        let _ = title;
                        if from == pane_id {
                            if let Some(pane) = self.panes.get_mut(&pane_id)
                                && let Some(cur) = pane.tabs.iter().position(|t| *t == dragged)
                            {
                                let target = if insert_at > cur { insert_at - 1 } else { insert_at };
                                let t = pane.tabs.remove(cur);
                                let target = target.min(pane.tabs.len());
                                pane.tabs.insert(target, t);
                                pane.active = target;
                            }
                        } else {
                            for pane in self.panes.values_mut() {
                                pane.remove(dragged);
                            }
                            if let Some(pane) = self.panes.get_mut(&pane_id) {
                                let at = insert_at.min(pane.tabs.len());
                                pane.tabs.insert(at, dragged);
                                pane.active = at;
                            }
                            self.active_pane = pane_id;
                        }
                        self.dragging_tab = None;
                    }
                } else if released {
                    // Released somewhere else: a later pane may claim it this frame; otherwise it is cleared at frame end.
                }
                if self.dragging_tab.is_some() {
                    let title = self.views.get(&dragged).and_then(|v| self.docs.get(&v.doc)).map(|d| d.title()).unwrap_or_default();
                    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tab-drag"))).text(
                        p + vec2(12.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        title,
                        font.clone(),
                        theme.ui.text,
                    );
                }
            }
        }
        if let Some(v) = to_activate {
            self.active_pane = pane_id;
            self.focus_view(v);
            ui.ctx().memory_mut(|m| m.request_focus(v.egui_id()));
        }
        if let Some(v) = to_close {
            self.active_pane = pane_id;
            self.request_close(vec![v], AfterClose::CloseViews);
        }
        if let Some((v, c)) = ctx_cmd {
            self.active_pane = pane_id;
            self.focus_view(v);
            self.queue.push_back((c, None));
        }
    }

    pub(crate) fn show_status_bar(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.ui.status_bar);
        ui.painter().hline(rect.x_range(), rect.min.y + 0.5, egui::Stroke::new(1.0, theme.ui.border));
        let font = egui::FontId::proportional(self.settings.ui_font_size - 1.0);
        let mut left = String::new();
        let mut right: Vec<String> = Vec::new();
        if let Some((v, d)) = self.active_ids() {
            let view = &self.views[&v];
            let doc = &self.docs[&d];
            let (line, col) = view.primary_position(doc);
            let display_col = apex_buffer::position::char_col_to_display_col(doc.rope().line(line), col, self.settings.tab_width);
            left = format!("Line {}, Column {}", line + 1, display_col + 1);
            let summary = editor::selection_summary(doc, &view.sels);
            if !summary.is_empty() {
                left.push_str("    ");
                left.push_str(&summary);
            }
            if view.overwrite {
                left.push_str("    OVR");
            }
            if let Some(v) = self.vim_status_label() {
                left = format!("{v}    {left}");
            }
            if !self.keymap.pending().is_empty() {
                left.push_str(&format!("    {} …", self.keymap.pending().iter().map(|k| k.to_string()).collect::<Vec<_>>().join(" ")));
            }
            right.push(doc.indent.label());
            right.push(doc.encoding.label());
            right.push(if doc.mixed_line_endings { format!("{} (mixed)", doc.line_ending.label()) } else { doc.line_ending.label().to_string() });
            right.push(if doc.language_override.as_deref() == Some("hex") { "Hex".to_string() } else { self.language_of(d).name.to_string() });
            if doc.read_only {
                right.push("Read-only".into());
            }
            if self.font_zoom != 0 {
                right.push(format!("{}pt", self.settings.font_size + self.font_zoom as f32));
            }
        }
        if let Some(b) = self.git_branch_label() {
            right.insert(0, format!("git: {b}"));
        }
        if self.show_frame_time {
            let dt = ui.input(|i| i.stable_dt);
            right.push(format!("{:.1} ms", dt * 1000.0));
        }
        if let Some(l) = self.lsp_status_label() {
            right.insert(0, l);
        }
        if let Some(l) = self.spell_status_label() {
            right.insert(0, l);
        }
        let problems = self.all_problems();
        let errors = problems.iter().filter(|p| p.is_error).count();
        if !problems.is_empty() {
            right.insert(0, format!("{errors} errors, {} warnings", problems.len() - errors));
        }
        if self.tasks.running.is_some() {
            left.push_str("    Running task…");
        }
        if let Some(g) = self.git_ops_status() {
            left.push_str(&format!("    {g}"));
        }
        if self.macro_recording.is_some() {
            left.push_str("    ● REC");
        }
        let painter = ui.painter_at(rect);
        let mut x = rect.min.x + 12.0;
        let status_colour = theme.ui.text_muted;
        if let Some((msg, at, is_error)) = &self.status
            && at.elapsed().as_secs() < 8
        {
            let c = if *is_error { theme.ui.error } else { theme.ui.text };
            let g = painter.layout_no_wrap(msg.clone(), font.clone(), c);
            painter.galley(pos2(x, rect.center().y - g.rect.height() / 2.0), g.clone(), c);
            x += g.rect.width() + 24.0;
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        painter.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, left, font.clone(), status_colour);
        let mut rx = rect.max.x - 12.0;
        let mut q = Vec::new();
        for (i, item) in right.iter().enumerate().rev() {
            let g = painter.layout_no_wrap(item.clone(), font.clone(), status_colour);
            let w = g.rect.width();
            let r = Rect::from_min_size(pos2(rx - w - 8.0, rect.min.y), vec2(w + 16.0, rect.height()));
            let resp = ui.interact(r, ui.id().with(("status", i)), egui::Sense::click());
            if resp.hovered() {
                painter.rect_filled(r, 0.0, theme.ui.hover);
            }
            painter.galley(pos2(rx - w, rect.center().y - g.rect.height() / 2.0), g, status_colour);
            let kind = if item.starts_with("Spaces") || item.starts_with("Tabs") {
                0
            } else if apex_buffer::Encoding::from_label(item).is_some() || item.starts_with("UTF") {
                1
            } else if LineEnding::from_label(item.split(' ').next().unwrap_or("")).is_some() {
                2
            } else if item.starts_with('⎇') {
                4
            } else if item.contains('✗') {
                5
            } else {
                3
            };
            let _ = i;
            resp.context_menu(|ui| Self::status_item_menu(ui, kind, &mut q));
            rx -= w + 20.0;
        }
        for c in q {
            self.queue.push_back(c);
        }
    }

    pub(crate) fn status_item_menu(ui: &mut Ui, index: usize, q: &mut Vec<(Command, Option<String>)>) {
        match index {
            0 => {
                for c in [Command::ConvertIndentToSpaces, Command::ConvertIndentToTabs] {
                    if ui.button(c.name()).clicked() {
                        q.push((c, None));
                        ui.close();
                    }
                }
            }
            1 => {
                ui.label("Save with encoding");
                for enc in Encoding::all() {
                    if ui.button(enc.label()).clicked() {
                        q.push((Command::SetEncoding, Some(enc.id())));
                        ui.close();
                    }
                }
            }
            2 => {
                for le in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
                    if ui.button(le.label()).clicked() {
                        q.push((Command::SetLineEnding, Some(le.label().into())));
                        ui.close();
                    }
                }
            }
            4 => {
                for c in [
                    Command::GitStatusPanel,
                    Command::NextChange,
                    Command::PrevChange,
                    Command::CompareWithIndex,
                    Command::CompareWithHead,
                    Command::FileHistory,
                ] {
                    if ui.button(c.name()).clicked() {
                        q.push((c, None));
                        ui.close();
                    }
                }
            }
            5 => {
                for c in [Command::ShowProblems, Command::NextDiagnostic, Command::PrevDiagnostic] {
                    if ui.button(c.name()).clicked() {
                        q.push((c, None));
                        ui.close();
                    }
                }
            }
            _ => {
                ui.label("Set syntax");
                if ui.button("Auto-detect").clicked() {
                    q.push((Command::SetLanguage, Some("auto".into())));
                    ui.close();
                }
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for l in apex_syntax::languages().iter().filter(|l| l.id != "markdown_inline" && l.id != "regex") {
                        if ui.button(l.name).clicked() {
                            q.push((Command::SetLanguage, Some(l.id.to_string())));
                            ui.close();
                        }
                    }
                });
            }
        }
    }

    pub(crate) fn show_find_bar(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        egui::Frame::new().fill(theme.ui.panel).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let needle_id = egui::Id::new("apex-find-needle");
                let resp = ui.add(egui::TextEdit::singleline(&mut self.find.needle).id(needle_id).hint_text("Find").desired_width(260.0).lock_focus(true));
                if self.find.focus_needle {
                    resp.request_focus();
                    self.find.focus_needle = false;
                }
                if resp.changed() {
                    self.update_matches();
                    // Incremental: select the first match after the caret without moving focus.
                    if let Some((v, _)) = self.active_ids() {
                        let head = self.views[&v].sels.primary().start();
                        let first_after = self.find.matches.iter().position(|m| m.start >= head);
                        if let Some(i) = first_after.or(if self.find.matches.is_empty() { None } else { Some(0) }) {
                            self.find.current = Some(i);
                            let r = self.find.matches[i].clone();
                            if let Some(view) = self.views.get_mut(&v) {
                                view.sels.set_single(Selection::new(r.start, r.end));
                                view.want_cursor_visible = true;
                            }
                        }
                    }
                }
                let toggle = |ui: &mut Ui, on: &mut bool, label: &str, tip: &str| {
                    if ui.selectable_label(*on, label).on_hover_text(tip).clicked() {
                        *on = !*on;
                    }
                };
                toggle(ui, &mut self.find.options.regex, ".*", "Regular expression (Alt+R)");
                toggle(ui, &mut self.find.options.case_sensitive, "Aa", "Case sensitive (Alt+C)");
                toggle(ui, &mut self.find.options.whole_word, "Word", "Whole word (Alt+W)");
                let mut in_sel = self.find.options.in_range.is_some();
                if ui.selectable_label(in_sel, "In selection").clicked() {
                    in_sel = !in_sel;
                    self.find.options.in_range = if in_sel {
                        self.active_ids().map(|(v, _)| self.views[&v].sels.primary()).filter(|s| !s.is_empty()).map(|s| (s.start(), s.end()))
                    } else {
                        None
                    };
                }
                if ui.button("Previous").clicked() {
                    self.queue.push_back((Command::FindPrev, None));
                }
                if ui.button("Next").clicked() {
                    self.queue.push_back((Command::FindNext, None));
                }
                self.update_matches();
                let count_text = if let Some(e) = &self.find.error {
                    egui::RichText::new(e.clone()).color(theme.ui.error)
                } else if self.find.needle.is_empty() {
                    egui::RichText::new("")
                } else if self.find.matches.is_empty() {
                    egui::RichText::new("No results").color(theme.ui.warning)
                } else {
                    let cur = self.find.current.map(|i| i + 1).unwrap_or(0);
                    egui::RichText::new(format!("{cur} of {}", self.find.matches.len())).color(theme.ui.text_muted)
                };
                ui.label(count_text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("×").clicked() {
                        self.queue.push_back((Command::CloseFind, None));
                    }
                    if ui.selectable_label(self.find.replace_open, "Replace").clicked() {
                        self.find.replace_open = !self.find.replace_open;
                    }
                });
            });
            if self.find.replace_open {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.find.replacement)
                            .id(egui::Id::new("apex-find-replace"))
                            .hint_text("Replace")
                            .desired_width(260.0)
                            .lock_focus(true),
                    );
                    if ui.button("Replace").clicked() {
                        self.queue.push_back((Command::ReplaceNext, None));
                    }
                    if ui.button("Replace All").clicked() {
                        self.queue.push_back((Command::ReplaceAll, None));
                    }
                    if self.find.options.regex {
                        ui.label(egui::RichText::new("$1 … $9 insert capture groups").color(theme.ui.text_muted));
                    }
                });
            }
        });
    }

    pub(crate) fn show_dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.clone() else { return };
        let mut close = false;
        if matches!(
            dialog,
            Dialog::ConfirmDelete(_)
                | Dialog::ConfirmReplaceInFiles
                | Dialog::Prompt { .. }
                | Dialog::Recovery(_)
                | Dialog::Info { .. }
                | Dialog::ConfirmRevertHunk { .. }
                | Dialog::Trust { .. }
                | Dialog::ConfirmWorkspaceEdit { .. }
                | Dialog::Confirm { .. }
        ) {
            self.show_workspace_dialog(ctx, dialog);
            return;
        }
        match dialog {
            Dialog::ConfirmClose { views, then, index } => {
                let Some(&vid) = views.get(index) else {
                    self.dialog = None;
                    self.finish_close(views, then);
                    return;
                };
                let title = self.views.get(&vid).and_then(|v| self.docs.get(&v.doc)).map(|d| d.title()).unwrap_or_default();
                let unique_dirty = self.views.get(&vid).is_some_and(|view| {
                    self.views.values().filter(|x| x.doc == view.doc).count() == 1 && self.docs.get(&view.doc).is_some_and(|d| d.is_dirty())
                });
                if !unique_dirty {
                    self.dialog = Some(Dialog::ConfirmClose { views, then, index: index + 1 });
                    return;
                }
                let r = egui::Modal::new(egui::Id::new("confirm-close")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.heading(format!("Save changes to {title}?"));
                    ui.label("Your changes will be lost if you don't save them.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            Some(0)
                        } else if ui.button("Don't Save").clicked() {
                            Some(1)
                        } else if ui.button("Cancel").clicked() {
                            Some(2)
                        } else {
                            None
                        }
                    })
                    .inner
                });
                let choice = r.inner.or_else(|| {
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        Some(2)
                    } else if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        Some(0)
                    } else {
                        None
                    }
                });
                match choice {
                    Some(0) => {
                        let untitled = self.views.get(&vid).and_then(|v| self.docs.get(&v.doc)).is_some_and(|d| d.path.is_none());
                        if untitled {
                            // Ask for a name; the close sequence resumes after the save.
                            self.dialog = None;
                            self.save_view_as_then(vid, Some(Dialog::ConfirmClose { views, then, index: index + 1 }));
                            return;
                        }
                        if self.save_view(vid, false) {
                            self.dialog = Some(Dialog::ConfirmClose { views, then, index: index + 1 });
                        } else if !matches!(self.dialog, Some(Dialog::SaveConflict { .. })) {
                            self.dialog = None;
                        }
                    }
                    Some(1) => self.dialog = Some(Dialog::ConfirmClose { views, then, index: index + 1 }),
                    Some(2) => self.dialog = None,
                    _ => {}
                }
                if r.should_close() {
                    self.dialog = None;
                }
                return;
            }
            Dialog::ExternalChange { doc } => {
                let title = self.docs.get(&doc).map(|d| d.title()).unwrap_or_default();
                let r = egui::Modal::new(egui::Id::new("external-change")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading(format!("{title} has changed on disk"));
                    ui.label("You have unsaved edits in the editor. What would you like to do?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Reload from disk").on_hover_text("Discard the editor's changes (undo is still available)").clicked() {
                            1
                        } else if ui.button("Keep my changes").on_hover_text("Saving will then overwrite the file on disk").clicked() {
                            2
                        } else if ui.button("Save As…").clicked() {
                            3
                        } else {
                            0
                        }
                    })
                    .inner
                });
                let view = self.views.values().find(|v| v.doc == doc).map(|v| v.id);
                match r.inner {
                    1 => {
                        if let Some(v) = view {
                            self.reload_view(v, None);
                        }
                        close = true;
                    }
                    2 => {
                        if let Some(d) = self.docs.get_mut(&doc)
                            && let Some(p) = &d.path
                        {
                            d.file_identity = fileio::identity(p);
                        }
                        close = true;
                    }
                    3 => {
                        self.dialog = None;
                        if let Some(v) = view {
                            self.save_view_as(v);
                        }
                        return;
                    }
                    _ => {}
                }
                if r.should_close() {
                    close = true;
                }
            }
            Dialog::SaveConflict { view } => {
                let r = egui::Modal::new(egui::Id::new("save-conflict")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading("The file has changed on disk since it was opened");
                    ui.label("Overwriting will discard whatever another program wrote.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Overwrite").clicked() {
                            1
                        } else if ui.button("Save As…").clicked() {
                            2
                        } else if ui.button("Cancel").clicked() {
                            3
                        } else {
                            0
                        }
                    })
                    .inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.save_view(view, true);
                        return;
                    }
                    2 => {
                        self.dialog = None;
                        self.save_view_as(view);
                        return;
                    }
                    3 => close = true,
                    _ => {}
                }
                if r.should_close() {
                    close = true;
                }
            }
            Dialog::GotoLine { mut text } => {
                let mut go = false;
                let mut changed = false;
                let r = egui::Modal::new(egui::Id::new("goto-line")).show(ctx, |ui| {
                    ui.set_width(300.0);
                    ui.label("Go to line (or line:column)");
                    let resp = ui.add(egui::TextEdit::singleline(&mut text).hint_text("e.g. 120 or 120:8").desired_width(f32::INFINITY));
                    resp.request_focus();
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    changed = resp.changed();
                });
                if (changed || go)
                    && let (Some((v, _)), Some((l, c))) = (self.active_ids(), parse_line_col(&text))
                {
                    self.goto_line_col(v, l, c);
                }
                if go || r.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                    self.focus_active_editor(ctx);
                } else {
                    self.dialog = Some(Dialog::GotoLine { text });
                }
            }
            Dialog::About => {
                let r = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.heading(format!("{} {}", apex_project::paths::APP_NAME, env!("CARGO_PKG_VERSION")));
                    ui.label("A native, high-performance text editor.");
                    ui.add_space(4.0);
                    ui.label("Licensed under the GNU Affero General Public License v3.0 or later.");
                    ui.label(format!("Configuration: {}", self.config_dir.display()));
                    ui.add_space(8.0);
                    ui.button("Close").clicked()
                });
                if r.inner || r.should_close() {
                    close = true;
                }
            }
            Dialog::ConfirmDelete(_)
            | Dialog::ConfirmReplaceInFiles
            | Dialog::Prompt { .. }
            | Dialog::Recovery(_)
            | Dialog::Info { .. }
            | Dialog::ConfirmRevertHunk { .. }
            | Dialog::Trust { .. }
            | Dialog::ConfirmWorkspaceEdit { .. }
            | Dialog::Confirm { .. } => {}
            Dialog::Issues(issues) => {
                let r = egui::Modal::new(egui::Id::new("issues")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.heading("Configuration problems");
                    ui.label("Apex started with defaults for the affected settings.");
                    ui.add_space(4.0);
                    for i in &issues {
                        ui.label(format!("• {}: {}", i.source, i.message));
                    }
                    ui.add_space(8.0);
                    ui.button("OK").clicked()
                });
                if r.inner || r.should_close() {
                    close = true;
                }
            }
        }
        if close {
            self.dialog = None;
        }
    }

    pub(crate) fn show_editor_context_menu(&mut self, ctx: &egui::Context) {
        let Some((vid, pos)) = self.context_menu_at else { return };
        let id = egui::Id::new(("editor-context", vid.0));
        let mut cmd = None;
        let spelling = self.views.get(&vid).map(|v| (v.doc, v.sels.primary().head)).and_then(|(d, head)| self.spell_suggestions_at(d, head));
        let mut spell_action: Option<(usize, usize, String, Option<String>)> = None;
        let muted = self.theme.ui.text_muted;
        let in_repo = self.views.get(&vid).map(|v| v.doc).and_then(|d| self.git_workdir_for_doc(d)).is_some();
        let shortcuts: Vec<(Command, String)> = [
            Command::Cut,
            Command::Copy,
            Command::Paste,
            Command::SelectAll,
            Command::ToggleComment,
            Command::DuplicateLine,
            Command::DeleteLine,
            Command::ToggleBookmark,
            Command::CopyFilePath,
            Command::GitStageHunk,
            Command::GitStageLines,
            Command::RevertHunk,
        ]
        .into_iter()
        .filter(|c| in_repo || c.category() != crate::command::Category::Git)
        .map(|c| (c, self.keymap.shortcut_for(c).unwrap_or_default()))
        .collect();
        let popup = egui::Popup::new(id, ctx.clone(), egui::PopupAnchor::Position(pos), egui::LayerId::new(egui::Order::Foreground, id))
            .open_memory(Some(egui::SetOpenCommand::Bool(true)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                if let Some((a, b, word, suggestions)) = &spelling {
                    ui.label(egui::RichText::new(format!("“{word}” is not in the dictionary")).color(muted));
                    for sg in suggestions {
                        if ui.button(sg).clicked() {
                            spell_action = Some((*a, *b, word.clone(), Some(sg.clone())));
                        }
                    }
                    if ui.button("Add to dictionary").clicked() {
                        spell_action = Some((*a, *b, word.clone(), Some(String::new())));
                    }
                    if ui.button("Ignore this session").clicked() {
                        spell_action = Some((*a, *b, word.clone(), None));
                    }
                    ui.separator();
                }
                for (c, s) in &shortcuts {
                    if ui.add(egui::Button::new(c.name()).shortcut_text(s)).clicked() {
                        cmd = Some(*c);
                    }
                }
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed()) && !popup.as_ref().is_some_and(|p| p.response.contains_pointer());
        if let Some((a, b, word, action)) = spell_action {
            match action {
                Some(s) if s.is_empty() => self.spell_add_word(&word),
                Some(s) => self.spell_replace(vid, a, b, &s),
                None => self.spell_ignore_word(&word),
            }
            self.context_menu_at = None;
            egui::Popup::close_id(ctx, id);
            return;
        }
        if let Some(c) = cmd {
            self.queue.push_back((c, None));
            self.context_menu_at = None;
            egui::Popup::close_id(ctx, id);
        } else if clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.context_menu_at = None;
            egui::Popup::close_id(ctx, id);
        }
    }

    pub(crate) fn show_pane(&mut self, ui: &mut Ui, pane_id: PaneId, rect: Rect) {
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        ui.set_clip_rect(rect);
        let mut inner = rect;
        if self.settings.tabs && !self.distraction_free {
            self.show_tabs(&mut ui, pane_id);
            inner.min.y += 30.0;
        }
        let Some(vid) = self.panes.get(&pane_id).and_then(|p| p.active_view()) else {
            let p = ui.painter_at(inner);
            p.rect_filled(inner, 0.0, self.theme.editor.background);
            p.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "No file open in this pane",
                egui::FontId::proportional(self.settings.ui_font_size),
                self.theme.ui.text_muted,
            );
            let r = ui.interact(inner, ui.id().with(("emptypane", pane_id.0)), egui::Sense::click());
            if r.clicked() {
                self.active_pane = pane_id;
            }
            return;
        };
        let font = editor::font_for(&self.settings, self.font_zoom);
        let (line_height, char_width) = editor::metrics(ui.ctx(), &font, self.settings.line_height);
        if self.distraction_free {
            let width = (self.settings.distraction_free_width * char_width).min(inner.width());
            let cx = inner.center().x;
            ui.painter().rect_filled(rect, 0.0, self.theme.editor.background);
            inner = Rect::from_min_max(pos2(cx - width / 2.0 - 60.0, inner.min.y), pos2(cx + width / 2.0, inner.max.y));
        }
        let focused = pane_id == self.active_pane;
        let Some(did) = self.views.get(&vid).map(|v| v.doc) else { return };
        let matches: Vec<std::ops::Range<usize>> =
            if self.find.open && self.find.rev.as_ref().is_some_and(|r| r.0 == did) { self.find.matches.clone() } else { Vec::new() };
        let current_match = self.find.current;
        let settings = self.settings.clone();
        let theme = self.theme.clone();
        self.syntax_tick(did);
        let fold_regions = self.fold_regions_for(did);
        let git_marks = self.git_marks_for(did);
        let mut diagnostics = self.diagnostics_for(did);
        if self.spell_enabled_for(did) {
            let (first, count) = {
                let v = &self.views[&vid];
                (v.top_line, v.lines_per_page + 2)
            };
            diagnostics.extend(self.spell_ranges(did, first, first + count).into_iter().map(|(a, b)| (a, b, crate::spell_ui::SPELL_SEVERITY)));
        }
        let (wrap, large) = {
            let view = &self.views[&vid];
            let doc = &self.docs[&did];
            (view.word_wrap.unwrap_or(self.settings.word_wrap) && !doc.large_file, doc.large_file)
        };
        let style = EditorStyle {
            theme: &theme,
            settings: &settings,
            font_id: font,
            line_height,
            char_width,
            focused,
            matches: &matches,
            current_match,
            git_marks: &git_marks,
            diagnostics: &diagnostics,
            fold_regions: &fold_regions,
            wrap,
            large_file: large,
            block_caret: self.vim_block_caret(),
        };
        let muted = settings.muted_syntax || settings.low_distraction;
        let mut actions = Vec::new();
        {
            let Some(doc) = self.docs.get(&did) else { return };
            let syntax_cell = std::cell::RefCell::new(self.syntax.get_mut(&did));
            let highlight = make_highlight(&syntax_cell, doc, &theme, muted);
            let view = self.views.get_mut(&vid).expect("view");
            view.sync(doc);
            editor::input(&mut ui, inner, view, doc, &style, &mut self.keymap, Some(&highlight), &mut actions);
        }
        for a in actions {
            match a {
                Action::Focus => self.active_pane = pane_id,
                Action::Insert(t) | Action::ImeCommit(t) => self.insert_text(vid, &t),
                Action::Paste(t) => {
                    let mode = self.pending_paste_mode.take().unwrap_or_default();
                    self.paste_text(vid, t, mode == "indent");
                }
                Action::Command(c, arg) => self.execute(ui.ctx(), c, arg),
                Action::Dropped(paths) => {
                    for p in paths {
                        if p.is_dir() {
                            self.add_root(&p);
                        } else {
                            self.open_path(&p);
                        }
                    }
                }
                Action::ContextMenu(p) => self.context_menu_at = Some((vid, p)),
                Action::ToggleFold(line) => {
                    self.active_pane = pane_id;
                    self.fold_command(Command::ToggleFold, Some(line));
                }
                Action::Hover(pos) => {
                    self.hover_pos = pos.map(|p| (vid, p));
                    self.last_pointer = ui.ctx().input(|i| i.pointer.latest_pos());
                    self.lsp_hover_moved(vid, pos);
                }
            }
        }
        // The view may have been closed by a command; the document may have changed.
        if !self.views.contains_key(&vid) || !self.docs.contains_key(&did) {
            return;
        }
        self.syntax_tick(did);
        let fold_regions = self.fold_regions_for(did);
        let style = EditorStyle { fold_regions: &fold_regions, ..style };
        let doc = self.docs.get(&did).expect("doc");
        let syntax_cell = std::cell::RefCell::new(self.syntax.get_mut(&did));
        let highlight = make_highlight(&syntax_cell, doc, &theme, muted);
        let view = self.views.get_mut(&vid).expect("view");
        view.sync(doc);
        editor::paint(&mut ui, inner, view, doc, &style, Some(&highlight));
    }

    /// Git gutter marks for a document.
    pub(crate) fn git_marks_for(&mut self, d: DocId) -> Vec<(usize, u8)> {
        self.git_marks(d)
    }

    /// Problems from language servers (filled by the LSP integration).
    pub(crate) fn lsp_problems(&self) -> Vec<apex_project::tasks::Problem> {
        self.lsp_problem_list()
    }

    /// Diagnostic underlines for a document (filled by the LSP integration).
    pub(crate) fn diagnostics_for(&self, d: DocId) -> Vec<(usize, usize, u8)> {
        self.lsp_diagnostics_for(d)
    }

    pub(crate) fn show_layout(&mut self, ui: &mut Ui, layout: Layout, rect: Rect, path: Vec<usize>) {
        match layout {
            Layout::Leaf(p) => self.show_pane(ui, p, rect),
            Layout::Split { axis, children, fractions } => {
                let n = children.len();
                let gap = 4.0;
                let total = match axis {
                    Axis::Horizontal => rect.width(),
                    Axis::Vertical => rect.height(),
                } - gap * (n as f32 - 1.0);
                let mut offset = 0.0;
                let mut new_fracs = fractions.clone();
                for (i, child) in children.into_iter().enumerate() {
                    let size = total * fractions[i];
                    let child_rect = match axis {
                        Axis::Horizontal => Rect::from_min_size(pos2(rect.min.x + offset, rect.min.y), vec2(size, rect.height())),
                        Axis::Vertical => Rect::from_min_size(pos2(rect.min.x, rect.min.y + offset), vec2(rect.width(), size)),
                    };
                    let mut child_path = path.clone();
                    child_path.push(i);
                    self.show_layout(ui, child, child_rect, child_path);
                    offset += size;
                    if i + 1 < n {
                        let div = match axis {
                            Axis::Horizontal => Rect::from_min_size(pos2(rect.min.x + offset, rect.min.y), vec2(gap, rect.height())),
                            Axis::Vertical => Rect::from_min_size(pos2(rect.min.x, rect.min.y + offset), vec2(rect.width(), gap)),
                        };
                        ui.painter().rect_filled(div, 0.0, self.theme.ui.border);
                        let id = ui.id().with(("divider", &path, i));
                        let resp = ui.interact(div.expand(2.0), id, egui::Sense::drag());
                        if resp.hovered() || resp.dragged() {
                            ui.ctx().set_cursor_icon(match axis {
                                Axis::Horizontal => egui::CursorIcon::ResizeHorizontal,
                                Axis::Vertical => egui::CursorIcon::ResizeVertical,
                            });
                        }
                        if resp.dragged() {
                            let d = match axis {
                                Axis::Horizontal => resp.drag_delta().x,
                                Axis::Vertical => resp.drag_delta().y,
                            } / total;
                            let min = 0.1;
                            let d = d.clamp(-(new_fracs[i] - min).max(0.0), (new_fracs[i + 1] - min).max(0.0));
                            new_fracs[i] += d;
                            new_fracs[i + 1] -= d;
                        }
                        offset += gap;
                    }
                }
                if new_fracs != fractions {
                    set_fractions(&mut self.layout, &path, new_fracs);
                }
            }
        }
    }

    pub(crate) fn update_title(&self, ctx: &egui::Context) {
        let title = match self.active_ids() {
            Some((_, d)) => {
                let doc = &self.docs[&d];
                let mark = if doc.is_dirty() { " •" } else { "" };
                match &doc.path {
                    Some(p) => format!(
                        "{}{mark} ({}) — {}",
                        doc.title(),
                        p.parent().map(|x| x.display().to_string()).unwrap_or_default(),
                        apex_project::paths::APP_NAME
                    ),
                    None => format!("{}{mark} — {}", doc.title(), apex_project::paths::APP_NAME),
                }
            }
            None => apex_project::paths::APP_NAME.to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    pub(crate) fn apply_fonts(&mut self, ctx: &egui::Context) {
        if self.fonts_applied {
            return;
        }
        self.fonts_applied = true;
        ctx.set_zoom_factor(self.settings.ui_scale);
        let ui_size = self.settings.ui_font_size;
        ctx.all_styles_mut(|style| {
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles = [
                (TextStyle::Small, FontId::new(ui_size - 2.0, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(ui_size, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(ui_size, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(ui_size + 4.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(ui_size, FontFamily::Monospace)),
            ]
            .into();
        });
    }
}

/// Send plain text to the default printer. Returns the program used.
fn print_text(title: &str, text: &str) -> std::io::Result<&'static str> {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("apex-print-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let safe: String = title.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let path = dir.join(format!("{safe}.txt"));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(text.as_bytes())?;
    f.flush()?;
    let try_run = |program: &'static str, args: &[&str]| -> Option<&'static str> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args).arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match cmd.status() {
            Ok(st) if st.success() => Some(program),
            _ => None,
        }
    };
    let via = if cfg!(windows) {
        let script = format!("Get-Content -LiteralPath '{}' | Out-Printer", path.display().to_string().replace('\'', "''"));
        let st = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).status();
        match st {
            Ok(s) if s.success() => Some("Out-Printer"),
            _ => None,
        }
    } else {
        try_run("lp", &["-t", title]).or_else(|| try_run("lpr", &["-T", title]))
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    via.ok_or_else(|| std::io::Error::other("no printing command found (lp or lpr on Unix, PowerShell Out-Printer on Windows) or the printer rejected the job"))
}

/// Build the lazy per-line highlighter used by the editor widget.
fn make_highlight<'a>(
    syntax: &'a std::cell::RefCell<Option<&'a mut SyntaxState>>,
    doc: &'a Document,
    theme: &'a Theme,
    muted: bool,
) -> impl Fn(usize) -> Vec<(usize, usize, crate::theme::SyntaxStyle)> + 'a {
    move |line: usize| {
        let mut guard = syntax.borrow_mut();
        let Some(st) = guard.as_mut() else { return Vec::new() };
        let fg = theme.editor.foreground;
        st.spans_for_line(doc, line)
            .iter()
            .filter_map(|sp| {
                let mut style = theme.syntax_style(sp.scope)?;
                if muted {
                    style.fg = egui::Color32::from_rgb(
                        ((style.fg.r() as u16 + fg.r() as u16 * 2) / 3) as u8,
                        ((style.fg.g() as u16 + fg.g() as u16 * 2) / 3) as u8,
                        ((style.fg.b() as u16 + fg.b() as u16 * 2) / 3) as u8,
                    );
                }
                Some((sp.start, sp.end, style))
            })
            .collect()
    }
}

fn set_fractions(layout: &mut Layout, path: &[usize], fracs: Vec<f32>) {
    let mut node = layout;
    for &i in path {
        node = match node {
            Layout::Split { children, .. } => &mut children[i],
            Layout::Leaf(_) => return,
        };
    }
    if let Layout::Split { fractions, .. } = node {
        *fractions = fracs;
    }
}

impl eframe::App for ApexApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_fonts(&ctx);

        // Global key handling for non-editor contexts (menus, find bar).
        let editor_focused = self.active_view_id().is_some_and(|v| ctx.memory(|m| m.has_focus(v.egui_id()))) || self.palette.open;
        // egui drops a text field's focus on Escape before we run, so treat the
        // find bar as focused whenever it is open and the editor is not.
        let find_focused = self.find.open && !editor_focused && self.dialog.is_none();
        if !editor_focused {
            let active_ctx =
                if find_focused { vec![crate::keymap::Context::Find, crate::keymap::Context::Global] } else { vec![crate::keymap::Context::Global] };
            let events = ctx.input(|i| i.events.clone());
            for ev in events {
                if let egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } = ev
                    && let crate::keymap::Dispatch::Command(c, arg) = self.keymap.dispatch_event(key, physical_key, modifiers, &active_ctx)
                {
                    let plain = !modifiers.ctrl && !modifiers.alt && !modifiers.command && !modifiers.mac_cmd;
                    if !(plain && find_focused && !matches!(key, egui::Key::Enter | egui::Key::Escape | egui::Key::F3)) {
                        self.queue.push_back((c, arg));
                        // Stop the text field from also receiving the key's character (Alt+R → "r").
                        let name = key.symbol_or_name().to_string();
                        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t.eq_ignore_ascii_case(&name))));
                    }
                }
            }
        }
        if self.settings.vim_mode
            && editor_focused
            && !self.palette.open
            && self.dialog.is_none()
            && self.lsp.completion.is_none()
            && self.snippet_session.is_none()
        {
            self.vim_input(&ctx);
        }
        self.poll_inbox(&ctx);
        self.poll_file_dialog(&ctx);
        self.follow_system_theme(&ctx);
        if self.lsp_completion_key(&ctx) || self.snippet_key(&ctx) {
            ctx.request_repaint();
        }
        if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true)))) || self.last_focus_check.elapsed().as_secs() >= 2 {
            self.last_focus_check = Instant::now();
            self.check_external_changes();
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            let dirty = self.docs.values().any(|d| d.is_dirty());
            if dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                let all: Vec<ViewId> = self.views.keys().copied().collect();
                self.request_close(all, AfterClose::Quit);
            }
        }
        if self.quit_confirmed {
            self.save_session();
            self.write_recovery_snapshots();
            self.lsp_shutdown_all();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.poll_background(&ctx);
        if self.lsp_tick(&ctx) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            if p.is_dir() {
                self.add_root(&p);
            } else {
                self.open_path(&p);
            }
        }

        while let Some((c, arg)) = self.queue.pop_front() {
            self.execute(&ctx, c, arg);
        }

        let theme_bg = self.theme.ui.background;
        if self.settings.menu_bar && !self.distraction_free {
            egui::Panel::top("apex-menu").frame(egui::Frame::new().fill(theme_bg).inner_margin(egui::Margin::symmetric(4, 2))).show(ui, |ui| {
                self.show_menu_bar(ui);
            });
        }
        if self.settings.status_bar && !self.distraction_free {
            egui::Panel::bottom("apex-status").frame(egui::Frame::new().fill(self.theme.ui.status_bar)).exact_size(24.0).show(ui, |ui| {
                self.show_status_bar(ui);
            });
        }
        if self.find.open {
            egui::Panel::bottom("apex-find").frame(egui::Frame::new().fill(self.theme.ui.panel)).show(ui, |ui| {
                self.show_find_bar(ui);
            });
        }
        if self.panel_open && !self.distraction_free {
            let panel_bg = self.theme.ui.panel;
            egui::Panel::bottom("apex-panel")
                .resizable(true)
                .default_size(260.0)
                .min_size(120.0)
                .frame(egui::Frame::new().fill(panel_bg).inner_margin(egui::Margin::symmetric(8, 6)))
                .show(ui, |ui| {
                    self.show_bottom_panel(ui);
                });
        }
        if self.settings.sidebar && !self.distraction_free {
            let sidebar_bg = self.theme.ui.sidebar;
            let width = self.sidebar.width;
            let resp = egui::Panel::left("apex-sidebar")
                .resizable(true)
                .default_size(width)
                .min_size(120.0)
                .max_size(800.0)
                .frame(egui::Frame::new().fill(sidebar_bg))
                .show(ui, |ui| {
                    self.show_sidebar(ui);
                });
            self.sidebar.width = resp.response.rect.width();
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme_bg)).show(ui, |ui| {
            let rect = ui.available_rect_before_wrap();
            let layout = self.layout.clone();
            self.show_layout(ui, layout, rect, Vec::new());
        });
        while let Some((c, arg)) = self.queue.pop_front() {
            self.execute(&ctx, c, arg);
            ctx.request_repaint();
        }
        if self.dragging_tab.is_some() && ctx.input(|i| i.pointer.primary_released() || !i.pointer.any_down()) {
            self.dragging_tab = None;
        }
        self.lsp_overlays(&ctx);
        self.show_settings_panel(&ctx);
        self.show_palette(&ctx);
        self.show_dialogs(&ctx);
        self.show_editor_context_menu(&ctx);
        self.update_title(&ctx);
        if std::mem::take(&mut self.pending_repaint) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self.dialog.is_none() && !self.find.open && !self.palette.open && ctx.memory(|m| m.focused()).is_none() {
            self.focus_active_editor(&ctx);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.theme.editor.background;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}

pub(crate) fn normalise_path(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf()) };
    // Canonicalise the parent only, so a not-yet-existing file keeps its name
    // and symlinked files keep their link path (we write through links).
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent).map(|c| c.join(name)).unwrap_or(abs),
        _ => abs,
    }
}

pub(crate) fn parse_line_col(s: &str) -> Option<(usize, usize)> {
    let s = s.trim().trim_start_matches(':');
    let mut it = s.split(':');
    let line = it.next()?.trim().parse().ok()?;
    let col = it.next().and_then(|c| c.trim().parse().ok()).unwrap_or(1);
    Some((line, col))
}

pub(crate) fn local_datetime_string() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (y, m, d, hh, mm, ss) = civil_from_unix(now + local_utc_offset_seconds());
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_unix(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (secs / 3600) as u32, ((secs % 3600) / 60) as u32, (secs % 60) as u32)
}

#[cfg(unix)]
fn local_utc_offset_seconds() -> i64 {
    use std::mem::MaybeUninit;
    let now: libc::time_t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    let mut tm = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r writes into the provided struct; both pointers are valid for the call.
    let ok = unsafe { !libc::localtime_r(&now, tm.as_mut_ptr()).is_null() };
    if ok {
        // SAFETY: initialised by localtime_r above.
        unsafe { tm.assume_init().tm_gmtoff }
    } else {
        0
    }
}

#[cfg(not(unix))]
fn local_utc_offset_seconds() -> i64 {
    0
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4);
    out.push_str("Binary file (read-only hexadecimal preview)\n\n");
    for (i, chunk) in bytes.chunks(16).enumerate().take(1 << 16) {
        out.push_str(&format!("{:08x}  ", i * 16));
        for (j, b) in chunk.iter().enumerate() {
            out.push_str(&format!("{b:02x} "));
            if j == 7 {
                out.push(' ');
            }
        }
        for _ in chunk.len()..16 {
            out.push_str("   ");
        }
        if chunk.len() <= 8 {
            out.push(' ');
        }
        out.push_str(" |");
        for &b in chunk {
            out.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        out.push_str("|\n");
    }
    if bytes.len() > 16 << 16 {
        out.push_str("… (preview truncated at 1 MiB)\n");
    }
    out
}

pub(crate) fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let cmd: std::io::Result<std::process::Child> = Err(std::io::Error::other("unsupported platform"));
    if let Err(e) = cmd {
        tracing::warn!("could not reveal {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(crate) fn parses_line_col() {
        assert_eq!(parse_line_col("12"), Some((12, 1)));
        assert_eq!(parse_line_col("12:8"), Some((12, 8)));
        assert_eq!(parse_line_col(":7"), Some((7, 1)));
        assert_eq!(parse_line_col("x"), None);
    }

    #[test]
    pub(crate) fn civil_date() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    }

    #[test]
    pub(crate) fn hex_preview_shape() {
        let p = hex_preview(b"ABC\x00\xff");
        assert!(p.contains("41 42 43 00 ff"));
        assert!(p.contains("|ABC..|"));
    }
}
