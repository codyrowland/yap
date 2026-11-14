use tauri::WebviewWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, HWND_TOPMOST, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow,
};

/// Shows or hides a window without ever activating it.
///
/// Tauri's `show()` calls `ShowWindow(SW_SHOW)`, which activates the window
/// even though it is marked not focusable. Windows only lets that happen
/// while Yap is the foreground app, so dictating into Yap's own window had
/// the overlay take focus from the text box (and sometimes not give it
/// back). Other apps were never affected because Windows blocks the
/// activation there. `SW_SHOWNOACTIVATE` shows it without taking focus.
///
/// Both directions go through Win32 so Tauri's idea of the window's
/// visibility never disagrees with what is on screen.
pub fn set_visible_without_focus(window: &WebviewWindow, visible: bool) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        if visible {
            SetWindowPos(
                hwnd.0,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }

        ShowWindow(hwnd.0, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

pub fn is_active(window: &WebviewWindow) -> bool {
    window
        .hwnd()
        .is_ok_and(|hwnd| unsafe { GetForegroundWindow() } == hwnd.0)
}

pub fn activate(window: &WebviewWindow) {
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            SetForegroundWindow(hwnd.0);
        }
    }
}
