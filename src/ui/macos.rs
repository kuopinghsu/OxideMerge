//! macOS application menu.

use objc2::sel;
use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;

/// Makes Quit in the application menu (⌘Q) close the window like its close button does, so
/// unsaved changes are asked about first instead of the app ending at once.
pub fn quit_closes_window() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: AppKit menus are only touched on the main thread, which `mtm` proves.
    unsafe {
        let Some(bar) = app.mainMenu() else {
            return;
        };
        for i in 0..bar.numberOfItems() {
            let Some(menu) = bar.itemAtIndex(i).and_then(|item| item.submenu()) else {
                continue;
            };
            for j in 0..menu.numberOfItems() {
                if let Some(item) = menu.itemAtIndex(j) {
                    if item.action() == Some(sel!(terminate:)) {
                        // No target: the action goes to the key window.
                        item.setTarget(None);
                        item.setAction(Some(sel!(performClose:)));
                    }
                }
            }
        }
    }
}
