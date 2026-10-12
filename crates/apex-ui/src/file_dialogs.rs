//! Native file dialogs without blocking the event loop.
//!
//! Every dialog is started from a worker thread with rfd's blocking API and
//! its result is handled on the UI thread in `poll_file_dialog`, so the
//! window keeps repainting. On macOS rfd hands the panel to the main thread
//! through the main dispatch queue, so it runs from the run loop *outside*
//! winit's event handler: starting it inside the handler (a nested
//! `runModal`, or a sheet) hung the editor before the panel appeared, and a
//! panel shown by a separate `osascript` process stayed behind the window.
//! Only the latest dialog's answer is used.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use crate::app::{ApexApp, Dialog, normalise_path};
use crate::view::ViewId;

/// What to do with the chosen paths.
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
        then: Option<Dialog>,
    },
}

enum Mode {
    PickFiles,
    PickFolder,
    PickFile,
    SaveFile,
}

/// Dialog parameters, independent of the backend.
pub struct FileDialogSpec {
    title: String,
    directory: Option<PathBuf>,
    file_name: Option<String>,
    filter: Option<(String, Vec<String>)>,
    mode: Mode,
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
    fn start(spec: FileDialogSpec, kind: FileDialogKind) -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("apex-file-dialog".into())
            .spawn(move || {
                let _ = tx.send(spec.run_sync());
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
    /// Show a native dialog; the result arrives through `poll_file_dialog`.
    pub(crate) fn show_file_dialog(&mut self, spec: FileDialogSpec, kind: FileDialogKind) {
        // A dialog that never answered must not block the next one: the
        // new request replaces it and a late answer is ignored.
        self.file_dialog = Some(PendingFileDialog::start(spec, kind));
    }

    /// Called every frame: deliver a finished dialog's result.
    pub(crate) fn poll_file_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.file_dialog else { return };
        let Some(paths) = pending.poll() else {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        };
        let Some(PendingFileDialog { kind, .. }) = self.file_dialog.take() else { return };
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
                    self.dialog = then;
                }
            }
        }
        ctx.request_repaint();
    }

    /// Ask for a path and save the view's document there; `then` is shown
    /// after a successful save (e.g. the next step of closing tabs).
    pub(crate) fn save_view_as_then(&mut self, vid: ViewId, then: Option<Dialog>) {
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

