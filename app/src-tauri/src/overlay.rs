//! The recording overlay (design 2h): a bar near the bottom of the screen
//! showing recording, transcribing, inserted and error states.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

use crate::state::{SessionState, Target};
use crate::{hotkey, platform_input};

pub const LABEL: &str = "overlay";
const WIDTH: f64 = 440.0;
const HEIGHT: f64 = 56.0;
const BOTTOM_MARGIN: f64 = 72.0;
/// How long the "Inserted" state stays up (and Undo stays available).
pub const DONE_VISIBLE: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Overlay {
    #[serde(rename_all = "camelCase")]
    Recording {
        toggle: bool,
        keys: String,
    },
    #[serde(rename_all = "camelCase")]
    Transcribing {
        engine: String,
        model: String,
    },
    /// The clean-up model is tidying the transcript.
    Polishing,
    #[serde(rename_all = "camelCase")]
    Done {
        words: usize,
        app: String,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
    },
    Hidden,
}

/// Never waits for the session lock: if a caller already holds it (or it is
/// busy), the overlay is shown rather than risking a deadlock.
fn enabled(app: &AppHandle) -> bool {
    match app.state::<SessionState>().try_lock() {
        Ok(session) => session.settings.show_overlay,
        Err(std::sync::TryLockError::WouldBlock) => true,
        Err(std::sync::TryLockError::Poisoned(_)) => false,
    }
}

/// Bottom-centre of the monitor the user is working on (the one under the
/// cursor), inside its work area so the taskbar never covers the bar.
fn place(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|cursor| app.monitor_from_point(cursor.x, cursor.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let x = area.position.x as f64 + (area.size.width as f64 - WIDTH * scale) / 2.0;
        let y = area.position.y as f64 + area.size.height as f64 - (HEIGHT + BOTTOM_MARGIN) * scale;
        let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
    }
}

pub fn show(app: &AppHandle, state: Overlay) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if matches!(state, Overlay::Hidden) || !enabled(app) {
        set_visible(&window, false);
        let _ = app.emit_to(LABEL, "overlay-state", Overlay::Hidden);
        return;
    }

    if matches!(state, Overlay::Recording { .. }) {
        place(app);
    }
    let _ = app.emit_to(LABEL, "overlay-state", &state);
    set_visible(&window, true);
}

/// Shows or hides the overlay without ever activating it.
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
#[cfg(windows)]
fn set_visible(window: &tauri::WebviewWindow, visible: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow};
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        ShowWindow(hwnd.0, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

/// The overlay must never keep focus. Even shown with `SW_SHOWNOACTIVATE`
/// and marked not focusable, its WebView2 page takes focus when the window
/// appears, which activates the overlay (yap.log: `active=overlay` 150 ms
/// after a press). Windows only allows that while Yap is in front, so this
/// only ever took focus from Yap's own window. Hand activation straight
/// back to the main window; its page then puts the cursor back in the field
/// the user was in.
///
/// Only acts when the overlay really is the active window: clicking Stop
/// while dictating into another app gives the overlay's page focus without
/// activating it, and must not pull Yap's window to the front.
#[cfg(windows)]
pub fn give_back_focus(app: &AppHandle) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let (Some(overlay), Some(main)) = (
        app.get_webview_window(LABEL),
        app.get_webview_window(crate::own_window::WINDOW),
    ) else {
        return;
    };
    let (Ok(overlay_hwnd), Ok(main_hwnd)) = (overlay.hwnd(), main.hwnd()) else {
        return;
    };
    if unsafe { GetForegroundWindow() } != overlay_hwnd.0 || !main.is_visible().unwrap_or(false) {
        return;
    }
    crate::logfile::write("overlay took focus; giving it back to the main window");
    unsafe {
        SetForegroundWindow(main_hwnd.0);
    }
    let webview: &tauri::Webview = main.as_ref();
    let _ = webview.set_focus();
}

#[cfg(not(windows))]
pub fn give_back_focus(_app: &AppHandle) {}

// macOS and Linux need their own non-activating show (an NSPanel on macOS).
#[cfg(not(windows))]
fn set_visible(window: &tauri::WebviewWindow, visible: bool) {
    let _ = if visible {
        window.show()
    } else {
        window.hide()
    };
}

/// Live text from a streaming engine while recording.
pub fn partial(app: &AppHandle, text: &str) {
    if enabled(app) {
        let _ = app.emit_to(LABEL, "overlay-partial", text);
    }
}

pub fn hide(app: &AppHandle) {
    show(app, Overlay::Hidden);
}

/// Shows a final state, then hides the overlay unless something newer
/// (another recording) replaced it in the meantime.
pub fn flash(app: &AppHandle, state: Overlay) {
    show(app, state);
    let app = app.clone();
    let generation = bump();
    std::thread::spawn(move || {
        std::thread::sleep(DONE_VISIBLE);
        if current() == generation {
            hide(&app);
        }
    });
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn bump() -> u64 {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

fn current() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Starts the overlay for recording `id` and feeds it microphone levels until
/// the recording ends. Stops the recording when it reaches the length limit.
pub fn track_recording(app: &AppHandle, id: u64) {
    bump();
    let (hold_key, toggle_key) = app
        .state::<SessionState>()
        .lock()
        .map(|session| {
            (
                session.settings.hold_key.clone(),
                session.settings.toggle_key.clone(),
            )
        })
        .unwrap_or_default();
    let toggle = hotkey::toggle_active();
    show(
        app,
        Overlay::Recording {
            toggle,
            keys: if toggle { toggle_key } else { hold_key },
        },
    );
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(60));
            let meter = {
                let state = app.state::<SessionState>();
                let Ok(session) = state.lock() else {
                    return;
                };
                if session.active_id != Some(id) {
                    return;
                }
                match session.capture.as_ref() {
                    Some(capture) => capture.meter(),
                    None => return,
                }
            };
            let _ = app.emit_to(LABEL, "overlay-level", meter.0);
            if meter.1 {
                hotkey::request_stop();
                return;
            }
        }
    });
}

#[tauri::command]
pub fn overlay_stop() -> Result<(), String> {
    if hotkey::request_stop() {
        Ok(())
    } else {
        Err("No recording to stop".to_owned())
    }
}

/// Undoes the last insertion if the same field still has focus: Ctrl+Z in
/// another app, or the page restoring its field in Yap's own window.
#[tauri::command]
pub async fn overlay_undo(
    app: AppHandle,
    session: tauri::State<'_, SessionState>,
) -> Result<(), String> {
    let last = crate::state::lock(&session)?.last_insert.take();
    let Some(last) = last else {
        return Err("Nothing to undo".to_owned());
    };
    let recent = last.at.elapsed() <= DONE_VISIBLE + Duration::from_secs(2);
    let handle = app.clone();
    let undone = recent
        && tauri::async_runtime::spawn_blocking(move || match last.target {
            Target::Yap => crate::own_window::undo(&handle, last.id).is_ok(),
            Target::App(destination) => {
                platform_input::destination_verified(
                    Some(&destination),
                    platform_input::focused_destination().as_ref(),
                    true,
                    platform_input::modifiers_released(),
                ) && crate::clipboard::undo()
            }
        })
        .await
        .unwrap_or(false);
    if !undone {
        flash(
            &app,
            Overlay::Error {
                message: "Could not undo — the original field lost focus".to_owned(),
            },
        );
        return Err("Could not undo".to_owned());
    }

    hide(&app);
    Ok(())
}
