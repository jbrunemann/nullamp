//! Files opened from Finder (double-click, "Open With", drop on the Dock icon).
//!
//! AppKit hands them to the application delegate as `application:openFiles:`, also when the app is already
//! running, and shows "cannot open files in the Audio file format" when the delegate has no such method.
//! winit installs a delegate without it and offers no hook, so the method is added to winit's delegate class
//! at runtime. For a file that launches the app, AppKit asks before `applicationDidFinishLaunching`, so this
//! is done when `NSApplicationWillFinishLaunchingNotification` is posted, the last moment the delegate is
//! known to exist and the first AppKit asks about files.

use std::path::PathBuf;
use std::sync::Once;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{NSObject, NSString};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NullampLaunchObserver"]
    struct LaunchObserver;

    impl LaunchObserver {
        #[unsafe(method(appWillFinishLaunching:))]
        fn app_will_finish_launching(&self, _notification: &AnyObject) {
            add_open_files();
        }
    }
);

/// Ask to be told when the app is about to finish launching. Call on the main thread before the event loop runs.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let observer: Retained<LaunchObserver> = unsafe { msg_send![super(mtm.alloc::<LaunchObserver>().set_ivars(())), init] };
    // The notification center does not retain its observers, so this one lives for the rest of the process.
    let observer: &'static LaunchObserver = unsafe { &*Retained::into_raw(observer) };
    unsafe {
        let center: Retained<AnyObject> = msg_send![objc2::class!(NSNotificationCenter), defaultCenter];
        let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
        let _: () = msg_send![&*center, addObserver: observer, selector: sel!(appWillFinishLaunching:), name: &*name, object: std::ptr::null::<AnyObject>()];
    }
}

/// Give the application delegate `application:openFiles:`. Safe to call again: a second add is refused.
pub fn add_open_files() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let app: Retained<AnyObject> = msg_send![objc2::class!(NSApplication), sharedApplication];
        let delegate: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
        let Some(delegate) = delegate else {
            log::warn!("no application delegate: Finder cannot open files in Nullamp");
            return;
        };
        let class: &AnyClass = msg_send![&*delegate, class];
        let open_files: unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &AnyObject) = open_files;
        // SAFETY: the signature matches the type encoding `v@:@@` given below.
        let imp: Imp = std::mem::transmute(open_files);
        let added = objc2::ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, sel!(application:openFiles:), imp, c"v@:@@".as_ptr());
        if !added.as_bool() {
            log::warn!("the application delegate already handles application:openFiles:");
        }
    });
}

/// `application:openFiles:`, called with an `NSArray` of path strings.
unsafe extern "C-unwind" fn open_files(_this: &AnyObject, _cmd: Sel, app: &AnyObject, files: &AnyObject) {
    unsafe {
        let count: usize = msg_send![files, count];
        let paths: Vec<PathBuf> = (0..count)
            .filter_map(|i| {
                let path: Option<Retained<NSString>> = msg_send![files, objectAtIndex: i];
                path.map(|p| PathBuf::from(p.to_string()))
            })
            .collect();
        log::info!("Finder opens {} file(s)", paths.len());
        crate::open::push(paths);
        // NSApplicationDelegateReplySuccess: without a reply AppKit keeps waiting and may report a failure.
        let _: () = msg_send![app, replyToOpenOrPrint: 0usize];
    }
}
