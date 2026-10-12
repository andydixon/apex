//! Native open and save panels on macOS, shown without deadlocking AppKit.
//!
//! # Why rfd is not used on macOS
//!
//! Creating an `NSOpenPanel` or `NSSavePanel` makes AppKit connect to the
//! system's separate panel service (`com.apple.appkit.xpc.openAndSavePanelService`).
//! `-[NSSavePanel _initBridgeAndStuff]` waits for that connection by spinning
//! a *nested* run loop on the main thread, and the service's reply is
//! delivered through the main dispatch queue (GCD).
//!
//! CoreFoundation never drains the main dispatch queue from a nested run loop
//! that is itself running inside a main-queue block, so the panel can only be
//! created from a plain run-loop callout. That rules out both ways rfd can be
//! driven from an eframe app:
//!
//! * From a worker thread, rfd uses `dispatch_sync` onto the main queue. The
//!   panel is then created *inside* a main-queue block, the reply can never
//!   be delivered, and the whole process hangs. This is exactly what the
//!   `sample` trace of the hang showed (`_dispatch_main_queue_drain` →
//!   `pick_folder` → `_initBridgeAndStuff` → `HIRunLoopSemaphore wait:`).
//! * From the UI thread, eframe's `update` runs inside winit's `drawRect:`,
//!   that is in the middle of a view drawing pass. Nesting a modal run loop
//!   there froze the editor before the panel appeared.
//!
//! # What this module does instead
//!
//! The panel is created and begun from a block queued on the main run loop
//! with `CFRunLoopPerformBlock` in the common modes. Such a block runs as an
//! ordinary run-loop callout after the current event has been handled:
//! outside any GCD block and outside any drawing pass, so AppKit's nested
//! wait can still service the main queue and the panel service attaches.
//!
//! The panel is shown as a sheet on the editor window (or as a modeless
//! panel if there is no window yet) with `beginSheetModalForWindow:`, which
//! returns immediately. Nothing blocks: winit keeps running the event loop,
//! and the completion handler sends the chosen paths down a channel that
//! `file_dialogs::poll_file_dialog` reads every frame.
//!
//! Filters (allowed file types) are not applied on macOS: the only caller
//! that sets one is Open Project, where the panel's own type column already
//! makes the right file easy to find.
//! ponytail: add `objc2-uniform-type-identifiers` and `setAllowedContentTypes`
//! if a dialog ever needs to restrict what can be chosen.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSSavePanel};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use objc2_foundation::{NSString, NSURL};

use crate::file_dialogs::{FileDialogSpec, Mode};

/// Queue the panel on the main run loop. Must be called on the main thread
/// (the egui `update` is); `tx` receives the chosen paths, or an empty list
/// when the user cancels or the panel cannot be shown.
pub(crate) fn start(spec: FileDialogSpec, tx: Sender<Vec<PathBuf>>, ctx: egui::Context) {
    let Some(main) = CFRunLoop::main() else {
        tracing::warn!("macOS file dialog not shown: no main run loop");
        let _ = tx.send(Vec::new());
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
fn present(spec: &FileDialogSpec, tx: Sender<Vec<PathBuf>>, ctx: egui::Context) {
    let Some(mtm) = MainThreadMarker::new() else {
        // Cannot happen for a block on the main run loop; answer rather than hang.
        tracing::warn!("macOS file dialog block ran off the main thread");
        let _ = tx.send(Vec::new());
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let panel: Retained<NSSavePanel> = match spec.mode {
        Mode::SaveFile => NSSavePanel::savePanel(mtm),
        mode => {
            let p = NSOpenPanel::openPanel(mtm);
            p.setCanChooseFiles(mode != Mode::PickFolder);
            p.setCanChooseDirectories(mode == Mode::PickFolder);
            p.setAllowsMultipleSelection(mode == Mode::PickFiles);
            // Folders a user picks as a workspace are often ones they are about to create.
            p.setCanCreateDirectories(mode == Mode::PickFolder);
            Retained::into_super(p)
        }
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
            Some(panel) if response == NSModalResponseOK => chosen_paths(&panel),
            // Cancelled, aborted by the system, or (impossibly) called twice.
            _ => Vec::new(),
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
