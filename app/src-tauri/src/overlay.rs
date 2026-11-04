//! The recording overlay (design 2h): a bar near the bottom of the screen
//! showing recording, transcribing, inserted and error states.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

use crate::state::SessionState;
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
        let _ = window.hide();
        let _ = app.emit_to(LABEL, "overlay-state", Overlay::Hidden);
        return;
    }

    if matches!(state, Overlay::Recording { .. }) {
        place(app);
    }
    let _ = app.emit_to(LABEL, "overlay-state", &state);
    let _ = window.show();
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

/// Undoes the last insertion with Ctrl+Z if the same field still has focus.
#[tauri::command]
pub fn overlay_undo(app: AppHandle, session: tauri::State<'_, SessionState>) -> Result<(), String> {
    let last = crate::state::lock(&session)?.last_insert.take();
    let Some(last) = last else {
        return Err("Nothing to undo".to_owned());
    };
    let still_there = last.at.elapsed() <= DONE_VISIBLE + Duration::from_secs(2)
        && platform_input::destination_verified(
            Some(&last.destination),
            platform_input::focused_destination().as_ref(),
            true,
            platform_input::modifiers_released(),
        );
    if !still_there || !crate::clipboard::undo() {
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
