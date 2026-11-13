//! The recording overlay (design 2h): a bar near the bottom of the screen
//! showing recording, transcribing, inserted and error states.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

use crate::state::{SessionState, SharedSettings, Target};
use crate::{platform, talk_key_state};

pub const LABEL: &str = "overlay";
const WIDTH: f64 = 440.0;
const HEIGHT: f64 = 56.0;
const BOTTOM_MARGIN: f64 = 72.0;
/// How long the "Inserted" state stays up (and Undo stays available).
pub const DONE_VISIBLE: Duration = Duration::from_secs(4);
const DRAW_TIMEOUT: Duration = Duration::from_secs(1);

static DRAW_CHECKS: AtomicU64 = AtomicU64::new(0);
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static DRAWN: AtomicU64 = AtomicU64::new(0);
static HIDDEN_AFTER: AtomicU64 = AtomicU64::new(0);
static NOT_DRAWING: AtomicBool = AtomicBool::new(false);
static MONITORS: Mutex<String> = Mutex::new(String::new());
static PAGE_VISIBILITY: Mutex<String> = Mutex::new(String::new());

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Overlay {
    #[serde(rename_all = "camelCase")]
    Recording {
        toggle: bool,
        keys: String,
        minimal: bool,
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

fn enabled(app: &AppHandle) -> bool {
    app.state::<SharedSettings>().get().show_overlay
}

fn minimal(app: &AppHandle) -> bool {
    app.state::<SharedSettings>().get().minimal_overlay
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
    log_monitor_changes(app);
    if monitor.is_none() {
        crate::logfile::write("overlay: no monitor found to place it on");
    }

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
    let hidden_when_minimal = matches!(
        state,
        Overlay::Transcribing { .. } | Overlay::Polishing | Overlay::Done { .. }
    );
    if matches!(state, Overlay::Hidden) || !enabled(app) || (hidden_when_minimal && minimal(app)) {
        platform::set_visible_without_focus(&window, false);
        HIDDEN_AFTER.store(DRAW_CHECKS.load(Ordering::Relaxed), Ordering::Relaxed);
        let _ = app.emit_to(LABEL, "overlay-state", Overlay::Hidden);
        return;
    }

    if matches!(state, Overlay::Recording { .. }) {
        place(app);
    }
    let _ = app.emit_to(LABEL, "overlay-state", &state);
    platform::set_visible_without_focus(&window, true);
    check_drawn(app, state_name(&state));
}

fn state_name(state: &Overlay) -> &'static str {
    match state {
        Overlay::Recording { .. } => "recording",
        Overlay::Transcribing { .. } => "transcribing",
        Overlay::Polishing => "polishing",
        Overlay::Done { .. } => "done",
        Overlay::Error { .. } => "error",
        Overlay::Hidden => "hidden",
    }
}

fn describe_monitors(app: &AppHandle) -> String {
    app.available_monitors()
        .map(|monitors| {
            monitors
                .iter()
                .map(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    format!(
                        "{} {}x{} at {},{} scale {}",
                        monitor.name().map_or("unnamed", |name| name.as_str()),
                        size.width,
                        size.height,
                        position.x,
                        position.y,
                        monitor.scale_factor()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|error| format!("unknown ({error})"))
}

fn log_monitor_changes(app: &AppHandle) {
    let monitors = describe_monitors(app);
    let Ok(mut last) = MONITORS.lock() else {
        return;
    };
    if *last != monitors {
        crate::logfile::write(&format!("monitors: {monitors}"));
        *last = monitors;
    }
}

fn describe_window(window: &tauri::WebviewWindow) -> String {
    let position = window.outer_position().map_or_else(
        |error| format!("unknown ({error})"),
        |p| format!("{},{}", p.x, p.y),
    );
    let size = window.outer_size().map_or_else(
        |error| format!("unknown ({error})"),
        |s| format!("{}x{}", s.width, s.height),
    );
    let monitor = match window.current_monitor() {
        Ok(Some(monitor)) => monitor
            .name()
            .cloned()
            .unwrap_or_else(|| "unnamed".to_owned()),
        Ok(None) => "none (off-screen?)".to_owned(),
        Err(error) => format!("unknown ({error})"),
    };
    format!(
        "visible={:?} minimized={:?} on_top={:?} position={position} size={size} monitor={monitor}",
        window.is_visible().ok(),
        window.is_minimized().ok(),
        window.is_always_on_top().ok(),
    )
}

fn check_drawn(app: &AppHandle, state: &'static str) {
    let check = DRAW_CHECKS.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = app.emit_to(LABEL, "overlay-draw-check", check);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DRAW_TIMEOUT);
        if DRAWN.load(Ordering::Relaxed) >= check || HIDDEN_AFTER.load(Ordering::Relaxed) >= check {
            return;
        }

        NOT_DRAWING.store(true, Ordering::Relaxed);
        let received = if RECEIVED.load(Ordering::Relaxed) >= check {
            PAGE_VISIBILITY.lock().map_or_else(
                |_| "yes".to_owned(),
                |visibility| format!("yes, page {visibility}"),
            )
        } else {
            "no".to_owned()
        };
        let window = app.get_webview_window(LABEL).map_or_else(
            || "window missing".to_owned(),
            |window| describe_window(&window),
        );
        crate::logfile::write(&format!(
            "overlay did not draw within {} ms (check {check}, state {state}): page received the check: {received}; {window}; monitors: {}",
            DRAW_TIMEOUT.as_millis(),
            describe_monitors(&app)
        ));
    });
}

#[tauri::command]
pub fn overlay_drawn(check: u64, stage: String, visibility: String) {
    if stage == "received" {
        if let Ok(mut last) = PAGE_VISIBILITY.lock() {
            *last = visibility;
        }

        RECEIVED.fetch_max(check, Ordering::Relaxed);
        return;
    }

    DRAWN.fetch_max(check, Ordering::Relaxed);
    if NOT_DRAWING.swap(false, Ordering::Relaxed) {
        crate::logfile::write(&format!(
            "overlay is drawing again (check {check}, page {visibility})"
        ));
    }
}

#[tauri::command]
pub fn overlay_log(message: String) {
    let message: String = message.chars().take(500).collect();
    crate::logfile::write(&format!("overlay page: {message}"));
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
pub fn give_back_focus(app: &AppHandle) {
    let (Some(overlay), Some(main)) = (
        app.get_webview_window(LABEL),
        app.get_webview_window(crate::own_window::WINDOW),
    ) else {
        return;
    };
    if !platform::is_active_window(&overlay) || !main.is_visible().unwrap_or(false) {
        return;
    }
    crate::logfile::write("overlay took focus; giving it back to the main window");
    platform::activate_window(&main);
    let webview: &tauri::Webview = main.as_ref();
    let _ = webview.set_focus();
}

/// Live text from a streaming engine while recording.
pub fn partial(app: &AppHandle, text: &str) {
    if enabled(app) && !minimal(app) {
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
    let settings = app.state::<SharedSettings>().get();
    let (hold_key, toggle_key) = (settings.hold_key.clone(), settings.toggle_key.clone());
    let toggle = talk_key_state::toggle_active();
    show(
        app,
        Overlay::Recording {
            toggle,
            keys: if toggle { toggle_key } else { hold_key },
            minimal: settings.minimal_overlay,
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
                if !session.coordinator.is_active(id) {
                    return;
                }
                match session.capture.as_ref() {
                    Some(capture) => capture.meter(),
                    None => return,
                }
            };
            let _ = app.emit_to(LABEL, "overlay-level", meter.0);
            if meter.1 {
                talk_key_state::request_stop();
                return;
            }
        }
    });
}

#[tauri::command]
pub fn overlay_stop() -> Result<(), String> {
    if talk_key_state::request_stop() {
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
    let last = crate::state::lock(&session)?.coordinator.take_last_insert();
    let Some(last) = last else {
        return Err("Nothing to undo".to_owned());
    };
    let recent = last.at.elapsed() <= DONE_VISIBLE + Duration::from_secs(2);
    let handle = app.clone();
    let undone = recent
        && tauri::async_runtime::spawn_blocking(move || match last.destination {
            Some(Target::Yap) => crate::own_window::undo(&handle, last.id).is_ok(),
            Some(Target::App(destination)) => {
                platform::still_focused(&destination) && platform::undo()
            }
            None => false,
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
