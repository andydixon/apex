//! Paths that arrive while the editor is running (Finder "Open With", a
//! second `apex` invocation). The sender half lives for the process; the
//! application polls the receiver every frame.

use std::path::PathBuf;
use std::sync::OnceLock;

use apex_ui::OpenRequest;

static SENDER: OnceLock<crossbeam_channel::Sender<OpenRequest>> = OnceLock::new();
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Create the channel; returns the receiver for `StartupArgs::inbox`.
pub fn init() -> crossbeam_channel::Receiver<OpenRequest> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let _ = SENDER.set(tx);
    rx
}

/// Remember the UI context so pushes can wake the event loop.
pub fn set_context(ctx: egui::Context) {
    let _ = CTX.set(ctx);
}

/// Queue paths for the running editor. Directories become workspace folders.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn push(paths: Vec<PathBuf>) {
    let mut req = OpenRequest::default();
    for p in paths {
        if p.is_dir() {
            req.folders.push(p);
        } else {
            req.files.push((p, None, None));
        }
    }
    push_request(req);
}

pub fn push_request(req: OpenRequest) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(req);
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}
