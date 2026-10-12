//! File dialogs that never block the event loop.
//!
//! Each platform gets the mechanism that is safe for it:
//!
//! * **Linux and Windows:** rfd's blocking API on a worker thread (the
//!   portal / GTK and Win32 dialogs are happy off the UI thread). The answer
//!   comes back through a channel.
//! * **macOS:** AppKit's panels must be created from a plain main-thread
//!   run-loop callout, neither from a GCD block (what rfd does for worker
//!   threads, which deadlocks) nor from inside winit's drawing pass (where
//!   egui's `update` runs). `macos_dialogs` queues the panel on the main run
//!   loop and shows it as a sheet; see that module for the full story.
//! * **Any platform, `native_file_dialogs` off:** an in-app path prompt with
//!   Tab completion. This is also the fallback when a dialog cannot start.
//!
//! In every case the result is handled on the UI thread in
//! `poll_file_dialog`, so the window keeps repainting while the dialog is up.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use crate::app::{ApexApp, Dialog, normalise_path};
use crate::view::ViewId;

/// What to do with the chosen paths.
#[derive(Debug, Clone)]
pub(crate) enum FileDialogKind {
    OpenFiles,
    /// `replace`: Open Folder (new workspace) rather than Add Folder.
    OpenFolder {
        replace: bool,
    },
    OpenProject,
    /// Save the view's document under the chosen path, then show `then`
    /// (used to resume a close-with-save sequence).
    SaveAs {
        vid: ViewId,
        then: Option<Box<Dialog>>,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    PickFiles,
    PickFolder,
    PickFile,
    SaveFile,
}

/// Dialog parameters, independent of the backend.
pub struct FileDialogSpec {
    pub(crate) title: String,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) file_name: Option<String>,
    /// `(name, extensions)`; honoured by rfd, ignored by the macOS panel.
    pub(crate) filter: Option<(String, Vec<String>)>,
    pub(crate) mode: Mode,
}

impl FileDialogSpec {
    pub fn open_files(title: &str) -> Self {
        Self::new(title, Mode::PickFiles)
    }
    pub fn open_file(title: &str) -> Self {
        Self::new(title, Mode::PickFile)
    }
    pub fn open_folder(title: &str) -> Self {
        Self::new(title, Mode::PickFolder)
    }
    pub fn save_file(title: &str) -> Self {
        Self::new(title, Mode::SaveFile)
    }
    fn new(title: &str, mode: Mode) -> Self {
        Self { title: title.to_string(), directory: None, file_name: None, filter: None, mode }
    }
    pub fn directory(mut self, dir: Option<PathBuf>) -> Self {
        self.directory = dir;
        self
    }
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }
    pub fn filter(mut self, name: &str, extensions: &[&str]) -> Self {
        self.filter = Some((name.to_string(), extensions.iter().map(|e| e.to_string()).collect()));
        self
    }

    #[cfg(not(target_os = "macos"))]
    fn sync_dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new().set_title(&self.title);
        if let Some(dir) = &self.directory {
            d = d.set_directory(dir);
        }
        if let Some(n) = &self.file_name {
            d = d.set_file_name(n);
        }
        if let Some((name, exts)) = &self.filter {
            d = d.add_filter(name, exts);
        }
        d
    }

    /// Run the blocking dialog (on a worker thread). Empty = cancelled.
    #[cfg(not(target_os = "macos"))]
    fn run_sync(&self) -> Vec<PathBuf> {
        let d = self.sync_dialog();
        match self.mode {
            Mode::PickFiles => d.pick_files().unwrap_or_default(),
            Mode::PickFolder => d.pick_folder().into_iter().collect(),
            Mode::PickFile => d.pick_file().into_iter().collect(),
            Mode::SaveFile => d.save_file().into_iter().collect(),
        }
    }
}

pub struct PendingFileDialog {
    kind: FileDialogKind,
    rx: Receiver<Vec<PathBuf>>,
}

impl PendingFileDialog {
    fn start(spec: FileDialogSpec, kind: FileDialogKind, ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        #[cfg(target_os = "macos")]
        crate::macos_dialogs::start(spec, tx, ctx);
        #[cfg(not(target_os = "macos"))]
        std::thread::Builder::new()
            .name("apex-file-dialog".into())
            .spawn(move || {
                let _ = tx.send(spec.run_sync());
                // Wake the UI only when there is an answer: repainting while
                // the dialog is up makes no difference to it.
                ctx.request_repaint();
            })
            .ok();
        Self { kind, rx }
    }

    /// Some(paths) once the user has answered; empty = cancelled.
    fn poll(&mut self) -> Option<Vec<PathBuf>> {
        match self.rx.try_recv() {
            Ok(v) => Some(v),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Vec::new()),
        }
    }
}

impl ApexApp {
    /// Show a native dialog (or the path prompt when `native_file_dialogs`
    /// is off); the result arrives through `poll_file_dialog`.
    pub(crate) fn show_file_dialog(&mut self, spec: FileDialogSpec, kind: FileDialogKind) {
        if !self.settings.native_file_dialogs {
            self.prompt_for_path(&spec, kind);
            return;
        }
        let Some(ctx) = self.egui_ctx.clone() else { return };
        // On macOS the sheet blocks the window, so a second request while one
        // is up can only come from a modeless panel; keep the first, it will
        // answer. Elsewhere a dialog that never answered must not block the
        // next one: the new request replaces it and a late answer is ignored.
        if cfg!(target_os = "macos") && self.file_dialog.is_some() {
            return;
        }
        self.file_dialog = Some(PendingFileDialog::start(spec, kind, ctx));
    }

