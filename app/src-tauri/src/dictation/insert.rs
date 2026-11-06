//! Inserts a finished transcript where the talk key was pressed: Yap's own
//! window through its page, any other app through the OS.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use yap_app::coordinator::{Decision, JobInfo};
use yap_app::settings::Insertion;

use super::{history, notify};
use crate::overlay::{self, Overlay};
use crate::platform_input::{self, Destination, Verifier};
use crate::state::{SessionState, SharedSettings, Target};
use crate::{clipboard, own_window, talk_key_state};

/// A finished insertion, for the overlay and History.
struct Inserted {
    app_name: String,
    text: String,
    info: JobInfo,
}

/// Inserts recording `id`'s transcript where the talk key was pressed.
/// Returns whether text was inserted.
pub async fn insert_transcript(app: AppHandle, id: u64) -> bool {
    let own = app.state::<SessionState>().lock().is_ok_and(|session| {
        matches!(session.coordinator.destination(id), Some(Some(Target::Yap)))
    });
    let result = if own {
        insert_own(&app, id).await
    } else {
        insert_external(&app, id).await
    };
    // `None`: the dictation was cancelled or Yap is exiting; say nothing.
    let Some(result) = result else {
        talk_key_state::finish();
        return false;
    };
    crate::logfile::write(&format!(
        "insert {id}: {}",
        result.as_ref().map_or_else(
            |error| format!("failed: {error}"),
            |done| format!("inserted into {}", done.app_name)
        )
    ));

    let inserted = result.is_ok();
    match result {
        Ok(Inserted {
            app_name,
            text,
            info,
        }) => {
            overlay::flash(
                &app,
                Overlay::Done {
                    words: text.split_whitespace().count(),
                    app: app_name.clone(),
                },
            );
            history::save(&app, app_name, text, info);
            talk_key_state::finish();
        }
        Err(message) => {
            talk_key_state::finish();
            overlay::flash(
                &app,
                Overlay::Error {
                    message: message.clone(),
                },
            );
            notify(&app, message);
        }
    }
    inserted
}

/// Hands the text to Yap's own window, which inserts it at its cursor.
async fn insert_own(app: &AppHandle, id: u64) -> Option<Result<Inserted, String>> {
    let session_state = Arc::clone(app.state::<SessionState>().inner());
    let decision = {
        let mut session = session_state.lock().ok()?;
        if !session.coordinator.is_active(id) {
            return None;
        }
        // The page checks the field when the text arrives.
        session.coordinator.begin_insert(id, true)
    };
    let Decision::Insert { text, info, .. } = decision else {
        return Some(Err("Could not insert text".to_owned()));
    };

    let handle = app.clone();
    let sent = text.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || own_window::insert(&handle, id, &sent))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
    if let Ok(mut session) = session_state.lock() {
        session
            .coordinator
            .insertion_result(id, result.is_ok(), Instant::now());
    }
    Some(result.map(|()| Inserted {
        app_name: "Yap".to_owned(),
        text,
        info,
    }))
}

/// Types or pastes the text into another app, after checking that the field
/// the talk key was pressed in still has focus.
async fn insert_external(app: &AppHandle, id: u64) -> Option<Result<Inserted, String>> {
    const LOST_FOCUS: &str = "Could not insert text — the original field lost focus";
    let session_for_wait = Arc::clone(app.state::<SessionState>().inner());
    let modifiers_ready = tauri::async_runtime::spawn_blocking(move || {
        wait_for_modifiers(
            || {
                session_for_wait
                    .lock()
                    .is_ok_and(|session| session.coordinator.is_active(id))
            },
            platform_input::modifiers_released,
            Duration::from_secs(1),
        )
    })
    .await
    .unwrap_or(false);
    let verifier = Arc::clone(app.state::<Arc<Mutex<Verifier>>>().inner());
    let verified = tauri::async_runtime::spawn_blocking(move || field_watch_passed(&verifier, id))
        .await
        .unwrap_or(false);
    let session_state = Arc::clone(app.state::<SessionState>().inner());
    let (decision, method) = {
        let mut session = session_state.lock().ok()?;
        if !session.coordinator.is_active(id) {
            return None;
        }

        let original = match session.coordinator.destination(id) {
            Some(Some(Target::App(field))) => Some(field),
            _ => None,
        };
        let safe = verified && modifiers_ready && still_focused(original);
        let decision = session.coordinator.begin_insert(id, safe);
        (decision, app.state::<SharedSettings>().get().insertion)
    };
    let Decision::Insert {
        destination: Some(Target::App(destination)),
        text,
        info,
        ..
    } = decision
    else {
        return Some(Err(LOST_FOCUS.to_owned()));
    };

    let state = Arc::clone(&session_state);
    let target = destination.clone();
    let typed = text.clone();
    let submitted = tauri::async_runtime::spawn_blocking(move || {
        let ready = state
            .lock()
            .is_ok_and(|session| !session.coordinator.closed())
            && still_focused(Some(&target));
        ready && send_text(&typed, method)
    })
    .await
    .unwrap_or(false);
    if let Ok(mut session) = session_state.lock() {
        session
            .coordinator
            .insertion_result(id, submitted, Instant::now());
    }
    Some(if submitted {
        Ok(Inserted {
            app_name: destination.app_name(),
            text,
            info,
        })
    } else {
        Err(LOST_FOCUS.to_owned())
    })
}

fn wait_for_modifiers(
    active: impl Fn() -> bool,
    released: impl Fn() -> bool,
    limit: Duration,
) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !active() {
            return false;
        }
        if released() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn field_watch_passed(verifier: &Mutex<Verifier>, id: u64) -> bool {
    let verified = verifier
        .lock()
        .map_err(|error| error.to_string())
        .and_then(|verifier| verifier.check_reason(id));
    if let Err(why) = &verified {
        crate::logfile::write(&format!("insert {id}: field check failed: {why}"));
    }
    verified.is_ok()
}

fn still_focused(original: Option<&Destination>) -> bool {
    platform_input::destination_verified(
        original,
        platform_input::focused_destination().as_ref(),
        true,
        platform_input::modifiers_released(),
    )
}

/// Inserts `text` with the configured method. Paste falls back to typing when
/// the clipboard holds something Yap could not restore (an image, files).
fn send_text(text: &str, method: Insertion) -> bool {
    match method {
        Insertion::Type => platform_input::insert_unicode(text),
        Insertion::Paste => match clipboard::paste(text) {
            clipboard::PasteResult::Pasted => true,
            clipboard::PasteResult::Unavailable => platform_input::insert_unicode(text),
            clipboard::PasteResult::Failed => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn modifier_wait_releases_or_times_out_without_submitting_after_cancellation() {
        let held = Arc::new(AtomicBool::new(true));
        let held_for_release = Arc::clone(&held);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            held_for_release.store(false, Ordering::Relaxed);
        });
        assert!(wait_for_modifiers(
            || true,
            || !held.load(Ordering::Relaxed),
            Duration::from_secs(1),
        ));
        release.join().unwrap();
        assert!(!wait_for_modifiers(
            || true,
            || false,
            Duration::from_millis(10),
        ));
        assert!(!wait_for_modifiers(
            || false,
            || true,
            Duration::from_secs(1),
        ));
    }
}
