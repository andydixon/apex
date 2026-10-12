//! Workspace-level behaviour of the application: folders, sidebar, file index,
//! watcher, palette modes, Find in Files panel, sessions, recovery and macros.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apex_edit::{DocId, Selection, SelectionSet};
use apex_project::index::{FileIndex, WalkOptions};
use apex_project::session::{LayoutState, PaneState, RecoveryEntry, Session, TabState, WindowState};
use apex_project::tree::TreeOptions;
use apex_project::watch::FsWatcher;
use egui::Ui;

use crate::app::{AfterClose, ApexApp, Dialog};
use crate::command::{Category, Command};
use crate::find_in_files::FifAction;
use crate::palette::{PaletteEvent, PaletteItem, PaletteMode, Payload};
use crate::sidebar::SidebarAction;
use crate::view::ViewId;
use crate::workspace::{Axis, Layout, PaneId};

/// One recorded macro step.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MacroStep {
    Command(Command, Option<String>),
    Insert(String),
}

/// What a text prompt dialog is for.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptKind {
    NewFile(PathBuf),
    NewFolder(PathBuf),
    MacroName,
    MarkName(bool),
    Sequence,
    Surround,
    SaveProject,
    Rename,
}

const MAX_JUMPS: usize = 100;

impl ApexApp {
    // ----- roots and index ----------------------------------------------------

    pub(crate) fn walk_options(&self) -> WalkOptions {
        WalkOptions {
            exclude: self.settings.exclude_patterns.clone(),
            hidden: self.settings.show_hidden_files,
            follow_symlinks: self.settings.follow_symlinks,
            respect_gitignore: true,
            max_files: 2_000_000,
        }
    }

    pub(crate) fn tree_options(&self) -> TreeOptions {
        TreeOptions {
            show_hidden: self.settings.show_hidden_files,
            exclude: self.settings.exclude_patterns.clone(),
            follow_symlinks: self.settings.follow_symlinks,
        }
    }

    pub(crate) fn add_root(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !path.is_dir() {
            self.set_status(format!("{} is not a folder", path.display()), true);
            return;
        }
        if !self.roots.contains(&path) {
            self.roots.push(path.clone());
        }
        self.roots_changed();
        self.push_recent_project(&path);
    }

    pub(crate) fn remove_root(&mut self, path: &Path) {
        self.roots.retain(|r| r != path);
        if let Some(w) = &mut self.watcher {
            w.unwatch(path);
        }
        self.roots_changed();
    }

    /// Replace the workspace roots (File ▸ Open Folder).
    pub(crate) fn open_folder(&mut self, path: &Path) {
        self.roots.clear();
        self.project_file = None;
        self.add_root(path);
    }

    pub(crate) fn roots_changed(&mut self) {
        let opts = self.tree_options();
        self.sidebar.opts = opts;
        let roots = self.roots.clone();
        self.sidebar.set_roots(&roots);
        if self.watcher.is_none() {
            match FsWatcher::new() {
                Ok(w) => self.watcher = Some(w),
                Err(e) => tracing::warn!("file watching unavailable: {e}"),
            }
        }
        if let Some(w) = &mut self.watcher {
            for r in &roots {
                w.watch(r);
            }
        }
        self.restart_index();
        self.symbols = Default::default();
        self.symbol_scan = None;
    }

    pub(crate) fn restart_index(&mut self) {
        if self.roots.is_empty() {
            self.index = FileIndex::default();
            self.index_scan = None;
            return;
        }
        let scan = apex_project::index::start_scan(self.roots.clone(), self.walk_options());
        self.index_scan = Some(scan);
    }

    pub(crate) fn push_recent_project(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        self.recent_projects.push_front(path.to_path_buf());
        self.recent_projects.truncate(20);
    }

    /// Relative display path for a file under one of the roots.
    pub(crate) fn display_path(&self, path: &Path) -> String {
        for r in &self.roots {
            if let Ok(rel) = path.strip_prefix(r) {
                let rel = rel.display().to_string();
                if self.roots.len() > 1 {
                    return format!("{}/{rel}", r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                }
                return rel;
            }
        }
        path.display().to_string()
    }

    // ----- background polling ---------------------------------------------------

    /// Drain background channels. Called once per frame before commands run.
    pub(crate) fn poll_background(&mut self, ctx: &egui::Context) {
        let mut repaint = false;
        if let Some(scan) = &self.index_scan {
            if self.index.poll(scan) {
                repaint = true;
                if self.index.complete {
                    tracing::debug!("indexed {} files in {:?}", self.index.files.len(), self.index.last_scan);
                }
            }
            if self.index.complete {
                self.index_scan = None;
            } else {
                repaint = true;
            }
        }
        if let Some(scan) = &self.symbol_scan {
            if self.symbols.poll(scan) {
                repaint = true;
            }
            if self.symbols.complete {
                self.symbol_scan = None;
            } else {
                repaint = true;
            }
        }
        if let Some(w) = &mut self.watcher {
            if let Some(changes) = w.poll(Duration::from_millis(300)) {
                if changes.overflow || changes.paths.len() > 50 {
                    self.sidebar.refresh_all();
                } else {
                    for p in &changes.paths {
                        self.sidebar.refresh_path(p);
                    }
                }
                self.rescan_at = Some(Instant::now() + Duration::from_millis(1500));
                self.check_external_changes();
                self.git_mark_dirty();
                repaint = true;
            } else if w.has_pending() {
                repaint = true;
            }
        }
        if self.rescan_at.is_some_and(|t| Instant::now() >= t) {
            self.rescan_at = None;
            self.restart_index();
        }
        if self.fif.poll() {
            repaint = true;
        }
        if self.git_poll() {
            repaint = true;
        }
        if self.git.has_repos() && self.last_git_tick.elapsed() >= Duration::from_secs(4) {
            self.last_git_tick = Instant::now();
            self.git_mark_dirty_status_only();
            repaint = true;
        }
        if self.tasks_poll() {
            repaint = true;
        }
        self.tasks_refresh();
        if self.last_recovery.elapsed() >= Duration::from_millis(self.settings.recovery_interval_ms.max(1000)) {
            self.last_recovery = Instant::now();
            self.write_recovery_snapshots();
        }
        if self.last_session_save.elapsed() >= Duration::from_secs(30) {
            self.last_session_save = Instant::now();
            self.save_session();
        }
        if repaint {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    // ----- sidebar ---------------------------------------------------------------

    pub(crate) fn show_sidebar(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        let active_file = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone());
        if self.sidebar.follow_active
            && let Some(p) = &active_file
            && self.sidebar.selected.as_deref() != Some(p.as_path())
            && self.roots.iter().any(|r| p.starts_with(r))
        {
            self.sidebar.reveal(p);
        }
        let actions = self.sidebar.ui(ui, &theme, ui_font, active_file.as_deref());
        for a in actions {
            self.handle_sidebar_action(ui.ctx(), a);
        }
    }

    pub(crate) fn handle_sidebar_action(&mut self, ctx: &egui::Context, a: SidebarAction) {
        match a {
            SidebarAction::OpenPreview(p) => {
                self.open_preview(&p);
            }
            SidebarAction::Open(p) => {
                if let Some(v) = self.open_path(&p) {
                    self.make_permanent(v);
                    self.focus_active_editor(ctx);
                }
            }
            SidebarAction::NewFile(dir) => {
                self.dialog =
                    Some(Dialog::Prompt { title: format!("New file in {}", self.display_path(&dir)), text: String::new(), kind: PromptKind::NewFile(dir) })
            }
            SidebarAction::NewFolder(dir) => {
                self.dialog =
                    Some(Dialog::Prompt { title: format!("New folder in {}", self.display_path(&dir)), text: String::new(), kind: PromptKind::NewFolder(dir) })
            }
            SidebarAction::Rename { from, new_name } => self.rename_path(&from, &new_name),
            SidebarAction::Duplicate(p) => self.duplicate_path(&p),
            SidebarAction::Delete(p) => self.dialog = Some(Dialog::ConfirmDelete(p)),
            SidebarAction::Reveal(p) => crate::app::reveal_in_file_manager(&p),
            SidebarAction::CopyPath(p, relative) => {
                let text = if relative { self.display_path(&p) } else { p.display().to_string() };
                ctx.copy_text(text);
                self.set_status("Path copied".into(), false);
            }
            SidebarAction::FindInFolder(dir) => {
                self.fif.where_ = vec![dir];
                self.open_find_in_files(None);
            }
            SidebarAction::RemoveRoot(p) => self.remove_root(&p),
            SidebarAction::AddFolder => self.execute(ctx, Command::AddFolder, None),
            SidebarAction::OpenFolder => self.execute(ctx, Command::OpenFolder, None),
            SidebarAction::Refresh => {
                self.sidebar.refresh_all();
                self.restart_index();
            }
        }
    }

    /// Open a file as a preview tab (replacing the pane's current preview).
    pub(crate) fn open_preview(&mut self, path: &Path) -> Option<ViewId> {
        let pane_id = self.active_pane;
        let already = self
            .views
            .iter()
            .find(|(_, v)| self.docs.get(&v.doc).and_then(|d| d.path.as_ref()).is_some_and(|p| p == &crate::app::normalise_path(path)))
            .map(|(id, _)| *id);
        if let Some(v) = already {
            self.focus_view(v);
            return Some(v);
        }
        let old_preview = self.panes.get(&pane_id).and_then(|p| p.preview);
        let v = self.open_path(path)?;
        if let Some(old) = old_preview
            && old != v
            && self.views.get(&old).and_then(|v| self.docs.get(&v.doc)).is_some_and(|d| !d.is_dirty())
        {
            self.close_view_now(old);
        }
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.preview = Some(v);
            pane.activate(v);
        }
        Some(v)
    }

    pub(crate) fn make_permanent(&mut self, v: ViewId) {
        for pane in self.panes.values_mut() {
            if pane.preview == Some(v) {
                pane.preview = None;
            }
        }
    }

    pub(crate) fn is_preview(&self, v: ViewId) -> bool {
        self.panes.values().any(|p| p.preview == Some(v))
    }

    pub(crate) fn create_file(&mut self, dir: &Path, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            self.set_status("Enter a plain file name without path separators".into(), true);
            return;
        }
        let path = dir.join(name);
        if path.exists() {
            self.set_status(format!("{} already exists", path.display()), true);
            return;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => {
                self.sidebar.refresh_path(&path);
                self.sidebar.reveal(&path);
                if let Some(v) = self.open_path(&path) {
                    self.make_permanent(v);
                }
            }
            Err(e) => self.set_status(format!("Could not create {}: {e}", path.display()), true),
        }
    }