    /// The in-app alternative: a path prompt with Tab completion.
    pub(crate) fn prompt_for_path(&mut self, spec: &FileDialogSpec, kind: FileDialogKind) {
        let mut initial = spec
            .directory
            .clone()
            .or_else(|| self.roots.first().cloned())
            .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        if !initial.is_empty() && !initial.ends_with(std::path::MAIN_SEPARATOR) {
            initial.push(std::path::MAIN_SEPARATOR);
        }
        if let Some(n) = &spec.file_name {
            initial.push_str(n);
        }
        let title = match spec.mode {
            Mode::PickFolder => format!("{} (folder path; Tab completes)", spec.title),
            _ => format!("{} (file path; Tab completes)", spec.title),
        };
        self.prompt(&title, &initial, crate::workspace_ui::PromptKind::Path(Box::new(kind), spec.mode == Mode::PickFolder));
    }

    /// Called every frame: deliver a finished dialog's result.
    pub(crate) fn poll_file_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.file_dialog else { return };
        let Some(paths) = pending.poll() else { return };
        let Some(PendingFileDialog { kind, .. }) = self.file_dialog.take() else { return };
        self.file_dialog_result(kind, paths);
        ctx.request_repaint();
    }

    pub(crate) fn file_dialog_result(&mut self, kind: FileDialogKind, paths: Vec<PathBuf>) {
        match kind {
            FileDialogKind::OpenFiles => {
                for p in paths {
                    self.open_path(&p);
                }
            }
            FileDialogKind::OpenFolder { replace } => {
                if let Some(p) = paths.first() {
                    if replace {
                        self.open_folder(p);
                    } else {
                        self.add_root(p);
                    }
                    self.settings.sidebar = true;
                }
            }
            FileDialogKind::OpenProject => {
                if let Some(p) = paths.first() {
                    self.open_project_file(p);
                }
            }
            FileDialogKind::SaveAs { vid, then } => {
                if let Some(p) = paths.first()
                    && self.save_view_to(vid, normalise_path(p))
                    && then.is_some()
                {
                    self.dialog = then.map(|d| *d);
                }
            }
        }
    }

    /// Ask for a path and save the view's document there; `then` is shown
    /// after a successful save (e.g. the next step of closing tabs).
    pub(crate) fn save_view_as_then(&mut self, vid: ViewId, then: Option<Dialog>) {
        let then = then.map(Box::new);
        let Some(view) = self.views.get(&vid) else { return };
        let Some(doc) = self.docs.get(&view.doc) else { return };
        let mut spec = FileDialogSpec::save_file("Save As");
        if let Some(p) = &doc.path {
            spec = spec.directory(p.parent().map(|d| d.to_path_buf()));
            if let Some(name) = p.file_name() {
                spec = spec.file_name(name.to_string_lossy());
            }
        } else {
            spec = spec.directory(self.current_dir()).file_name(format!("{}.txt", doc.title().to_lowercase().replace(' ', "-")));
        }
        self.show_file_dialog(spec, FileDialogKind::SaveAs { vid, then });
    }
}

/// Tab completion for a typed path: the completed text and the candidates
/// (names, with a trailing separator for folders) when it is ambiguous.
pub(crate) fn complete_path(text: &str, folders_only: bool) -> (String, Vec<String>) {
    let sep = std::path::MAIN_SEPARATOR;
    let expanded = match text.strip_prefix('~') {
        Some(rest) => directories::UserDirs::new().map(|u| format!("{}{rest}", u.home_dir().display())).unwrap_or_else(|| text.to_string()),
        None => text.to_string(),
    };
    let (dir, prefix) = match expanded.rfind(['/', sep]) {
        Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
        None => return (text.to_string(), Vec::new()),
    };
    let Ok(rd) = std::fs::read_dir(&dir) else { return (expanded, Vec::new()) };
    let lower = prefix.to_lowercase();
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.path().is_dir();
            (name.to_lowercase().starts_with(&lower) && (is_dir || !folders_only) && (!name.starts_with('.') || prefix.starts_with('.')))
                .then(|| if is_dir { format!("{name}{sep}") } else { name })
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    match names.len() {
        0 => (expanded, Vec::new()),
        1 => (format!("{dir}{}", names[0]), Vec::new()),
        _ => {
            // Longest common prefix (case-insensitive match, keep the first's case).
            let first: Vec<char> = names[0].chars().collect();
            let mut n = first.len();
            for other in &names[1..] {
                n = n.min(first.iter().zip(other.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count());
            }
            let common: String = first[..n].iter().collect();
            let completed = if common.chars().count() > prefix.chars().count() { format!("{dir}{common}") } else { expanded };
            (completed, names)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_completion() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::create_dir(root.join("alpine")).unwrap();
        std::fs::write(root.join("beta.txt"), "").unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        let base = format!("{}{sep}", root.display());
        let (t, c) = complete_path(&format!("{base}al"), false);
        assert_eq!(t, format!("{base}alp"));
        assert_eq!(c, vec![format!("alpha{sep}"), format!("alpine{sep}")]);
        let (t, c) = complete_path(&format!("{base}alph"), true);
        assert_eq!(t, format!("{base}alpha{sep}"));
        assert!(c.is_empty());
        assert_eq!(complete_path(&format!("{base}be"), false).0, format!("{base}beta.txt"));
        assert_eq!(complete_path(&format!("{base}be"), true).0, format!("{base}be"));
        assert_eq!(complete_path("nothing", false).0, "nothing");
    }
}
