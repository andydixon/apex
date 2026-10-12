//! Native file dialogs without blocking the event loop.
//!
//! rfd's synchronous dialogs run a nested modal loop. On macOS that deadlocks
//! inside winit's event dispatch (Open Folder froze the editor), so the
//! asynchronous dialog is polled from the frame loop there. On Linux and
//! Windows the synchronous dialog runs on its own thread, which also keeps the
//! window responsive while the portal or shell dialog is open. One dialog at a
//! time; its result is handled on the UI thread in `poll_file_dialog`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, channel};
use std::task::{Context, Poll, Waker};
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

    fn async_dialog(&self) -> rfd::AsyncFileDialog {
        let mut d = rfd::AsyncFileDialog::new().set_title(&self.title);
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

    /// The non-blocking dialog as a future (polled from the frame loop).
    fn run_async(&self) -> Pin<Box<dyn Future<Output = Vec<PathBuf>>>> {
        let d = self.async_dialog();
        match self.mode {
            Mode::PickFiles => Box::pin(async move { d.pick_files().await.map(|v| v.iter().map(|h| h.path().to_path_buf()).collect()).unwrap_or_default() }),
            Mode::PickFolder => Box::pin(async move { d.pick_folder().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default() }),
            Mode::PickFile => Box::pin(async move { d.pick_file().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default() }),
            Mode::SaveFile => Box::pin(async move { d.save_file().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default() }),
        }
    }
}

enum Backend {
    Thread(Receiver<Vec<PathBuf>>),
    Future(Pin<Box<dyn Future<Output = Vec<PathBuf>>>>),
}

pub struct PendingFileDialog {
    kind: FileDialogKind,
    backend: Backend,
}

impl PendingFileDialog {
    fn start(spec: FileDialogSpec, kind: FileDialogKind) -> Self {
        // macOS: NSOpenPanel must live on the main thread, and runModal would nest
        // the event loop, so use the completion-handler based async API there.
        let use_future = cfg!(target_os = "macos");
        let backend = if use_future {
            Backend::Future(spec.run_async())
        } else {
            let (tx, rx) = channel();
            std::thread::Builder::new()
                .name("apex-file-dialog".into())
                .spawn(move || {
                    let _ = tx.send(spec.run_sync());
                })
                .ok();
            Backend::Thread(rx)
        };
        Self { kind, backend }
    }

    /// Some(paths) once the user has answered; empty = cancelled.
    fn poll(&mut self) -> Option<Vec<PathBuf>> {
        match &mut self.backend {
            Backend::Thread(rx) => match rx.try_recv() {
                Ok(v) => Some(v),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Vec::new()),
            },
            Backend::Future(fut) => {
                let mut cx = Context::from_waker(Waker::noop());
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(v) => Some(v),
                    Poll::Pending => None,
                }
            }
        }
    }
}

impl ApexApp {
    /// Show a native dialog; the result arrives through `poll_file_dialog`.
    pub(crate) fn show_file_dialog(&mut self, spec: FileDialogSpec, kind: FileDialogKind) {
        if self.file_dialog.is_some() {
            self.set_status("A file dialog is already open".into(), false);
            return;
        }
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