    pub(crate) fn create_folder(&mut self, dir: &Path, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            self.set_status("Enter a plain folder name".into(), true);
            return;
        }
        let path = dir.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {
                self.sidebar.refresh_path(&path);
                self.sidebar.reveal(&path);
            }
            Err(e) => self.set_status(format!("Could not create folder: {e}"), true),
        }
    }

    pub(crate) fn rename_path(&mut self, from: &Path, new_name: &str) {
        let Some(parent) = from.parent() else { return };
        let to = parent.join(new_name);
        if to.exists() {
            self.set_status(format!("{} already exists", to.display()), true);
            return;
        }
        match std::fs::rename(from, &to) {
            Ok(()) => {
                // Update open documents pointing at the old path (or inside a renamed folder).
                for doc in self.docs.values_mut() {
                    if let Some(p) = &doc.path
                        && let Ok(rest) = p.strip_prefix(from)
                    {
                        doc.path = Some(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) });
                        doc.file_identity = doc.path.as_ref().and_then(|p| apex_project::fileio::identity(p));
                    }
                }
                self.sidebar.refresh_path(from);
                self.sidebar.reveal(&to);
                self.set_status(format!("Renamed to {}", to.display()), false);
            }
            Err(e) => self.set_status(format!("Could not rename: {e}"), true),
        }
    }

    pub(crate) fn duplicate_path(&mut self, p: &Path) {
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let parent = p.parent().unwrap_or(Path::new("."));
        let mut n = 1;
        let target = loop {
            let candidate = parent.join(if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") });
            if !candidate.exists() {
                break candidate;
            }
            n += 1;
            if n > 1000 {
                return;
            }
        };
        match std::fs::copy(p, &target) {
            Ok(_) => {
                self.sidebar.refresh_path(&target);
                self.open_path(&target);
            }
            Err(e) => self.set_status(format!("Could not duplicate: {e}"), true),
        }
    }

    pub(crate) fn delete_path(&mut self, p: &Path) {
        let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        match r {
            Ok(()) => {
                let affected: Vec<ViewId> = self
                    .views
                    .iter()
                    .filter(|(_, v)| self.docs.get(&v.doc).and_then(|d| d.path.as_ref()).is_some_and(|dp| dp.starts_with(p)))
                    .map(|(id, _)| *id)
                    .collect();
                for v in affected {
                    // Keep the buffer if it has unsaved edits; detach the path otherwise.
                    let did = self.views[&v].doc;
                    if let Some(doc) = self.docs.get_mut(&did) {
                        if doc.is_dirty() {
                            doc.file_identity = None;
                        } else {
                            self.close_view_now(v);
                        }
                    }
                }
                if self.views.is_empty() {
                    self.new_untitled();
                }
                self.sidebar.refresh_path(p);
                self.index.remove_path(p);
                self.set_status(format!("Deleted {}", p.display()), false);
            }
            Err(e) => self.set_status(format!("Could not delete: {e}"), true),
        }
    }

    // ----- palette ---------------------------------------------------------------

    pub(crate) fn open_command_palette(&mut self) {
        let has_editor = self.active_view_id().is_some();
        let mut items = Vec::with_capacity(Command::ALL.len());
        for (i, c) in Command::ALL.iter().enumerate() {
            if c.needs_editor() && !has_editor {
                continue;
            }
            let recent = self.recent_commands.iter().position(|r| r == c).map(|p| 40u32.saturating_sub(p as u32 * 4)).unwrap_or(0);
            items.push(PaletteItem {
                label: format!("{}: {}", c.category().label(), c.name()),
                detail: String::new(),
                shortcut: self.keymap.shortcut_for(*c).unwrap_or_default(),
                payload: Payload::Command(*c, None),
                boost: recent + (Command::ALL.len() - i) as u32 / 100,
            });
        }
        self.palette.show_with(PaletteMode::Commands, items, "Command palette", "");
    }

    pub(crate) fn open_goto_anything(&mut self, initial: &str) {
        self.remember_palette_restore();
        let mut items: Vec<PaletteItem> = Vec::with_capacity(self.index.files.len() + 16);
        let recent: Vec<PathBuf> = self.recent_files.iter().cloned().collect();
        let multi = self.roots.len() > 1;
        for f in &self.index.files {
            let boost = recent.iter().position(|r| r == &f.path).map(|p| 30u32.saturating_sub(p as u32)).unwrap_or(0);
            let detail = if multi {
                self.roots.get(f.root).and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                String::new()
            };
            items.push(PaletteItem { label: f.rel.to_string(), detail, shortcut: String::new(), payload: Payload::File(f.path.clone(), None), boost });
        }
        // Open files not under any root (and recent files) are still reachable.
        for d in self.docs.values() {
            if let Some(p) = &d.path
                && !items.iter().any(|i| matches!(&i.payload, Payload::File(fp, _) if fp == p))
            {
                items.push(PaletteItem {
                    label: d.title(),
                    detail: p.parent().map(|x| x.display().to_string()).unwrap_or_default(),
                    shortcut: String::new(),
                    payload: Payload::File(p.clone(), None),
                    boost: 20,
                });
            }
        }
        for p in recent.iter().take(30) {
            if !items.iter().any(|i| matches!(&i.payload, Payload::File(fp, _) if fp == p)) {
                items.push(PaletteItem {
                    label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    detail: p.parent().map(|x| x.display().to_string()).unwrap_or_default(),
                    shortcut: String::new(),
                    payload: Payload::File(p.clone(), None),
                    boost: 10,
                });
            }
        }
        let placeholder = if self.index_scan.is_some() { "Go to file… (indexing)" } else { "Go to file  —  :line  @symbol  #text" };
        self.palette.show_with(PaletteMode::Anything, items, placeholder, initial);
    }

    fn remember_palette_restore(&mut self) {
        self.palette_preview = None;
        if let Some(v) = self.active_view_id()
            && let Some(view) = self.views.get(&v)
        {
            self.palette.restore = Some((v, view.sels.clone(), view.scroll));
        }
    }

    pub(crate) fn open_symbol_palette(&mut self, initial: &str) {
        let Some((_, d)) = self.active_ids() else { return };
        self.remember_palette_restore();
        self.ensure_syntax(d);
        let syms = match (self.syntax.get(&d), self.docs.get(&d)) {
            (Some(st), Some(doc)) => apex_syntax::symbols::document_symbols(st, doc.rope(), 5000),
            _ => Vec::new(),
        };
        let items: Vec<PaletteItem> = syms
            .iter()
            .map(|s| PaletteItem {
                label: format!("{}{}", "  ".repeat(s.depth as usize), s.name),
                detail: format!("{} · line {}", s.kind.label(), s.line + 1),
                shortcut: String::new(),
                payload: Payload::Symbol { line: s.line, byte: s.name_range.0, path: None },
                boost: 0,
            })
            .collect();
        let placeholder = if items.is_empty() { "No symbols in this file" } else { "Go to symbol" };
        self.palette.show_with(PaletteMode::Symbols, items, placeholder, initial);
    }

    pub(crate) fn open_project_symbol_palette(&mut self) {
        if self.symbol_scan.is_none() && !self.symbols.complete && !self.index.files.is_empty() {
            let files: Vec<PathBuf> = self.index.files.iter().map(|f| f.path.clone()).collect();
            self.symbol_scan = Some(crate::symbol_index::start(files));
        }
        self.remember_palette_restore();
        let items: Vec<PaletteItem> = self
            .symbols
            .symbols
            .iter()
            .map(|ps| PaletteItem {
                label: ps.symbol.name.clone(),
                detail: format!("{} · {}:{}", ps.symbol.kind.label(), self.display_path(&ps.path), ps.symbol.line + 1),
                shortcut: String::new(),
                payload: Payload::Symbol { line: ps.symbol.line, byte: ps.symbol.name_range.0, path: Some(ps.path.clone()) },
                boost: 0,
            })
            .collect();
        let placeholder = if self.symbol_scan.is_some() {
            "Go to symbol in project… (indexing)"
        } else if self.roots.is_empty() {
            "Open a folder to index project symbols"
        } else {
            "Go to symbol in project"
        };
        self.palette.show_with(PaletteMode::ProjectSymbols, items, placeholder, "");
    }

    pub(crate) fn open_list_palette(&mut self, mode: PaletteMode) {
        let items: Vec<PaletteItem> = match &mode {
            PaletteMode::Themes => self
                .themes
                .iter()
                .map(|t| PaletteItem {
                    label: t.name.clone(),
                    detail: if t.dark { "dark".into() } else { "light".into() },
                    shortcut: String::new(),
                    payload: Payload::Text(t.name.clone()),
                    boost: (t.name == self.theme.name) as u32,
                })
                .collect(),
            PaletteMode::Languages => {
                let mut v = vec![PaletteItem {
                    label: "Auto-detect".into(),
                    detail: String::new(),
                    shortcut: String::new(),
                    payload: Payload::Text("auto".into()),
                    boost: 1,
                }];
                v.extend(apex_syntax::languages().iter().filter(|l| l.id != "markdown_inline" && l.id != "regex").map(|l| PaletteItem {
                    label: l.name.to_string(),
                    detail: l.extensions.iter().take(4).map(|e| format!(".{e}")).collect::<Vec<_>>().join(" "),
                    shortcut: String::new(),
                    payload: Payload::Text(l.id.to_string()),
                    boost: 0,
                }));
                v
            }
            PaletteMode::Encodings => apex_buffer::Encoding::all()
                .iter()
                .map(|e| PaletteItem { label: e.label(), detail: String::new(), shortcut: String::new(), payload: Payload::Text(e.id()), boost: 0 })
                .collect(),
            PaletteMode::LineEndings => ["LF", "CRLF", "CR"]
                .iter()
                .map(|l| PaletteItem { label: l.to_string(), detail: String::new(), shortcut: String::new(), payload: Payload::Text(l.to_string()), boost: 0 })
                .collect(),
            PaletteMode::RecentFiles => self
                .recent_files
                .iter()
                .enumerate()
                .map(|(i, p)| PaletteItem {
                    label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    detail: p.parent().map(|x| x.display().to_string()).unwrap_or_default(),
                    shortcut: String::new(),
                    payload: Payload::File(p.clone(), None),
                    boost: 100u32.saturating_sub(i as u32),
                })
                .collect(),
            PaletteMode::RecentProjects => self
                .recent_projects
                .iter()
                .enumerate()
                .map(|(i, p)| PaletteItem {
                    label: p.display().to_string(),
                    detail: String::new(),
                    shortcut: String::new(),
                    payload: Payload::File(p.clone(), None),
                    boost: 100u32.saturating_sub(i as u32),
                })
                .collect(),
            PaletteMode::ClipboardHistory => self
                .clipboard_history
                .iter()
                .enumerate()
                .map(|(i, t)| PaletteItem {
                    label: t.lines().next().unwrap_or("").chars().take(80).collect(),
                    detail: format!("{} chars", t.chars().count()),
                    shortcut: String::new(),
                    payload: Payload::Index(i),
                    boost: 100u32.saturating_sub(i as u32),
                })
                .collect(),
            PaletteMode::Macros => self
                .macros
                .keys()
                .map(|k| PaletteItem {
                    label: k.clone(),
                    detail: format!("{} steps", self.macros[k].len()),
                    shortcut: String::new(),
                    payload: Payload::Text(k.clone()),
                    boost: 0,
                })
                .collect(),
            _ => Vec::new(),
        };
        let placeholder = match &mode {
            PaletteMode::Themes => "Select colour scheme",
            PaletteMode::Languages => "Set syntax",
            PaletteMode::Encodings => "Save with encoding",
            PaletteMode::LineEndings => "Line endings",
            PaletteMode::RecentFiles => "Open recent file",
            PaletteMode::RecentProjects => "Open recent folder",
            PaletteMode::ClipboardHistory => "Paste from history",
            PaletteMode::Macros => "Run macro",
            _ => "",
        };
        if matches!(mode, PaletteMode::Themes) {
            self.remember_palette_restore();
        }
        self.palette.show_with(mode, items, placeholder, "");
    }

    pub(crate) fn show_palette(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        // Operators of Goto Anything change the item set live.
        if self.palette.mode == PaletteMode::Anything
            && let Some((op, rest)) = self.palette.operator().map(|(o, r)| (o, r.to_string()))
        {
            if op == '@' && self.palette.match_text().is_empty() {
                self.open_symbol_palette(&rest);
                self.palette.query = format!("@{rest}");
                self.palette.mode = PaletteMode::Symbols;
            } else if op == '#' && self.palette.match_text().is_empty() {
                self.open_text_palette(&rest);
            }
        }
        let theme = self.theme.clone();
        let event = self.palette.ui(ctx, &theme, self.settings.ui_font_size);
        match event {
            PaletteEvent::None => {}
            PaletteEvent::Highlight(p) => self.palette_highlight(ctx, p),
            PaletteEvent::Accept(p, query) => {
                self.palette_accept(ctx, p, &query);
            }
            PaletteEvent::AcceptText(text) => {
                let mode = self.palette.mode.clone();
                self.palette.close();
                match mode {
                    PaletteMode::Anything | PaletteMode::Symbols => {
                        // `:120` with no file goes to the line in the current file.
                        if let Some(rest) = text.trim().strip_prefix(':')
                            && let Some((l, c)) = crate::app::parse_line_col(rest)
                            && let Some(v) = self.active_view_id()
                        {
                            self.note_jump();
                            self.goto_line_col(v, l, c);
                        }
                    }
                    PaletteMode::Prompt(_) => {}
                    _ => {}
                }
                self.focus_active_editor(ctx);
            }
            PaletteEvent::Cancel => {
                self.palette_cancel(ctx);
            }
        }
    }

    /// `#text` operator: fuzzy search over the document's lines.
    fn open_text_palette(&mut self, rest: &str) {
        let Some((_, d)) = self.active_ids() else { return };
        let Some(doc) = self.docs.get(&d) else { return };
        let rope = doc.rope();
        let n = rope.len_lines().min(100_000);
        let items: Vec<PaletteItem> = (0..n)
            .filter_map(|l| {
                let t = doc.line_text(l);
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(PaletteItem {
                    label: trimmed.chars().take(160).collect(),
                    detail: format!("line {}", l + 1),
                    shortcut: String::new(),
                    payload: Payload::Line(l + 1, None),
                    boost: 0,
                })
            })
            .collect();
        self.palette.mode = PaletteMode::Symbols;
        self.palette.items = items;
        self.palette.query = rest.to_string();
        self.palette.placeholder = "Go to text".into();
        self.palette.results.clear();
        self.palette.rescore();
    }

    fn palette_highlight(&mut self, _ctx: &egui::Context, p: Payload) {
        match p {
            Payload::File(path, _) if matches!(self.palette.mode, PaletteMode::Anything | PaletteMode::RecentFiles) => {
                if path.is_file()
                    && let Some(v) = self.open_preview(&path)
                {
                    self.palette_preview = Some(v);
                }
            }
            Payload::Symbol { line, byte, path: None } => {
                if let Some(v) = self.active_view_id() {
                    self.goto_byte(v, line, byte, true);
                }
            }
            Payload::Line(l, c) => {
                if let Some(v) = self.active_view_id() {
                    self.goto_line_col(v, l, c.unwrap_or(1));
                }
            }
            Payload::Text(name) if self.palette.mode == PaletteMode::Themes && self.themes.iter().any(|t| t.name == name) && self.theme.name != name => {
                self.settings.theme = name.clone();
                self.theme = self.themes.iter().find(|t| t.name == name).cloned().unwrap_or_else(|| self.theme.clone());
                self.theme.apply_to_egui(_ctx);
                self.invalidate_all_layouts();
            }
            _ => {}
        }
    }

    fn palette_accept(&mut self, ctx: &egui::Context, p: Payload, query: &str) {
        let mode = self.palette.mode.clone();
        let operator = self.palette.operator().map(|(o, r)| (o, r.to_string()));
        self.palette.close();
        if self.lsp_palette_accept(ctx, &mode, &p) || self.snippet_palette_accept(ctx, &mode, &p) {
            return;
        }
        match (mode, p) {
            (PaletteMode::Commands, Payload::Command(c, arg)) => {
                self.recent_commands.retain(|r| *r != c);
                self.recent_commands.push_front(c);
                self.recent_commands.truncate(20);
                self.focus_active_editor(ctx);
                self.execute(ctx, c, arg);
            }
            (PaletteMode::Anything | PaletteMode::RecentFiles, Payload::File(path, _)) => {
                self.note_jump();
                if let Some(v) = self.open_path(&path) {
                    self.make_permanent(v);
                    if let Some((':', rest)) = operator
                        && let Some((l, c)) = crate::app::parse_line_col(&rest)
                    {
                        self.goto_line_col(v, l, c);
                    }
                }
                self.palette_preview = None;
                self.focus_active_editor(ctx);
            }
            (PaletteMode::RecentProjects, Payload::File(path, _)) => self.open_folder(&path),
            (PaletteMode::Symbols | PaletteMode::Anything, Payload::Symbol { line, byte, path: None }) => {
                if let Some(v) = self.active_view_id() {
                    self.goto_byte(v, line, byte, false);
                }
                self.focus_active_editor(ctx);
            }
            (PaletteMode::ProjectSymbols, Payload::Symbol { line, byte, path: Some(path) }) => {
                self.note_jump();
                if let Some(v) = self.open_path(&path) {
                    self.make_permanent(v);
                    self.goto_byte(v, line, byte, false);
                }
                self.focus_active_editor(ctx);
            }
            (_, Payload::Line(l, c)) => {
                if let Some(v) = self.active_view_id() {
                    self.note_jump();
                    self.goto_line_col(v, l, c.unwrap_or(1));
                }
                self.focus_active_editor(ctx);
            }
            (PaletteMode::Themes, Payload::Text(name)) => {
                self.execute(ctx, Command::SelectTheme, Some(name));
                self.focus_active_editor(ctx);
            }
            (PaletteMode::Languages, Payload::Text(id)) => self.execute(ctx, Command::SetLanguage, Some(id)),
            (PaletteMode::Encodings, Payload::Text(id)) => self.execute(ctx, Command::SetEncoding, Some(id)),
            (PaletteMode::LineEndings, Payload::Text(id)) => self.execute(ctx, Command::SetLineEnding, Some(id)),
            (PaletteMode::ClipboardHistory, Payload::Index(i)) => self.execute(ctx, Command::PasteFromHistory, Some(i.to_string())),
            (PaletteMode::Macros, Payload::Text(name)) => self.run_macro_named(ctx, &name),
            (PaletteMode::BuildTasks, Payload::Index(i)) => {
                self.focus_active_editor(ctx);
                self.run_task(i, ctx);
            }
            (PaletteMode::Prompt(kind), Payload::Text(t)) if kind == "history" => {
                ctx.copy_text(t);
                self.set_status("Commit id copied".into(), false);
                self.focus_active_editor(ctx);
            }
            _ => {
                let _ = query;
            }
        }
    }

    fn palette_cancel(&mut self, ctx: &egui::Context) {
        let restore = self.palette.restore.take();
        let mode = self.palette.mode.clone();
        self.palette.close();
        if let Some(v) = self.palette_preview.take()
            && self.is_preview(v)
            && self.views.get(&v).and_then(|x| self.docs.get(&x.doc)).is_some_and(|d| !d.is_dirty())
        {
            self.close_view_now(v);
        }
        if let Some((v, sels, scroll)) = restore {
            if mode == PaletteMode::Themes && self.theme.name != self.settings.theme {
                let name = self.settings.theme.clone();
                self.execute(ctx, Command::SelectTheme, Some(name));
            }
            if self.views.contains_key(&v) {
                self.focus_view(v);
                if let Some(view) = self.views.get_mut(&v) {
                    view.sels = sels;
                    view.scroll = scroll;
                    view.want_cursor_visible = false;
                }
            }
        }
        if self.views.is_empty() {
            self.new_untitled();
        }
        self.focus_active_editor(ctx);
    }

    /// Place the caret at a byte offset (or line start) and centre it.
    pub(crate) fn goto_byte(&mut self, v: ViewId, line: usize, byte: usize, preview: bool) {
        let Some(view) = self.views.get_mut(&v) else { return };
        let Some(doc) = self.docs.get(&view.doc) else { return };
        let rope = doc.rope();
        let pos = if byte <= rope.len_bytes() { rope.byte_to_char(byte) } else { rope.line_to_char(line.min(rope.len_lines().saturating_sub(1))) };
        view.set_selections(SelectionSet::single(Selection::point(pos)));
        view.want_centre = true;
        let _ = preview;
    }

    // ----- jump history -----------------------------------------------------------

    pub(crate) fn note_jump(&mut self) {
        let Some((v, d)) = self.active_ids() else { return };
        let sels = self.views[&v].sels.clone();
        self.jumps.truncate(self.jump_pos);
        if self.jumps.last().is_some_and(|(ld, ls)| *ld == d && ls.primary().head == sels.primary().head) {
            return;
        }
        self.jumps.push((d, sels));
        if self.jumps.len() > MAX_JUMPS {
            self.jumps.remove(0);
        }
        self.jump_pos = self.jumps.len();
    }

    pub(crate) fn jump(&mut self, back: bool) {
        if back {
            if self.jump_pos == self.jumps.len() {
                // Record where we are so Forward can return.
                self.note_jump();
                self.jump_pos = self.jumps.len().saturating_sub(1);
            }
            if self.jump_pos == 0 {
                return;
            }
            self.jump_pos -= 1;
        } else {
            if self.jump_pos + 1 >= self.jumps.len() {
                return;
            }
            self.jump_pos += 1;
        }
        let (d, sels) = self.jumps[self.jump_pos].clone();
        if let Some(v) = self.views.values().find(|v| v.doc == d).map(|v| v.id) {
            self.focus_view(v);
            if let Some(view) = self.views.get_mut(&v)
                && let Some(doc) = self.docs.get(&d)
            {
                view.sels = sels;
                view.sels.clamp(doc.rope());
                view.want_centre = true;
            }
        }
    }

    // ----- find in files ------------------------------------------------------------

    pub(crate) fn open_find_in_files(&mut self, initial: Option<String>) {
        self.panel_open = true;
        self.panel_tab = crate::tasks_ui::PanelTab::Find;
        self.fif.open = true;
        self.fif.focus_query = true;
        if let Some(t) = initial {
            self.fif.query = t;
        } else if let Some((v, d)) = self.active_ids() {
            let p = self.views[&v].sels.primary();
            if !p.is_empty() && p.len() < 200 {
                self.fif.query = self.docs[&d].rope().slice(p.range()).to_string();
            }
        }
    }

    pub(crate) fn show_bottom_panel(&mut self, ui: &mut Ui) {
        use crate::tasks_ui::PanelTab;
        let theme = self.theme.clone();
        let roots = self.roots.clone();
        let ui_font = self.settings.ui_font_size;
        ui.horizontal(|ui| {
            for (tab, label) in [(PanelTab::Find, "FIND IN FILES"), (PanelTab::Output, "OUTPUT"), (PanelTab::Problems, "PROBLEMS")] {
                let selected = self.panel_tab == tab;
                let text = egui::RichText::new(label).size(ui_font - 2.0).color(if selected { theme.ui.text } else { theme.ui.text_muted }).strong();
                if ui.selectable_label(selected, text).clicked() {
                    self.panel_tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Close panel").clicked() {
                    self.panel_open = false;
                    self.fif.cancel();
                }
            });
        });
        match self.panel_tab {
            PanelTab::Output => {
                self.show_output_panel(ui);
                return;
            }
            PanelTab::Problems => {
                self.show_problems_panel(ui);
                return;
            }
            PanelTab::Find => {}
        }
        let actions = self.fif.ui(ui, &theme, ui_font, &roots);
        for a in actions {
            match a {
                FifAction::Open { path, line, col, neighbour } if path.as_os_str().is_empty() => {
                    let _ = (line, col, neighbour);
                    let exclude = self.settings.exclude_patterns.clone();
                    let follow = self.settings.follow_symlinks;
                    let roots = self.roots.clone();
                    self.fif.start(roots, &exclude, follow);
                }
                FifAction::Open { path, line, col, neighbour } => {
                    self.note_jump();
                    if neighbour {
                        let ids = self.layout.pane_ids();
                        if ids.len() == 1 {
                            self.split_active(Axis::Horizontal);
                        } else if let Some(i) = ids.iter().position(|p| *p == self.active_pane) {
                            self.active_pane = ids[(i + 1) % ids.len()];
                        }
                    }
                    if let Some(v) = self.open_path(&path) {
                        self.make_permanent(v);
                        let col_chars = self.docs.get(&self.views[&v].doc).map(|d| {
                            let rope = d.rope();
                            let ls = rope.line_to_byte(line.min(rope.len_lines().saturating_sub(1)));
                            rope.byte_to_char((ls + col).min(rope.len_bytes()))
                                - rope.line_to_char(rope.char_to_line(rope.byte_to_char((ls + col).min(rope.len_bytes()))))
                        });
                        self.goto_line_col(v, line + 1, col_chars.unwrap_or(0) + 1);
                        self.focus_active_editor(ui.ctx());
                    }
                }
                FifAction::ApplyReplace { .. } => {}
            }
        }
        if self.fif.confirm_replace {
            self.fif.confirm_replace = false;
            self.dialog = Some(Dialog::ConfirmReplaceInFiles);
        }
    }

    /// Perform the project-wide replacement after confirmation.
    pub(crate) fn apply_replace_in_files(&mut self) {
        let req = apex_project::search::SearchRequest {
            roots: Vec::new(),
            pattern: self.fif.query.clone(),
            regex: self.fif.regex,
            case_sensitive: self.fif.case_sensitive,
            whole_word: self.fif.whole_word,
            include: Vec::new(),
            exclude: Vec::new(),
            hidden: false,
            respect_gitignore: true,
            follow_symlinks: false,
            max_file_bytes: 0,
            max_results: 0,
            context_lines: 0,
        };
        let re = match apex_project::search::compile(&req) {
            Ok(r) => r,
            Err(e) => {
                self.set_status(format!("Invalid pattern: {e}"), true);
                return;
            }
        };
        let replacement = self.fif.replacement.clone();
        let files: Vec<PathBuf> = self.fif.results.iter().filter(|f| !self.fif.excluded.contains(&f.path)).map(|f| f.path.clone()).collect();
        let mut replaced = 0usize;
        let mut files_ok = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in files {
            // Open documents are edited in the editor so the change is undoable.
            let open_doc = self.docs.iter().find(|(_, d)| d.path.as_ref() == Some(&path)).map(|(id, d)| (*id, d.is_dirty()));
            match open_doc {
                Some((did, true)) => {
                    failures.push(format!("{}: has unsaved changes in the editor; skipped", self.display_path(&path)));
                    let _ = did;
                }
                Some((did, false)) => {
                    if let Some(v) = self.views.values().find(|v| v.doc == did).map(|v| v.id) {
                        let text = self.docs[&did].text();
                        let count = re.find_iter(&text).filter(|m| m.start() != m.end()).count();
                        if count > 0 {
                            let new_text = re.replace_all(&text, replacement.as_str()).into_owned();
                            self.run_edit(v, |d, s| {
                                let tx = apex_edit::Transaction::single(apex_edit::Change::replace(0, d.len_chars(), new_text));
                                let mut ns = s.clone();
                                ns.map(&tx, apex_edit::Assoc::Before);
                                Some(apex_edit::ops::EditResult { tx, sels: ns, kind: apex_edit::EditKind::Other })
                            });
                            if self.save_view(v, false) {
                                replaced += count;
                                files_ok += 1;
                            } else {
                                failures.push(format!("{}: could not save", self.display_path(&path)));
                            }
                        }
                    }
                }
                None => {
                    let out = apex_project::search::replace_in_file(&path, &re, &replacement, None);
                    match out.error {
                        None => {
                            replaced += out.replaced;
                            files_ok += 1;
                        }
                        Some(e) => failures.push(format!("{}: {e}", self.display_path(&path))),
                    }
                }
            }
        }
        let mut msg =
            format!("Replaced {replaced} occurrence{} in {files_ok} file{}", if replaced == 1 { "" } else { "s" }, if files_ok == 1 { "" } else { "s" });
        if !failures.is_empty() {
            msg.push_str(&format!("; {} failed: {}", failures.len(), failures.join("; ")));
        }
        self.set_status(msg, !failures.is_empty());
        let exclude = self.settings.exclude_patterns.clone();
        let follow = self.settings.follow_symlinks;
        let roots = self.roots.clone();
        self.fif.start(roots, &exclude, follow);
    }

    // ----- session and recovery ----------------------------------------------------

    pub(crate) fn build_session(&self) -> Session {
        let pane_ids = self.layout.pane_ids();
        let mut panes = Vec::new();
        for pid in &pane_ids {
            let pane = self.panes.get(pid).cloned().unwrap_or_default();
            let tabs: Vec<TabState> = pane
                .tabs
                .iter()
                .filter_map(|v| {
                    let view = self.views.get(v)?;
                    let doc = self.docs.get(&view.doc)?;
                    if doc.path.is_none() && !doc.is_dirty() {
                        return None;
                    }
                    Some(TabState {
                        path: doc.path.clone(),
                        recovery_id: self.recovery_ids.get(&doc.id).map(|(id, _)| id.clone()),
                        sels: view.sels.clone(),
                        scroll: (view.scroll.x, view.top_line as f32),
                        pinned: pane.pinned.contains(v),
                        language: doc.language_override.clone(),
                    })
                })
                .collect();
            panes.push(PaneState { tabs, active: pane.active });
        }
        fn layout_state(l: &Layout, ids: &[PaneId]) -> LayoutState {
            match l {
                Layout::Leaf(p) => LayoutState::Leaf { pane: ids.iter().position(|x| x == p).unwrap_or(0) },
                Layout::Split { axis, children, fractions } => LayoutState::Split {
                    horizontal: *axis == Axis::Horizontal,
                    children: children.iter().map(|c| layout_state(c, ids)).collect(),
                    fractions: fractions.clone(),
                },
            }
        }
        let window = WindowState {
            panes,
            layout: layout_state(&self.layout, &pane_ids),
            active_pane: pane_ids.iter().position(|p| *p == self.active_pane).unwrap_or(0),
            size: None,
            position: None,
            maximised: false,
            sidebar_width: Some(self.sidebar.width),
        };
        Session {
            version: apex_project::session::SESSION_VERSION,
            windows: vec![window],
            roots: self.roots.clone(),
            project_file: self.project_file.clone(),
            recent_files: self.recent_files.iter().cloned().collect(),
            recent_projects: self.recent_projects.iter().cloned().collect(),
            find_history: self.fif.history.clone(),
            replace_history: Vec::new(),
            theme: Some(self.settings.theme.clone()),
            font_zoom: self.font_zoom,
        }
    }

    pub(crate) fn save_session(&mut self) {
        if !self.settings.restore_session {
            return;
        }
        if let Err(e) = self.build_session().save() {
            tracing::warn!("could not save session: {e}");
        }
    }

    /// Restore a saved session. Returns true if any tab was restored.
    pub(crate) fn restore_session(&mut self, s: Session) -> bool {
        let mut any = false;
        for r in &s.roots {
            if r.is_dir() {
                self.roots.push(r.clone());
            }
        }
        if !self.roots.is_empty() {
            self.roots_changed();
        }
        self.recent_projects = s.recent_projects.into_iter().collect();
        self.font_zoom = s.font_zoom;
        self.fif.history = s.find_history;
        let Some(w) = s.windows.into_iter().next() else { return false };
        // Rebuild panes in order, then the layout tree.
        let mut pane_ids: Vec<PaneId> = Vec::new();
        for (i, p) in w.panes.iter().enumerate() {
            let pid = if i == 0 { self.active_pane } else { self.new_pane() };
            pane_ids.push(pid);
            self.active_pane = pid;
            for t in &p.tabs {
                let v = match &t.path {
                    Some(path) if path.exists() => self.open_path(path),
                    Some(path) => {
                        // Missing file: keep a tab only if recovery has its text.
                        if let Some(rid) = &t.recovery_id
                            && let Some(e) = RecoveryEntry::list().into_iter().find(|e| &e.id == rid)
                        {
                            let mut doc = apex_edit::Document::from_text(&e.text);
                            doc.path = Some(path.clone());
                            let v = self.add_document(doc);
                            self.recovery_ids.insert(self.views[&v].doc, (rid.clone(), 0));
                            Some(v)
                        } else {
                            None
                        }
                    }
                    None => {
                        if let Some(rid) = &t.recovery_id
                            && let Some(e) = RecoveryEntry::list().into_iter().find(|e| &e.id == rid)
                        {
                            let v = self.new_untitled();
                            let did = self.views[&v].doc;
                            let sels = SelectionSet::default();
                            if let Some(doc) = self.docs.get_mut(&did) {
                                doc.replace_all(&e.text, &sels);
                            }
                            self.recovery_ids.insert(did, (rid.clone(), 0));
                            Some(v)
                        } else {
                            None
                        }
                    }
                };
                if let Some(v) = v {
                    any = true;
                    if let Some(view) = self.views.get_mut(&v) {
                        view.sels = t.sels.clone();
                        if let Some(doc) = self.docs.get(&view.doc) {
                            view.sels.clamp(doc.rope());
                        }
                        view.scroll = egui::vec2(t.scroll.0, 0.0);
                        view.top_line = t.scroll.1.max(0.0) as usize;
                        view.want_cursor_visible = false;
                    }
                    if let Some(doc) = self.views.get(&v).and_then(|view| self.docs.get_mut(&view.doc)) {
                        doc.language_override = t.language.clone();
                    }
                    if t.pinned
                        && let Some(pane) = self.panes.get_mut(&pid)
                    {
                        pane.pinned.insert(v);
                    }
                }
            }
            if let Some(pane) = self.panes.get_mut(&pid) {
                pane.active = p.active.min(pane.tabs.len().saturating_sub(1));
            }
        }
        fn layout_from(l: &LayoutState, ids: &[PaneId]) -> Option<Layout> {
            match l {
                LayoutState::Leaf { pane } => ids.get(*pane).map(|p| Layout::Leaf(*p)),
                LayoutState::Split { horizontal, children, fractions } => {
                    let kids: Vec<Layout> = children.iter().filter_map(|c| layout_from(c, ids)).collect();
                    if kids.len() < 2 || fractions.len() != kids.len() {
                        return kids.into_iter().next();
                    }
                    Some(Layout::Split { axis: if *horizontal { Axis::Horizontal } else { Axis::Vertical }, children: kids, fractions: fractions.clone() })
                }
            }
        }
        if let Some(l) = layout_from(&w.layout, &pane_ids) {
            // Only adopt the layout if it references every pane we created.
            let mut referenced = l.pane_ids();
            referenced.sort();
            let mut created = pane_ids.clone();
            created.sort();
            if referenced == created {
                self.layout = l;
            } else {
                // Fall back to columns.
                let mut layout = Layout::Leaf(pane_ids[0]);
                for w in pane_ids.windows(2) {
                    layout.split_leaf(w[0], w[1], Axis::Horizontal);
                }
                self.layout = layout;
            }
        }
        // Drop empty panes except the first.
        for pid in pane_ids.iter().skip(1) {
            if self.panes.get(pid).is_some_and(|p| p.tabs.is_empty()) {
                self.layout.remove_leaf(*pid);
                self.panes.remove(pid);
            }
        }
        self.active_pane = pane_ids.get(w.active_pane).copied().filter(|p| self.panes.contains_key(p)).unwrap_or(pane_ids[0]);
        if let Some(width) = w.sidebar_width {
            self.sidebar.width = width.clamp(120.0, 800.0);
        }
        any
    }

    /// Snapshot dirty buffers whose revision changed since the last snapshot.
    pub(crate) fn write_recovery_snapshots(&mut self) {
        if !self.settings.recovery {
            return;
        }
        let ids: Vec<DocId> = self.docs.keys().copied().collect();
        for d in ids {
            let doc = &self.docs[&d];
            let (rid, last_rev) = self.recovery_ids.get(&d).cloned().unwrap_or_else(|| (format!("{:x}-{:x}", apex_project::session::now_unix(), d.0), 0));
            if !doc.is_dirty() {
                if last_rev != 0 {
                    RecoveryEntry::remove(&rid);
                    self.recovery_ids.remove(&d);
                }
                continue;
            }
            if last_rev == doc.revision() {
                continue;
            }
            if doc.len_bytes() > 32 * 1024 * 1024 {
                continue; // too large to journal; documented limitation
            }
            let disk = doc
                .path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let entry = RecoveryEntry {
                id: rid.clone(),
                path: doc.path.clone(),
                title: doc.title(),
                text: doc.text(),
                disk_modified_unix: disk,
                saved_at_unix: apex_project::session::now_unix(),
            };
            match entry.write() {
                Ok(()) => {
                    self.recovery_ids.insert(d, (rid, doc.revision()));
                }
                Err(e) => tracing::warn!("recovery snapshot failed: {e}"),
            }
        }
    }

    pub(crate) fn forget_recovery(&mut self, d: DocId) {
        if let Some((rid, _)) = self.recovery_ids.remove(&d) {
            RecoveryEntry::remove(&rid);
        }
    }

    pub(crate) fn restore_recovery_entry(&mut self, e: &RecoveryEntry) {
        let v = match &e.path {
            Some(p) => {
                // Open the on-disk file (if any) and put the recovered text in as an edit,
                // so the user keeps undo back to the disk version and nothing is overwritten.
                let v = if p.exists() { self.open_path(p) } else { None };
                match v {
                    Some(v) => {
                        let sels = SelectionSet::default();
                        let did = self.views[&v].doc;
                        if let Some(doc) = self.docs.get_mut(&did) {
                            doc.replace_all(&e.text, &sels);
                        }
                        v
                    }
                    None => {
                        let mut doc = apex_edit::Document::from_text(&e.text);
                        doc.path = Some(p.clone());
                        let v = self.add_document(doc);
                        let did = self.views[&v].doc;
                        // Mark dirty so it is not mistaken for a saved file.
                        let sels = SelectionSet::default();
                        let text = e.text.clone();
                        if let Some(doc) = self.docs.get_mut(&did) {
                            doc.replace_all(&text, &sels);
                        }
                        v
                    }
                }
            }
            None => {
                let v = self.new_untitled();
                let did = self.views[&v].doc;
                let sels = SelectionSet::default();
                if let Some(doc) = self.docs.get_mut(&did) {
                    doc.replace_all(&e.text, &sels);
                }
                v
            }
        };
        let did = self.views[&v].doc;
        self.recovery_ids.insert(did, (e.id.clone(), 0));
        self.focus_view(v);
    }

    // ----- macros ----------------------------------------------------------------

    pub(crate) fn record_step(&mut self, step: MacroStep) {
        if let Some(rec) = &mut self.macro_recording
            && rec.len() < 10_000
        {
            rec.push(step);
        }
    }

    pub(crate) fn toggle_macro_recording(&mut self) {
        match self.macro_recording.take() {
            Some(steps) => {
                self.last_macro = steps;
                self.set_status(format!("Macro recorded ({} steps)", self.last_macro.len()), false);
            }
            None => {
                self.macro_recording = Some(Vec::new());
                self.set_status("Recording macro… (press again to stop)".into(), false);
            }
        }
    }

    pub(crate) fn play_macro(&mut self, ctx: &egui::Context, steps: &[MacroStep]) {
        if self.macro_recording.is_some() {
            return;
        }
        let Some(v) = self.active_view_id() else { return };
        for step in steps {
            match step {
                MacroStep::Command(c, arg) => self.execute(ctx, *c, arg.clone()),
                MacroStep::Insert(t) => self.insert_text(v, t),
            }
        }
    }

    pub(crate) fn run_macro_named(&mut self, ctx: &egui::Context, name: &str) {
        if let Some(steps) = self.macros.get(name).cloned() {
            self.play_macro(ctx, &steps);
        }
    }

    pub(crate) fn macros_path(&self) -> PathBuf {
        self.config_dir.join("macros.json")
    }

    pub(crate) fn load_macros(&mut self) {
        if let Ok(bytes) = std::fs::read(self.macros_path())
            && let Ok(m) = serde_json::from_slice::<std::collections::BTreeMap<String, Vec<MacroStep>>>(&bytes)
        {
            self.macros = m;
        }
    }

    pub(crate) fn save_macros(&mut self) {
        let _ = apex_project::paths::ensure_private_dir(&self.config_dir);
        match serde_json::to_vec_pretty(&self.macros) {
            Ok(bytes) => {
                if let Err(e) = apex_project::fileio::save_atomic(&self.macros_path(), &bytes) {
                    self.set_status(format!("Could not save macros: {e}"), true);
                }
            }
            Err(e) => self.set_status(format!("Could not serialise macros: {e}"), true),
        }
    }

    // ----- prompts -----------------------------------------------------------------

    pub(crate) fn prompt(&mut self, title: &str, initial: &str, kind: PromptKind) {
        self.dialog = Some(Dialog::Prompt { title: title.into(), text: initial.into(), kind });
    }

    pub(crate) fn prompt_accepted(&mut self, ctx: &egui::Context, kind: PromptKind, text: String) {
        match kind {
            PromptKind::NewFile(dir) => self.create_file(&dir, &text),
            PromptKind::NewFolder(dir) => self.create_folder(&dir, &text),
            PromptKind::MacroName => {
                let name = text.trim().to_string();
                if !name.is_empty() && !self.last_macro.is_empty() {
                    self.macros.insert(name.clone(), self.last_macro.clone());
                    self.save_macros();
                    self.set_status(format!("Saved macro {name}"), false);
                }
            }
            PromptKind::MarkName(set) => {
                let name = text.trim().to_string();
                if !name.is_empty() {
                    self.execute(ctx, if set { Command::SetMark } else { Command::GotoMark }, Some(name));
                }
            }
            PromptKind::Sequence => self.execute(ctx, Command::InsertSequence, Some(text)),
            PromptKind::Surround => self.execute(ctx, Command::SurroundWith, Some(text)),
            PromptKind::SaveProject => {
                let path = PathBuf::from(text.trim());
                self.save_project_file(&path);
            }
            PromptKind::Rename => self.execute(ctx, Command::LspRename, Some(text)),
        }
        self.focus_active_editor(ctx);
    }

    // ----- project files -------------------------------------------------------------

    pub(crate) fn save_project_file(&mut self, path: &Path) {
        let path = if path.extension().is_none() { path.with_extension("apex-project") } else { path.to_path_buf() };
        let base = path.parent().unwrap_or(Path::new("."));
        let folders: Vec<String> = self.roots.iter().map(|r| pathdiff_relative(base, r)).collect();
        let body = format!(
            "# Apex project file. Safe to commit: it holds no session state.\n# Paths are relative to this file.\nfolders = {}\n\n[settings]\n# Per-project setting overrides, same keys as settings.toml.\n\n# Build tasks run only after you trust this workspace (Project ▸ Trust Workspace).\n# [[tasks]]\n# name = \"Check\"\n# command = [\"cargo\", \"check\"]\n# problem_matcher = \"rustc\"\n",
            toml_array(&folders)
        );
        match apex_project::fileio::save_atomic(&path, body.as_bytes()) {
            Ok(()) => {
                self.project_file = Some(path.clone());
                self.push_recent_project(&path);
                self.set_status(format!("Saved project {}", path.display()), false);
            }
            Err(e) => self.set_status(format!("Could not save project: {e}"), true),
        }
    }

    pub(crate) fn open_project_file(&mut self, path: &Path) {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                self.set_status(format!("Could not read project: {e}"), true);
                return;
            }
        };
        let table: toml::Table = match toml::from_str(&src) {
            Ok(t) => t,
            Err(e) => {
                self.set_status(format!("Project file is not valid TOML: {e}"), true);
                return;
            }
        };
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        self.roots.clear();
        if let Some(folders) = table.get("folders").and_then(|f| f.as_array()) {
            for f in folders.iter().filter_map(|v| v.as_str()) {
                let p = base.join(f);
                if p.is_dir() {
                    self.roots.push(std::fs::canonicalize(&p).unwrap_or(p));
                }
            }
        }
        if let Some(tasks) = table.get("tasks") {
            match tasks.clone().try_into::<Vec<apex_project::tasks::TaskDef>>() {
                Ok(t) => self.set_project_tasks(t),
                Err(e) => self.set_status(format!("Project tasks ignored: {e}"), true),
            }
        } else {
            self.set_project_tasks(Vec::new());
        }
        if let Some(settings) = table.get("settings").and_then(|s| s.as_table()) {
            let settings = crate::settings_ui::project_safe_settings(settings);
            let src = toml::to_string(&settings).unwrap_or_default();
            let (merged, issues) = crate::settings::Settings::parse_layer(&self.settings, &src, &path.display().to_string());
            self.settings = merged;
            for i in issues {
                tracing::warn!("{}: {}", i.source, i.message);
            }
        }
        self.project_file = Some(path.to_path_buf());
        self.roots_changed();
        self.push_recent_project(path);
        self.set_status(format!("Opened project {}", path.display()), false);
    }

    // ----- workspace dialogs ------------------------------------------------------------

    pub(crate) fn show_workspace_dialog(&mut self, ctx: &egui::Context, dialog: Dialog) {
        match dialog {
            Dialog::ConfirmDelete(p) => {
                let r = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading(format!("Delete {}?", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    ui.label(if p.is_dir() {
                        "The folder and everything inside it will be permanently deleted."
                    } else {
                        "The file will be permanently deleted."
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            1
                        } else if ui.button("Cancel").clicked() {
                            2
                        } else {
                            0
                        }
                    })
                    .inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.delete_path(&p);
                    }
                    2 => self.dialog = None,
                    _ => {
                        if r.should_close() {
                            self.dialog = None;
                        }
                    }
                }
            }
            Dialog::ConfirmReplaceInFiles => {
                let n = self.fif.total_matches();
                let files = self.fif.results.iter().filter(|f| !self.fif.excluded.contains(&f.path)).count();
                let r = egui::Modal::new(egui::Id::new("confirm-replace")).show(ctx, |ui| {
                    ui.set_width(460.0);
                    ui.heading(format!("Replace {n} occurrences in {files} files?"));
                    ui.label(format!("“{}” → “{}”", self.fif.query, self.fif.replacement));
                    ui.label(egui::RichText::new("Each file is rewritten with a safe save; files that changed since the search are skipped and reported. This is not a single atomic operation across files.").color(self.theme.ui.text_muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| if ui.button("Replace").clicked() { 1 } else if ui.button("Cancel").clicked() { 2 } else { 0 }).inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.apply_replace_in_files();
                    }
                    2 => self.dialog = None,
                    _ => {
                        if r.should_close() {
                            self.dialog = None;
                        }
                    }
                }
            }
            Dialog::Prompt { title, mut text, kind } => {
                let mut accept = false;
                let mut cancel = false;
                let r = egui::Modal::new(egui::Id::new("prompt")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(&title);
                    let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                    resp.request_focus();
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        accept = true;
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            accept = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || r.should_close() {
                    cancel = true;
                }
                if accept {
                    self.dialog = None;
                    self.prompt_accepted(ctx, kind, text);
                } else if cancel {
                    self.dialog = None;
                    self.focus_active_editor(ctx);
                } else {
                    self.dialog = Some(Dialog::Prompt { title, text, kind });
                }
            }
            Dialog::Info { title, body } => {
                let r = egui::Modal::new(egui::Id::new("info")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.heading(&title);
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        ui.label(egui::RichText::new(&body).monospace());
                    });
                    ui.add_space(8.0);
                    ui.button("Close").clicked()
                });
                if r.inner || r.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.dialog = None;
                }
            }
            Dialog::ConfirmRevertHunk { view, hunk_start, hunk_end, old_start, old_end } => {
                let r = egui::Modal::new(egui::Id::new("revert-hunk")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading("Revert this change?");
                    ui.label(format!(
                        "Lines {}–{} will be replaced with the version from the Git index. Undo is available afterwards.",
                        hunk_start + 1,
                        hunk_end.max(hunk_start + 1)
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Revert").clicked() {
                            1
                        } else if ui.button("Cancel").clicked() {
                            2
                        } else {
                            0
                        }
                    })
                    .inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.git_apply_revert(view, hunk_start, hunk_end, old_start, old_end);
                    }
                    2 => self.dialog = None,
                    _ => {
                        if r.should_close() {
                            self.dialog = None;
                        }
                    }
                }
            }
            Dialog::Trust { root, then } => {
                let r = egui::Modal::new(egui::Id::new("trust")).show(ctx, |ui| {
                    ui.set_width(480.0);
                    ui.heading("Trust this workspace?");
                    ui.label(format!("{}", root.display()));
                    ui.label("Trusting allows build tasks and language servers defined in this folder's project configuration to be started. Programs run with your user's permissions. Editing files never requires trust.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| if ui.button("Trust").clicked() { 1 } else if ui.button("Not now").clicked() { 2 } else { 0 }).inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.trust.trust(&root);
                        self.set_status(format!("Trusted {}", root.display()), false);
                        if let Some((c, a)) = then {
                            self.execute(ctx, c, a);
                        }
                    }
                    2 => self.dialog = None,
                    _ => {
                        if r.should_close() {
                            self.dialog = None;
                        }
                    }
                }
            }
            Dialog::ConfirmWorkspaceEdit { label, edit } => {
                let files: Vec<String> = edit
                    .changes
                    .iter()
                    .map(|(uri, edits)| {
                        format!(
                            "{}  ({} edit{})",
                            apex_project::lsp::path_for(uri).map(|p| self.display_path(&p)).unwrap_or_else(|| uri.clone()),
                            edits.len(),
                            if edits.len() == 1 { "" } else { "s" }
                        )
                    })
                    .collect();
                let warn = self.theme.ui.warning;
                let muted = self.theme.ui.text_muted;
                let r = egui::Modal::new(egui::Id::new("workspace-edit")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.heading(format!("{label}: apply changes to {} file{}?", files.len(), if files.len() == 1 { "" } else { "s" }));
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for f in &files {
                            ui.label(f);
                        }
                        for op in &edit.unsupported_operations {
                            ui.label(egui::RichText::new(format!("file {op} operation will be skipped")).color(warn));
                        }
                    });
                    ui.label(egui::RichText::new("Open files are edited in place (undo available); other files are rewritten with a safe save.").color(muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            1
                        } else if ui.button("Cancel").clicked() {
                            2
                        } else {
                            0
                        }
                    })
                    .inner
                });
                match r.inner {
                    1 => {
                        self.dialog = None;
                        self.lsp_confirm_workspace_edit(&edit, true);
                    }
                    2 => {
                        self.dialog = None;
                        self.lsp_confirm_workspace_edit(&edit, false);
                    }
                    _ => {
                        if r.should_close() {
                            self.dialog = None;
                            self.lsp_confirm_workspace_edit(&edit, false);
                        }
                    }
                }
            }
            Dialog::Recovery(entries) => {
                let mut restore: Option<usize> = None;
                let mut delete: Option<usize> = None;
                let mut delete_all = false;
                let mut close = false;
                let theme = self.theme.clone();
                let r = egui::Modal::new(egui::Id::new("recovery")).show(ctx, |ui| {
                    ui.set_width(560.0);
                    ui.heading("Recover unsaved files");
                    ui.label("Apex did not shut down cleanly last time. These snapshots were kept:");
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for (i, e) in entries.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let changed = e
                                    .path
                                    .as_ref()
                                    .and_then(|p| std::fs::metadata(p).ok())
                                    .and_then(|m| m.modified().ok())
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs())
                                    .zip(e.disk_modified_unix)
                                    .is_some_and(|(now, then)| now != then);
                                let label = format!("{}  ({} chars)", e.title, e.text.chars().count());
                                ui.label(label);
                                if changed {
                                    ui.label(egui::RichText::new("file on disk changed since").color(theme.ui.warning));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("Delete").clicked() {
                                        delete = Some(i);
                                    }
                                    if ui.button("Restore").clicked() {
                                        restore = Some(i);
                                    }
                                });
                            });
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete All").clicked() {
                            delete_all = true;
                        }
                        if ui.button("Later").clicked() {
                            close = true;
                        }
                    });
                });
                let mut entries = entries;
                if let Some(i) = restore {
                    let e = entries.remove(i);
                    self.restore_recovery_entry(&e);
                }
                if let Some(i) = delete {
                    let e = entries.remove(i);
                    RecoveryEntry::remove(&e.id);
                }
                if delete_all {
                    for e in &entries {
                        RecoveryEntry::remove(&e.id);
                    }
                    entries.clear();
                }
                if entries.is_empty() || close || r.should_close() {
                    self.dialog = None;
                } else {
                    self.dialog = Some(Dialog::Recovery(entries));
                }
            }
            _ => {}
        }
    }

    /// Commands handled by this module. Returns false if not handled.
    pub(crate) fn execute_workspace(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::OpenFolder | Command::AddFolder => {
                let title = if cmd == Command::OpenFolder { "Open Folder" } else { "Add Folder to Workspace" };
                let spec = crate::file_dialogs::FileDialogSpec::open_folder(title).directory(self.roots.first().cloned());
                self.show_file_dialog(spec, crate::file_dialogs::FileDialogKind::OpenFolder { replace: cmd == Command::OpenFolder });
            }
            Command::RemoveFolder => {
                let target =
                    self.sidebar.selected.clone().and_then(|s| self.roots.iter().find(|r| s.starts_with(r)).cloned()).or_else(|| self.roots.last().cloned());
                if let Some(r) = target {
                    self.remove_root(&r);
                }
            }
            Command::CloseProject => {
                self.roots.clear();
                self.project_file = None;
                self.roots_changed();
            }
            Command::RefreshExplorer => {
                self.sidebar.refresh_all();
                self.restart_index();
            }
            Command::RevealActiveFile => {
                if let Some(p) = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    self.settings.sidebar = true;
                    self.sidebar.reveal(&p);
                }
            }
            Command::FocusSidebar => self.settings.sidebar = true,
            Command::NewFileInFolder | Command::NewFolder => {
                let dir = self
                    .sidebar
                    .selected
                    .clone()
                    .map(|s| if s.is_dir() { s } else { s.parent().map(Path::to_path_buf).unwrap_or(s) })
                    .or_else(|| self.roots.first().cloned())
                    .or_else(|| self.current_dir());
                let Some(dir) = dir else {
                    self.set_status("Open a folder first".into(), true);
                    return true;
                };
                if cmd == Command::NewFileInFolder {
                    self.prompt(&format!("New file in {}", self.display_path(&dir)), "", PromptKind::NewFile(dir));
                } else {
                    self.prompt(&format!("New folder in {}", self.display_path(&dir)), "", PromptKind::NewFolder(dir));
                }
            }
            Command::RenameFile => {
                if let Some(p) = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    self.settings.sidebar = true;
                    self.sidebar.reveal(&p);
                    self.sidebar.renaming = Some((p.clone(), p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    self.sidebar.focus_rename = true;
                }
            }
            Command::DeleteFile => {
                if let Some(p) = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    self.dialog = Some(Dialog::ConfirmDelete(p));
                }
            }
            Command::DuplicateFile => {
                if let Some(p) = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone()) {
                    self.duplicate_path(&p);
                }
            }
            Command::SaveProject => {
                let default = self
                    .roots
                    .first()
                    .map(|r| r.join(format!("{}.apex-project", r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "workspace".into()))))
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.prompt("Save project file as", &default, PromptKind::SaveProject);
            }
            Command::OpenProject => {
                let spec = crate::file_dialogs::FileDialogSpec::open_file("Open Project").filter("Apex project", &["apex-project", "toml"]);
                self.show_file_dialog(spec, crate::file_dialogs::FileDialogKind::OpenProject);
            }
            Command::RecentProjects => self.open_list_palette(PaletteMode::RecentProjects),
            Command::OpenProjectSettings => {
                if let Some(p) = self.project_file.clone() {
                    self.open_path(&p);
                } else {
                    self.set_status("Save a project file first (Project ▸ Save Project As…)".into(), false);
                }
            }
            Command::InsertSnippet => self.open_snippet_palette(),
            Command::CommandPalette => self.open_command_palette(),
            Command::GotoAnything => self.open_goto_anything(arg.as_deref().unwrap_or("")),
            Command::GotoSymbol => self.open_symbol_palette(""),
            Command::GotoSymbolInProject => self.open_project_symbol_palette(),
            Command::NavigateBack => self.jump(true),
            Command::NavigateForward => self.jump(false),
            Command::FindInFiles => self.open_find_in_files(None),
            Command::TogglePanel | Command::ShowOutput => {
                self.panel_open = !self.panel_open;
                if self.panel_open {
                    self.fif.open = true;
                }
            }
            Command::RecordMacro => self.toggle_macro_recording(),
            Command::PlayMacro => {
                let steps = self.last_macro.clone();
                if steps.is_empty() {
                    self.set_status("No macro recorded yet (Tools ▸ Start Recording Macro)".into(), false);
                } else {
                    self.play_macro(ctx, &steps);
                }
            }
            Command::SaveMacro => {
                if self.last_macro.is_empty() {
                    self.set_status("Record a macro first".into(), false);
                } else {
                    self.prompt("Macro name", "", PromptKind::MacroName);
                }
            }
            Command::RunNamedMacro => match arg {
                Some(name) => self.run_macro_named(ctx, &name),
                None => self.open_list_palette(PaletteMode::Macros),
            },
            Command::ShowRecovery => {
                let entries = RecoveryEntry::list();
                if entries.is_empty() {
                    self.set_status("No recoverable files".into(), false);
                } else {
                    self.dialog = Some(Dialog::Recovery(entries));
                }
            }
            Command::ShowClipboardHistory => self.open_list_palette(PaletteMode::ClipboardHistory),
            Command::SelectTheme if arg.is_none() => self.open_list_palette(PaletteMode::Themes),
            Command::SetLanguage if arg.is_none() => self.open_list_palette(PaletteMode::Languages),
            Command::SetEncoding if arg.is_none() => self.open_list_palette(PaletteMode::Encodings),
            Command::SetLineEnding if arg.is_none() => self.open_list_palette(PaletteMode::LineEndings),
            Command::OpenRecent if arg.is_none() => self.open_list_palette(PaletteMode::RecentFiles),
            Command::SetMark if arg.is_none() => self.prompt("Mark name", "", PromptKind::MarkName(true)),
            Command::GotoMark if arg.is_none() => self.prompt("Go to mark", "", PromptKind::MarkName(false)),
            Command::InsertSequence if arg.is_none() => self.prompt("Sequence start[,step]", "1,1", PromptKind::Sequence),
            Command::SurroundWith if arg.is_none() => self.prompt("Surround with (e.g. () or \")", "", PromptKind::Surround),
            Command::KeyboardShortcuts | Command::ShowKeyboardCheatSheet => {
                // The command palette shows every command with its shortcut; open it filtered to nothing.
                self.open_command_palette();
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn startup_recovery_check(&mut self) {
        if self.settings.recovery && RecoveryEntry::exists_any() {
            let entries = RecoveryEntry::list();
            // Entries for documents already restored by the session are not offered again.
            let known: Vec<String> = self.recovery_ids.values().map(|(id, _)| id.clone()).collect();
            let entries: Vec<RecoveryEntry> = entries.into_iter().filter(|e| !known.contains(&e.id)).collect();
            if !entries.is_empty() {
                self.dialog = Some(Dialog::Recovery(entries));
            }
        }
    }

    pub(crate) fn tab_is_preview(&self, v: ViewId) -> bool {
        self.is_preview(v)
    }
}

fn toml_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("{s:?}")).collect();
    format!("[{}]", inner.join(", "))
}

/// Relative path from `base` to `target` using `..` where needed.
fn pathdiff_relative(base: &Path, target: &Path) -> String {
    let base: Vec<_> = base.components().collect();
    let target_c: Vec<_> = target.components().collect();
    let common = base.iter().zip(target_c.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &target_c[common..] {
        out.push(c);
    }
    let s = out.display().to_string().replace('\\', "/");
    if s.is_empty() { ".".into() } else { s }
}

#[allow(dead_code)]
fn _unused(_: &VecDeque<Command>, _: Category, _: AfterClose) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(pathdiff_relative(Path::new("/a/b"), Path::new("/a/b/c")), "c");
        assert_eq!(pathdiff_relative(Path::new("/a/b"), Path::new("/a/x")), "../x");
        assert_eq!(pathdiff_relative(Path::new("/a/b"), Path::new("/a/b")), ".");
        assert_eq!(toml_array(&["x".into(), "y z".into()]), "[\"x\", \"y z\"]");
    }
}
