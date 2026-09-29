//! macOS integration: Finder "Open With", drag onto the Dock icon and
//! double-clicking an associated document do not pass paths on the command
//! line. They arrive as an `odoc` Apple Event, which winit does not forward,
//! so a handler is registered with `NSAppleEventManager` before the event
//! loop starts and queues the paths for the running editor.
//!
//! Document types are declared in `packaging/macos/Info.plist`.

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSObject, NSObjectProtocol, NSURL};

const fn four_cc(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}
/// `kCoreEventClass`.
const CORE_EVENT_CLASS: u32 = four_cc(b"aevt");
/// `kAEOpenDocuments`.
const OPEN_DOCUMENTS: u32 = four_cc(b"odoc");
/// `keyDirectObject`.
const DIRECT_OBJECT: u32 = four_cc(b"----");

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the type has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ApexOpenDocumentsHandler"]
    struct OpenDocumentsHandler;

    impl OpenDocumentsHandler {
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let mut paths: Vec<PathBuf> = Vec::new();
            // SAFETY: standard NSAppleEventDescriptor messages with their documented
            // argument types (AEKeyword / NSInteger); results are retained objects.
            let list: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
            if let Some(list) = list {
                let n: isize = unsafe { msg_send![&*list, numberOfItems] };
                for i in 1..=n.max(0) {
                    let item: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![&*list, descriptorAtIndex: i] };
                    let Some(item) = item else { continue };
                    let url: Option<Retained<NSURL>> = unsafe { msg_send![&*item, fileURLValue] };
                    if let Some(url) = url
                        && let Some(p) = url.path()
                    {
                        paths.push(PathBuf::from(p.to_string()));
                    }
                }
            }
            if !paths.is_empty() {
                crate::open_requests::push(paths);
            }
        }
    }

    unsafe impl NSObjectProtocol for OpenDocumentsHandler {}
);

/// Register the handler. Must run on the main thread before the app launches.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("Apple Event handler not installed: not on the main thread");
        return;
    };
    let this = OpenDocumentsHandler::alloc(mtm).set_ivars(());
    // SAFETY: plain NSObject init on a freshly allocated instance.
    let handler: Retained<OpenDocumentsHandler> = unsafe { msg_send![super(this), init] };
    // SAFETY: the shared manager is a singleton; the handler object is leaked so
    // it outlives every event the process will receive.
    unsafe {
        let manager: Retained<AnyObject> = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![
            &*manager,
            setEventHandler: &*handler as &AnyObject,
            andSelector: sel!(handleOpenDocuments:withReplyEvent:),
            forEventClass: CORE_EVENT_CLASS,
            andEventID: OPEN_DOCUMENTS
        ];
    }
    std::mem::forget(handler);
    let _ = OpenDocumentsHandler::class();
}
