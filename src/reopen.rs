//! macOS: clicking the Dock icon while the window is closed to the menu bar should bring
//! it back. winit's application delegate doesn't implement
//! `applicationShouldHandleReopen:hasVisibleWindows:`, so it is added at runtime.

use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, msg_send, sel};
use std::sync::OnceLock;

static ON_REOPEN: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

unsafe extern "C-unwind" fn should_handle_reopen(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    if let Some(f) = ON_REOPEN.get() {
        f();
    }
    Bool::NO // handled here; AppKit should do nothing else
}

/// Call once, on the main thread, after the app has launched.
pub fn install(on_reopen: impl Fn() + Send + Sync + 'static) {
    if ON_REOPEN.set(Box::new(on_reopen)).is_err() {
        return;
    }
    // SAFETY: plain Objective-C runtime calls on the main thread. The method signature
    // and type encoding match `applicationShouldHandleReopen:hasVisibleWindows:`.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![app, delegate];
        let Some(delegate) = delegate.as_ref() else {
            log::warn!("no app delegate; Dock icon won't reopen the window");
            return;
        };
        let cls = delegate.class() as *const AnyClass as *mut AnyClass;
        let imp: Imp = std::mem::transmute(
            should_handle_reopen
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool,
        );
        // BOOL is `bool` ("B") on arm64 and `signed char` ("c") on x86_64.
        let types = if cfg!(target_arch = "aarch64") { c"B@:@B" } else { c"c@:@c" };
        let sel = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        if !objc2::ffi::class_addMethod(cls, sel, imp, types.as_ptr()).as_bool() {
            log::warn!("Dock reopen handler not installed (delegate already has one)");
        }
    }
}
