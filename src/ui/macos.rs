//! macOS application quitting.

use std::sync::atomic::{AtomicBool, Ordering};

use objc2::runtime::{AnyObject, Sel};
use objc2::{ffi, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;

/// Some tab has unsaved changes; kept up to date by the app every frame.
static UNSAVED: AtomicBool = AtomicBool::new(false);

pub fn set_unsaved(unsaved: bool) {
    UNSAVED.store(unsaved, Ordering::Relaxed);
}

const TERMINATE_CANCEL: usize = 0;
const TERMINATE_NOW: usize = 1;

/// `-applicationShouldTerminate:`: with unsaved changes, quitting (⌘Q, the Dock, logging out)
/// closes the window instead, which asks about them first.
extern "C" fn should_terminate(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
    if !UNSAVED.load(Ordering::Relaxed) {
        return TERMINATE_NOW;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return TERMINATE_NOW;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: plain AppKit messages sent on the main thread, which `mtm` proves.
    unsafe {
        let windows: *mut AnyObject = msg_send![&app, windows];
        if windows.is_null() {
            return TERMINATE_NOW;
        }
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let w: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            let main: bool = msg_send![w, canBecomeMainWindow];
            let mini: bool = msg_send![w, isMiniaturized];
            let visible: bool = msg_send![w, isVisible];
            if !main || !(mini || visible) {
                continue;
            }
            if mini {
                let _: () = msg_send![w, deminiaturize: std::ptr::null::<AnyObject>()];
            }
            let _: () = msg_send![w, performClose: std::ptr::null::<AnyObject>()];
            return TERMINATE_CANCEL;
        }
    }
    TERMINATE_NOW
}

/// Has quitting the app ask about unsaved changes first, however it is asked to quit.
pub fn ask_before_quitting() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: the delegate is winit's, alive for the whole run; the method added matches the
    // signature AppKit calls it with ("Q@:@": NSUInteger return, self, _cmd, sender).
    unsafe {
        let delegate: *mut AnyObject = msg_send![&app, delegate];
        if delegate.is_null() {
            return;
        }
        let class = ffi::object_getClass(delegate as *const ffi::objc_object);
        let imp: unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> usize = should_terminate;
        ffi::class_addMethod(
            class as *mut ffi::objc_class,
            sel!(applicationShouldTerminate:).as_ptr(),
            Some(std::mem::transmute::<
                unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
                unsafe extern "C" fn(),
            >(imp)),
            c"Q@:@".as_ptr(),
        );
    }
}
