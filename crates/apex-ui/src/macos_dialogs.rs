//! Native macOS file panels, created on the main run loop and shown as sheets.
//!
//! AppKit must create these panels on the main thread, outside the editor's
//! drawing pass and any main dispatch-queue callback. Asynchronous completion
//! returns paths through a channel. A failed panel returns an error instead of
//! panicking; macOS panel-service initialisation can still block the main thread.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::{NSApplication, NSModalResponse, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel, NSSavePanel};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use objc2_foundation::{NSString, NSURL};

use crate::file_dialogs::{FileDialogSpec, Mode};

/// Queue the panel on the main run loop. Must be called on the main thread
/// (the egui `update` is); `tx` receives the chosen paths, or an empty list
/// when the user cancels, or an error when the panel cannot be shown.
pub(crate) fn start(spec: FileDialogSpec, tx: Sender<Result<Vec<PathBuf>, String>>, ctx: egui::Context) {
    let Some(main) = CFRunLoop::main() else {
        let _ = tx.send(Err("macOS file dialog has no main run loop".into()));
        ctx.request_repaint();
        return;
    };
    // `Fn` because that is the block ABI; CoreFoundation runs it exactly once.
    let block = RcBlock::new(move || present(&spec, tx.clone(), ctx.clone()));
    // SAFETY: the main run loop, a static mode name, and a block copied by
    // CoreFoundation before this function returns.
    unsafe {
        main.perform_block(kCFRunLoopCommonModes.map(|m| &**m), Some(&block));
    }
    // The loop is awake already (we are inside an event), but this is the
    // documented pairing and costs nothing.
    main.wake_up();
}

/// Runs on the main thread from the run loop: build the panel from `spec`,
/// show it and hand the result to `tx` when it closes.
fn present(spec: &FileDialogSpec, tx: Sender<Result<Vec<PathBuf>, String>>, ctx: egui::Context) {
    let Some(mtm) = MainThreadMarker::new() else {
        // Cannot happen for a block on the main run loop; answer rather than hang.
        let _ = tx.send(Err("macOS file dialog ran off the main thread".into()));
        ctx.request_repaint();
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // The generated bindings assume non-null, but AppKit returned nil after a
    // panel-service failure during testing. Preserve that as a recoverable error.
    let panel: Option<Retained<NSSavePanel>> = match spec.mode {
        // SAFETY: the AppKit class and selector match the binding; we are on the main thread.
        Mode::SaveFile => unsafe { msg_send![NSSavePanel::class(), savePanel] },
        mode => {
            // SAFETY: the AppKit class and selector match the binding; we are on the main thread.
            let p: Option<Retained<NSOpenPanel>> = unsafe { msg_send![NSOpenPanel::class(), openPanel] };
            let Some(p) = p else {
                let _ = tx.send(Err("macOS could not create the Open panel".into()));
                ctx.request_repaint();
                return;
            };
            p.setCanChooseFiles(mode != Mode::PickFolder);
            p.setCanChooseDirectories(mode == Mode::PickFolder);
            p.setAllowsMultipleSelection(mode == Mode::PickFiles);
            // Folders a user picks as a workspace are often ones they are about to create.
            p.setCanCreateDirectories(mode == Mode::PickFolder);
            Some(Retained::into_super(p))
        }
    };
    let Some(panel) = panel else {
        let _ = tx.send(Err("macOS could not create the Save panel".into()));
        ctx.request_repaint();
        return;
    };
    // `message` is the text above the file list; the title bar of a sheet is not shown.
    panel.setMessage(Some(&NSString::from_str(&spec.title)));
    if let Some(dir) = spec.directory.as_ref().and_then(|d| d.to_str()) {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath_isDirectory(&NSString::from_str(dir), true)));
    }
    if let Some(name) = &spec.file_name {
        panel.setNameFieldStringValue(&NSString::from_str(name));
    }

    // The completion handler must not capture the panel directly: the panel
    // retains its handler, and a handler retaining the panel would leak both.
    // Holding it in a shared cell that the handler empties breaks the cycle.
    let slot: Rc<RefCell<Option<Retained<NSSavePanel>>>> = Rc::new(RefCell::new(Some(panel.clone())));
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let paths = match slot.borrow_mut().take() {
            Some(panel) if response == NSModalResponseOK => Ok(chosen_paths(&panel)),
            Some(_) if response == NSModalResponseCancel => Ok(Vec::new()),
            _ => Err("macOS could not complete the file dialog".into()),
        };
        let _ = tx.send(paths);
        // Wake the UI thread so `poll_file_dialog` sees the answer promptly.
        ctx.request_repaint();
    });
    match app.keyWindow().or_else(|| app.mainWindow()) {
        Some(window) => panel.beginSheetModalForWindow_completionHandler(&window, &handler),
        None => panel.beginWithCompletionHandler(&handler),
    }
}

/// The selected paths: every URL of an open panel, the one URL of a save panel.
fn chosen_paths(panel: &NSSavePanel) -> Vec<PathBuf> {
    let to_path = |url: &NSURL| url.path().map(|p| PathBuf::from(p.to_string()));
    match panel.downcast_ref::<NSOpenPanel>() {
        Some(open) => open.URLs().iter().filter_map(|u| to_path(&u)).collect(),
        None => panel.URL().and_then(|u| to_path(&u)).into_iter().collect(),
    }
}
