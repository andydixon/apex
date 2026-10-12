//! Native file dialogs without blocking the event loop.
//!
//! Every dialog runs on a worker thread and its result is handled on the UI
//! thread in `poll_file_dialog`, so the window keeps repainting. Linux and
//! Windows use rfd (portal / shell dialog). On macOS AppKit panels must be
//! shown from the main thread, and both rfd paths (a nested `runModal`, or a
//! sheet started from inside winit's event handler) hung the editor before
//! the panel appeared; there the standard dialog is shown by `osascript` in
//! its own process instead, so nothing in Apex's run loop can block on it.
//! One dialog at a time.

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

    /// macOS: the standard dialog via `osascript` (see the module comment).
    /// Values travel as arguments, never inside the script text.
    #[cfg(target_os = "macos")]
    fn run_sync(&self) -> Vec<PathBuf> {
        let dir = self.directory.as_ref().filter(|d| d.is_dir()).map(|d| d.display().to_string());
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(apple_script(&self.mode, dir.is_some(), self.file_name.is_some()).iter().flat_map(|l| ["-e", l]))
            .arg(&self.title)
            .arg(dir.unwrap_or_default())
            .arg(self.file_name.clone().unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .output();
        // ponytail: no type filter on macOS (`of type` needs UTIs); the
        // caller validates what it opens.
        match output {
            // Cancel exits with status 1 ("User canceled. (-128)").
            Ok(o) if o.status.success() => parse_osascript_paths(&String::from_utf8_lossy(&o.stdout)),
            Ok(_) => Vec::new(),
            Err(e) => {
                tracing::error!("could not run osascript for the file dialog: {e}");
                Vec::new()
            }
        }
    }
}

/// AppleScript for one dialog. Arguments: 1 = prompt, 2 = folder, 3 = name.
/// Prints one POSIX path per line.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn apple_script(mode: &Mode, with_dir: bool, with_name: bool) -> Vec<String> {
    let loc = if with_dir { " default location (POSIX file (item 2 of argv))" } else { "" };
    let choose = match mode {
        Mode::PickFolder => format!("choose folder with prompt (item 1 of argv){loc}"),
        Mode::PickFile => format!("choose file with prompt (item 1 of argv){loc}"),
        Mode::PickFiles => format!("choose file with prompt (item 1 of argv){loc} with multiple selections allowed"),
        Mode::SaveFile => {
            let name = if with_name { " default name (item 3 of argv)" } else { "" };
            format!("choose file name with prompt (item 1 of argv){name}{loc}")
        }
    };
    [
        "on run argv",
        // Bring osascript's dialog in front of the editor.
        "activate",
        &format!("set picked to {choose}"),
        "if class of picked is not list then set picked to {picked}",
        "set out to \"\"",
        "repeat with f in picked",
        "set out to out & POSIX path of (contents of f) & linefeed",
        "end repeat",
        "return out",
        "end run",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_osascript_paths(out: &str) -> Vec<PathBuf> {
    out.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        // Folders come back with a trailing slash.
        .map(|l| PathBuf::from(if l.len() > 1 { l.trim_end_matches('/') } else { l }))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osascript_output_and_script() {
        assert_eq!(parse_osascript_paths("/Users/a/Code/\n"), vec![PathBuf::from("/Users/a/Code")]);
        assert_eq!(parse_osascript_paths("/a b/x.rs\n/c.txt\n"), vec![PathBuf::from("/a b/x.rs"), PathBuf::from("/c.txt")]);
        assert_eq!(parse_osascript_paths("/\n"), vec![PathBuf::from("/")]);
        assert!(parse_osascript_paths("").is_empty());
        let s = apple_script(&Mode::SaveFile, true, true).join("\n");
        assert!(s.contains("choose file name with prompt (item 1 of argv) default name (item 3 of argv) default location (POSIX file (item 2 of argv))"));
        assert!(apple_script(&Mode::PickFolder, false, false).join("\n").contains("set picked to choose folder with prompt (item 1 of argv)\n"));
    }
}
