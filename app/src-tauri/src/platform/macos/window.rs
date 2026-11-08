use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};
use tauri::WebviewWindow;

/// Shows or hides a window without activating Yap, so the app the user is
/// dictating into keeps focus. Shown on every Space and over full-screen
/// apps. AppKit calls must run on the main thread.
pub fn set_visible_without_focus(window: &WebviewWindow, visible: bool) {
    let Ok(pointer) = window.ns_window() else {
        return;
    };
    let pointer = pointer as usize;
    let _ = window.run_on_main_thread(move || {
        let window = unsafe { &*(pointer as *const NSWindow) };
        if visible {
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            window.orderFrontRegardless();
        } else {
            window.orderOut(None);
        }
    });
}

pub fn is_active(window: &WebviewWindow) -> bool {
    window.is_focused().unwrap_or(false)
}

pub fn activate(window: &WebviewWindow) {
    let _ = window.set_focus();
}
