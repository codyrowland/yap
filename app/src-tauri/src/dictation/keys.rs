//! Talk-key events from the keyboard hook, turned into dictation steps.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};

use super::{begin_recording, cancel_recording, fail, insert_transcript, notify, stop_recording};
use crate::overlay::{self, Overlay};
use crate::platform::{self, FieldWatch, WindowId};
use crate::state::{SessionState, Target};
use crate::{own_window, talk_key_state};

fn on_pressed(handle: &AppHandle, window: Option<WindowId>) {
    let same_foreground = window.is_some() && platform::foreground_window() == window;
    crate::logfile::write(&format!(
        "talk key pressed: yap_in_front={} same_foreground={same_foreground} toggle={}",
        platform::app_in_front(),
        talk_key_state::toggle_active()
    ));
    // Yap's own window takes the text directly; anything else is reached
    // through the OS and watched until the text is inserted.
    let destination = if !same_foreground {
        None
    } else if own_window::in_front(handle) {
        Some(Target::Yap)
    } else {
        platform::focused_destination().map(Target::App)
    };
    let verifier = Arc::clone(handle.state::<Arc<Mutex<FieldWatch>>>().inner());
    if let Err(error) = begin_recording(
        handle.state::<SessionState>().inner(),
        handle,
        destination,
        Some(&verifier),
    ) {
        talk_key_state::finish();
        overlay::flash(
            handle,
            Overlay::Error {
                message: error.clone(),
            },
        );
        notify(handle, error);
    }
}

fn on_released(handle: &AppHandle) {
    let Ok(job) = handle.state::<SessionState>().lock().map(|session| {
        let id = session.coordinator.active_id()?;
        Some((id, session.coordinator.cancel_flag(id)?))
    }) else {
        return;
    };
    let Some((id, cancelled)) = job else {
        return;
    };
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let result = stop_recording(handle.state::<SessionState>(), handle.clone(), id).await;
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        let still_active = handle
            .state::<SessionState>()
            .lock()
            .is_ok_and(|session| session.coordinator.is_active(id));
        match result {
            Ok(_) if still_active => {
                insert_transcript(handle, id).await;
            }
            Ok(text) | Err(text) => fail(&handle, text),
        }
    });
}

/// Handles talk-key events from the keyboard hook until the app exits.
pub fn run_talk_key_events(
    handle: AppHandle,
    events: std::sync::mpsc::Receiver<talk_key_state::Event>,
) {
    std::thread::spawn(move || {
        crate::logfile::write("talk-key listener running");
        for event in events {
            crate::logfile::write(&format!("talk key event: {event:?}"));
            match event {
                talk_key_state::Event::Pressed(window) => on_pressed(&handle, window),
                talk_key_state::Event::Released => on_released(&handle),
                talk_key_state::Event::Cancelled => {
                    let _ = cancel_recording(handle.state::<SessionState>().inner(), &handle);
                    talk_key_state::finish();
                    notify(&handle, "Cancelled");
                }
            }
        }
    });
}
