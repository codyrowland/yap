//! The dictation pipeline: talk key → record → transcribe → insert.

mod engines;
mod history;
mod insert;
mod keys;
mod live;
mod transcribe;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::coordinator::Settings as JobSettings;

use crate::overlay::{self, Overlay};
use crate::platform::FieldWatch;
use crate::state::{SessionState, SharedSettings, Target, lock};
use crate::{audio, talk_key_state};

pub use engines::{Engines, preload, unload};
pub use insert::insert_transcript;
pub use keys::run_talk_key_events;
pub use live::LiveState;

/// Tells the settings window (and onboarding) what happened.
pub fn notify(app: &AppHandle, message: impl Into<String>) {
    let message = message.into();
    crate::logfile::write(&format!("notice: {message}"));
    let _ = app.emit_to("main", "dictation-preview", message);
}

pub fn begin_recording(
    session: &SessionState,
    app: &AppHandle,
    destination: Option<Target>,
    verifier: Option<&Arc<Mutex<FieldWatch>>>,
) -> Result<u64, String> {
    let mut session = lock(session)?;
    if !session.idle() {
        return Err("Dictation is already active or exiting".to_owned());
    }

    // Read under the session lock: settings only change while it is held.
    let settings = app.state::<SharedSettings>().get();
    let job = JobSettings {
        microphone_index: settings.microphone_index,
        speech: settings.speech_config()?,
        language: settings.language_code(),
    };
    let index = job.microphone_index;
    let streaming = job
        .speech
        .streams()
        .then(|| (job.speech.clone(), job.language.clone()));
    let id = session
        .coordinator
        .start(job, destination.clone(), Instant::now())
        .ok_or("Dictation is already active")?;
    // Only another app's field needs watching; Yap's own window checks its
    // field itself when the text arrives.
    if let Some(verifier) = verifier {
        match &destination {
            Some(Target::Yap) => lock(verifier)?.cancel(),
            Some(Target::App(field)) => lock(verifier)?.capture(id, Some(field.clone())),
            None => lock(verifier)?.capture(id, None),
        }
    }

    let capture = match app.state::<audio::MicrophoneState>().start(index) {
        Ok(capture) => capture,
        Err(error) => {
            session.coordinator.fail(id);
            if let Some(verifier) = verifier {
                lock(verifier)?.cancel();
            }
            return Err(error);
        }
    };
    let tap = capture.tap();
    session.capture = Some(capture);
    if settings.mute_while_recording {
        session.muted = crate::speakers::mute(app);
    }

    drop(session);
    overlay::track_recording(app, id);
    if let Some((speech, language)) = streaming {
        live::start(app, id, speech, language, tap);
    }
    Ok(id)
}

/// Runs the transcript through the clean-up model. Any failure keeps the
/// original words: clean-up must never lose a dictation.
async fn polish(app: &AppHandle, text: String, cancelled: &Arc<AtomicBool>) -> String {
    overlay::show(app, Overlay::Polishing);
    let original = text.clone();
    let cleaned = tauri::async_runtime::spawn_blocking(move || crate::assistant::clean(&text))
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
    match cleaned {
        Ok(cleaned) if !cancelled.load(Ordering::Relaxed) => cleaned,
        Ok(_) => original,
        Err(error) => {
            notify(app, format!("Inserted without clean-up: {error}"));
            original
        }
    }
}

pub async fn stop_recording(
    session: State<'_, SessionState>,
    app: AppHandle,
    expected: u64,
) -> Result<String, String> {
    let session = Arc::clone(session.inner());
    let (id, capture, muted, cancelled, cleanup) = {
        let mut session = lock(&session)?;
        let id = session
            .coordinator
            .active_id()
            .ok_or("No recording is active")?;
        if expected != id {
            return Err("Cancelled".to_owned());
        }
        let cancelled = session
            .coordinator
            .cancel_flag(id)
            .ok_or("No recording is active")?;
        let capture = session.capture.take().ok_or("No recording is active")?;
        let cleanup = app.state::<SharedSettings>().get().cleanup_enabled;
        (id, capture, session.muted.take(), cancelled, cleanup)
    };
    drop(muted);
    let recording = capture.stop();
    // A recording that hit the length limit is still transcribed: the limit
    // ends it, like releasing the key.
    let normalized = recording
        .as_ref()
        .ok()
        .map(audio::Recording::normalized_16k);
    let (request, audio) = {
        let mut session = lock(&session)?;
        if !session.coordinator.is_active(id) || cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let recording = match recording {
            Ok(recording) => recording,
            Err(error) => {
                session.coordinator.fail(id);
                return Err(error);
            }
        };
        let normalized = normalized.unwrap_or_default();
        let seconds = recording.samples.len() as f32 / recording.sample_rate.max(1) as f32;
        crate::logfile::write(&format!("recording {id} stopped ({seconds:.1}s)"));
        if !audio::meaningful_audio(&normalized) {
            crate::logfile::write(&format!("recording {id}: no meaningful audio"));
            session.coordinator.release(id, None, Instant::now());
            return Ok("No meaningful audio".to_owned());
        }

        let Some(request) = session
            .coordinator
            .release(id, Some(seconds), Instant::now())
        else {
            return Err("Recording timed out".to_owned());
        };
        let raw = request
            .settings
            .speech
            .streams()
            .then_some((recording.samples, recording.sample_rate));
        (request, transcribe::Audio { normalized, raw })
    };
    let (engine, model) = request.settings.speech.describe();
    overlay::show(&app, Overlay::Transcribing { engine, model });

    let worker_cancelled = Arc::clone(&cancelled);
    let worker_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        transcribe::run(&worker_app, &request, audio, &worker_cancelled)
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => Err(error.to_string()),
    };
    match &result {
        Ok(text) => crate::logfile::write(&format!(
            "transcribed {id} ({} words)",
            text.split_whitespace().count()
        )),
        Err(error) => crate::logfile::write(&format!("transcription {id} failed: {error}")),
    }
    let result = match result {
        Ok(text) if cleanup && yap_app::cleanup::worth_cleaning(&text) => {
            Ok(polish(&app, text, &cancelled).await)
        }
        other => other,
    };
    let mut session = lock(&session)?;
    if !session.coordinator.is_active(id) {
        return Err("Cancelled".to_owned());
    }

    match result {
        Ok(text)
            if session
                .coordinator
                .transcript(id, text.clone(), Instant::now()) =>
        {
            Ok(text)
        }
        Ok(_) => Err("Transcription was empty or timed out".to_owned()),
        Err(error) => {
            session.coordinator.fail(id);
            Err(error)
        }
    }
}

pub fn cancel_recording(session: &SessionState, app: &AppHandle) -> Result<(), String> {
    overlay::hide(app);
    app.state::<LiveState>().clear();
    let mut session = lock(session)?;
    session.coordinator.cancel_current();
    session.capture = None;
    let muted = session.muted.take();
    drop(session);
    drop(muted);
    lock(app.state::<Arc<Mutex<FieldWatch>>>().inner())?.cancel();
    Ok(())
}

fn fail(app: &AppHandle, message: String) {
    if let Ok(verifier) = app.state::<Arc<Mutex<FieldWatch>>>().lock() {
        verifier.cancel();
    }
    talk_key_state::finish();
    if message == "Cancelled" {
        return;
    }
    if message == "No meaningful audio" {
        overlay::hide(app);
    } else {
        overlay::flash(
            app,
            Overlay::Error {
                message: message.clone(),
            },
        );
    }
    notify(app, message);
}
