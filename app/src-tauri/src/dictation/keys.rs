//! Talk-key events from the keyboard hook, turned into dictation steps.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};

use super::{begin_recording, cancel_recording, fail, insert_transcript, notify, stop_recording};
use crate::overlay::{self, Overlay};
use crate::platform_input::{self, Verifier};
use crate::state::{SessionState, Target};
use crate::{hotkey, own_window};

fn on_pressed(handle: &AppHandle, window: isize) {
    let same_foreground = window != 0 && platform_input::foreground_window_handle() == window;
    crate::logfile::write(&format!(
        "talk key pressed: yap_in_front={} same_foreground={same_foreground} toggle={}",
        platform_input::app_is_foreground(),
        hotkey::toggle_active()
    ));
    // Yap's own window takes the text directly; anything else is reached
    // through the OS and watched until the text is inserted.
    let destination = if !same_foreground {
        None
    } else if own_window::in_front(handle) {
        Some(Target::Yap)
    } else {
        platform_input::focused_destination().map(Target::App)
    };
    let verifier = Arc::clone(handle.state::<Arc<Mutex<Verifier>>>().inner());
    if let Err(error) = begin_recording(
        handle.state::<SessionState>().inner(),
        handle,
        destination,
        Some(&verifier),
    ) {
        hotkey::finish();
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
pub fn run_hotkey_events(handle: AppHandle, events: std::sync::mpsc::Receiver<hotkey::Event>) {
    std::thread::spawn(move || {
        crate::logfile::write("talk-key listener running");
        for event in events {
            crate::logfile::write(&format!("talk key event: {event:?}"));
            match event {
                hotkey::Event::Pressed(window) => on_pressed(&handle, window),
                hotkey::Event::Released => on_released(&handle),
                hotkey::Event::Cancelled => {
                    let _ = cancel_recording(handle.state::<SessionState>().inner(), &handle);
                    hotkey::finish();
                    notify(&handle, "Cancelled");
                }
            }
        }
    });
}
