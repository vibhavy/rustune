//! macOS: the mini player should open over whatever is on screen, including other
//! desktops (Spaces) and full-screen apps, like a menu bar popover.
//!
//! macOS only shows a window over another app's full-screen Space when it belongs to a
//! background ("accessory") app, as menu bar apps are. So while the mini player is
//! open, rustune switches to that mode (its Dock icon and menu bar hide), and its
//! window is set to join every Space at the menu bar's window level.

use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use std::ffi::{CStr, c_char};

/// `NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary`
const ALL_SPACES: usize = (1 << 0) | (1 << 8);
/// `NSStatusWindowLevel`: the level menu bar extras use, above full-screen apps.
const STATUS_LEVEL: isize = 25;
/// `NSApplicationActivationPolicyRegular` / `…Accessory`
const REGULAR: isize = 0;
const ACCESSORY: isize = 1;

/// Run as a background app (no Dock icon or menu bar) or as a normal one again.
/// Call on the main thread.
pub fn set_background_app(on: bool) {
    // SAFETY: AppKit call on the main thread with an NSApplicationActivationPolicy value.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: if on { ACCESSORY } else { REGULAR }];
    }
}

/// Make the app's window titled `title` show on every Space and over full-screen apps.
/// Returns false if no such window exists (yet). Call on the main thread.
pub fn float_everywhere(title: &str) -> bool {
    // SAFETY: AppKit calls on the main thread (egui runs viewport callbacks there);
    // pointers are null-checked, argument types match NSUInteger / NSInteger.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        if windows.is_null() {
            return false;
        }
        let n: usize = msg_send![windows, count];
        for i in 0..n {
            let w: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            let t: *mut AnyObject = msg_send![w, title];
            if t.is_null() {
                continue;
            }
            let s: *const c_char = msg_send![t, UTF8String];
            if s.is_null() || CStr::from_ptr(s).to_bytes() != title.as_bytes() {
                continue;
            }
            let _: () = msg_send![w, setCollectionBehavior: ALL_SPACES];
            let _: () = msg_send![w, setLevel: STATUS_LEVEL];
            // The new behaviour applies when the window is next ordered in; this brings
            // it onto the Space being looked at.
            let _: () = msg_send![w, orderFrontRegardless];
            return true;
        }
    }
    false
}
