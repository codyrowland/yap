//! The dictation pipeline: talk key → record → transcribe → insert.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::coordinator::{Decision, Settings as JobSettings};
use yap_app::history::History;
use yap_app::settings::{Engine, Insertion, Settings};
use yap_app::speech::{self, SpeechConfig};

use crate::overlay::{self, Overlay};
use crate::platform_input::{self, Destination, Verifier};
use crate::state::{HistoryMeta, LastInsert, SessionState, lock};
use crate::{audio, clipboard, hotkey, onboarding};

/// Tells the settings window (and onboarding) what happened.
pub fn notify(app: &AppHandle, message: impl Into<String>) {
    let _ = app.emit_to("main", "dictation-preview", message.into());
}

/// Builds the engine configuration for one job from the saved settings.
pub fn speech_config(settings: &Settings) -> Result<SpeechConfig, String> {
    match settings.engine {
        Engine::Local => {
            let artifact = PathBuf::from(settings.local_model_path.clone().unwrap_or_default());
            if artifact.as_os_str().is_empty() {
                return Err("Select an on-device model before dictating".to_owned());
            }
            let model_id = artifact
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unselected".to_owned());
            Ok(SpeechConfig::Local {
                adapter: "whisper.cpp".to_owned(),
                model_id,
                artifact,
            })
        }
        Engine::Cloud => Ok(SpeechConfig::Cloud {
            provider: settings.cloud_provider.clone(),
            model_id: settings.cloud_model(),
            base_url: settings.custom_base_url.clone(),
        }),
        Engine::Remote => {
            if settings.remote_endpoint.trim().is_empty() || settings.remote_model.trim().is_empty()
            {
                return Err("Enter a server URL and model".to_owned());
            }
            Ok(SpeechConfig::OpenAiCompatible {
                model_id: settings.remote_model.clone(),
                endpoint: settings.remote_endpoint.clone(),
                api_key_env: settings.remote_api_key_env.clone(),
            })
        }
    }
}

pub fn begin_recording(
    session: &SessionState,
    app: &AppHandle,
    destination: Option<Destination>,
    verifier: Option<&Arc<Mutex<Verifier>>>,
) -> Result<u64, String> {
    let mut session = lock(session)?;
    if !session.idle() {
        return Err("Dictation is already active or exiting".to_owned());
    }

    #[cfg_attr(not(all(debug_assertions, feature = "input-test")), allow(unused_mut))]
    let mut speech = speech_config(&session.settings);
    #[cfg(all(debug_assertions, feature = "input-test"))]
    if crate::input_test::active() {
        let model_id = if matches!(session.test_audio, audio::Fixture::InferenceError) {
            "test-inference-error"
        } else {
            "test-model.bin"
        };
        speech = Ok(SpeechConfig::Local {
            adapter: "whisper.cpp".to_owned(),
            model_id: model_id.to_owned(),
            artifact: PathBuf::from("test-model.bin"),
        });
    }
    let job = JobSettings {
        microphone_index: session.settings.microphone_index,
        speech: speech?,
        language: session.settings.language_code(),
    };
    let index = job.microphone_index;
    let id = session
        .coordinator
        .start(job, destination.clone(), Instant::now())
        .ok_or("Dictation is already active")?;
    if let Some(verifier) = verifier {
        lock(verifier)?.capture(id, destination.clone());
    }

    #[cfg(all(debug_assertions, feature = "input-test"))]
    let capture = if crate::input_test::active() {
        if !crate::input_test::controlled_window() {
            Err("Controlled test window is not foreground".to_owned())
        } else {
            Ok(audio::Capture::fixture(session.test_audio))
        }
    } else {
        audio::Capture::start(index)
    };
    #[cfg(not(all(debug_assertions, feature = "input-test")))]
    let capture = audio::Capture::start(index);
    let capture = match capture {
        Ok(capture) => capture,
        Err(error) => {
            session.coordinator.cancel(id);
            if let Some(verifier) = verifier {
                lock(verifier)?.cancel();
            }
            return Err(error);
        }
    };
    session.cancelled = Arc::new(AtomicBool::new(false));
    session.history_meta = None;
    session.last_insert = None;
    session.active_id = Some(id);
    session.target = destination;
    session.capture = Some(capture);
    drop(session);
    overlay::track_recording(app, id);
    Ok(id)
}

pub async fn stop_recording(
    session: State<'_, SessionState>,
    app: AppHandle,
    expected: u64,
) -> Result<String, String> {
    let session = Arc::clone(session.inner());
    let (id, capture, cancelled) = {
        let mut session = lock(&session)?;
        let id = session.active_id.ok_or("No recording is active")?;
        if expected != id {
            return Err("Cancelled".to_owned());
        }
        let capture = session.capture.take().ok_or("No recording is active")?;
        (id, capture, Arc::clone(&session.cancelled))
    };
    let recording = capture.stop();
    // A recording that hit the length limit is still transcribed: the limit
    // ends it, like releasing the key.
    let normalized = recording
        .as_ref()
        .ok()
        .map(audio::Recording::normalized_16k);
    let (request, normalized) = {
        let mut session = lock(&session)?;
        if session.active_id != Some(id) || cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let recording = match recording {
            Ok(recording) => recording,
            Err(error) => {
                session.coordinator.cancel(id);
                session.active_id = None;
                return Err(error);
            }
        };
        let normalized = normalized.unwrap_or_default();
        if !audio::meaningful_audio(&normalized) {
            session.coordinator.release(id, false, Instant::now());
            session.active_id = None;
            return Ok("No meaningful audio".to_owned());
        }

        let Some(request) = session.coordinator.release(id, true, Instant::now()) else {
            session.active_id = None;
            return Err("Recording timed out".to_owned());
        };
        let (engine, model) = request.settings.speech.describe();
        session.history_meta = Some(HistoryMeta {
            duration_seconds: recording.samples.len() as f32 / recording.sample_rate as f32,
            engine,
            model,
        });
        (request, normalized)
    };
    // Read what to show, then release the session lock before touching the
    // overlay: `overlay::show` locks the session itself.
    let transcribing = lock(&session)?
        .history_meta
        .as_ref()
        .map(|meta| Overlay::Transcribing {
            engine: meta.engine.clone(),
            model: meta.model.clone(),
        });
    if let Some(state) = transcribing {
        overlay::show(&app, state);
    }

    let worker_cancelled = Arc::clone(&cancelled);
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let cancelled = worker_cancelled;
        #[cfg(all(debug_assertions, feature = "input-test"))]
        if crate::input_test::active() {
            std::thread::sleep(std::time::Duration::from_millis(300));
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }
            if matches!(&request.settings.speech, SpeechConfig::Local { model_id, .. } if model_id == "test-inference-error")
            {
                return Err("Test inference failure".to_owned());
            }
            return Ok("Yap test ✓".to_owned());
        }

        let mut provider =
            speech::load_provider(&request.settings.speech, request.settings.language.as_deref())?;
        provider
            .transcribe(&normalized, &cancelled)
            .map(|transcript| transcript.text)
    })
    .await;
    let mut session = lock(&session)?;
    let result = match result {
        Ok(result) => result,
        Err(error) => Err(error.to_string()),
    };
    if session.active_id != Some(id) {
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
        Ok(_) => {
            session.active_id = None;
            session.target = None;
            session.history_meta = None;
            Err("Transcription was empty or timed out".to_owned())
        }
        Err(error) => {
            session.coordinator.cancel(id);
            session.active_id = None;
            session.target = None;
            session.history_meta = None;
            Err(error)
        }
    }
}

pub fn cancel_recording(session: &SessionState, app: &AppHandle) -> Result<(), String> {
    overlay::hide(app);
    let mut session = lock(session)?;
    session.cancelled.store(true, Ordering::Relaxed);
    if let Some(id) = session.active_id.take() {
        session.coordinator.cancel(id);
    }
    session.capture = None;
    session.target = None;
    session.trial_id = None;
    session.history_meta = None;
    drop(session);
    lock(app.state::<Arc<Mutex<Verifier>>>().inner())?.cancel();
    Ok(())
}

pub fn wait_for_modifiers(
    active: impl Fn() -> bool,
    released: impl Fn() -> bool,
    limit: std::time::Duration,
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
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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

pub async fn insert_transcript(app: AppHandle, id: u64) -> bool {
    let session_for_wait = Arc::clone(app.state::<SessionState>().inner());
    let modifiers_ready = tauri::async_runtime::spawn_blocking(move || {
        wait_for_modifiers(
            || {
                session_for_wait
                    .lock()
                    .is_ok_and(|session| session.active_id == Some(id))
            },
            platform_input::modifiers_released,
            std::time::Duration::from_secs(1),
        )
    })
    .await
    .unwrap_or(false);
    let verifier = Arc::clone(app.state::<Arc<Mutex<Verifier>>>().inner());
    let verified = tauri::async_runtime::spawn_blocking(move || {
        verifier.lock().is_ok_and(|verifier| verifier.check(id))
    })
    .await
    .unwrap_or(false);
    let session_state = Arc::clone(app.state::<SessionState>().inner());
    let (decision, history_meta, method) = {
        let Ok(mut session) = session_state.lock() else {
            return false;
        };
        if session.exiting || session.active_id != Some(id) {
            return false;
        }

        let current = platform_input::focused_destination();
        let safe = platform_input::destination_verified(
            session.target.as_ref(),
            current.as_ref(),
            verified && modifiers_ready,
            platform_input::modifiers_released(),
        );
        let decision = session.coordinator.begin_insert(id, safe);
        session.active_id = None;
        session.target = None;
        (
            decision,
            session.history_meta.take(),
            session.settings.insertion,
        )
    };
    let (inserted, entry) = match decision {
        Decision::Insert {
            destination: Some(destination),
            text,
            ..
        } => {
            let state = Arc::clone(&session_state);
            let target = destination.clone();
            let typed = text.clone();
            let submitted = tauri::async_runtime::spawn_blocking(move || {
                let still_focused = state.lock().is_ok_and(|session| !session.exiting)
                    && platform_input::destination_verified(
                        Some(&target),
                        platform_input::focused_destination().as_ref(),
                        true,
                        platform_input::modifiers_released(),
                    );
                still_focused && send_text(&typed, method)
            })
            .await
            .unwrap_or(false);
            if let Ok(mut session) = session_state.lock() {
                session.coordinator.insertion_result(id);
                if submitted {
                    session.last_insert = Some(LastInsert {
                        destination: destination.clone(),
                        at: Instant::now(),
                    });
                }
            }
            (submitted, submitted.then(|| (destination.app_name(), text)))
        }
        _ => (false, None),
    };

    if let Some((app_name, text)) = &entry {
        overlay::flash(
            &app,
            Overlay::Done {
                words: text.split_whitespace().count(),
                app: app_name.clone(),
            },
        );
    }
    if let (Some(meta), Some((app_name, text))) = (history_meta, entry) {
        let result = app
            .state::<Mutex<History>>()
            .lock()
            .map_err(|error| error.to_string())
            .and_then(|mut history| {
                history.add(
                    app_name,
                    meta.duration_seconds,
                    text,
                    meta.engine,
                    meta.model,
                )
            });
        match result {
            Ok(()) => {
                let _ = app.emit_to("main", "history-updated", ());
            }
            Err(error) => notify(&app, format!("Could not save history: {error}")),
        }
    }

    hotkey::finish();
    if !inserted {
        let message = "Could not insert text — the original field lost focus";
        overlay::flash(
            &app,
            Overlay::Error {
                message: message.to_owned(),
            },
        );
        notify(&app, message);
    }
    inserted
}

fn fail(app: &AppHandle, message: String) {
    if let Ok(verifier) = app.state::<Arc<Mutex<Verifier>>>().lock() {
        verifier.cancel();
    }
    hotkey::finish();
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

fn on_pressed(handle: &AppHandle, window: isize) {
    let same_foreground = window != 0 && platform_input::foreground_window_handle() == window;
    let practicing = same_foreground
        && handle
            .state::<SessionState>()
            .lock()
            .is_ok_and(|session| session.trial_focused)
        && platform_input::app_is_foreground();
    if practicing {
        match onboarding::trial_start(handle.state::<SessionState>(), handle.clone()) {
            Ok(_) => {
                let _ = handle.emit_to("main", "onboarding-listening", ());
            }
            Err(error) => {
                hotkey::finish();
                notify(handle, error);
            }
        }
        return;
    }

    let destination = same_foreground
        .then(platform_input::focused_destination)
        .flatten();
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
    let Ok((id, cancelled, trial_id)) = handle.state::<SessionState>().lock().map(|session| {
        (
            session.active_id,
            Arc::clone(&session.cancelled),
            session.trial_id,
        )
    }) else {
        return;
    };
    let Some(id) = id else {
        return;
    };
    let handle = handle.clone();
    if trial_id == Some(id) {
        tauri::async_runtime::spawn(async move {
            match onboarding::trial_end(id, handle.state::<SessionState>(), handle.clone()).await {
                Ok(text) => {
                    let _ = handle.emit_to("main", "onboarding-transcript", text);
                }
                Err(error) => notify(&handle, error),
            }
            overlay::hide(&handle);
            hotkey::finish();
        });
        return;
    }

    tauri::async_runtime::spawn(async move {
        let result = stop_recording(handle.state::<SessionState>(), handle.clone(), id).await;
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        let still_active = handle
            .state::<SessionState>()
            .lock()
            .is_ok_and(|session| session.active_id == Some(id));
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
        for event in events {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_wait_releases_or_times_out_without_submitting_after_cancellation() {
        let held = Arc::new(AtomicBool::new(true));
        let held_for_release = Arc::clone(&held);
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            held_for_release.store(false, Ordering::Relaxed);
        });
        assert!(wait_for_modifiers(
            || true,
            || !held.load(Ordering::Relaxed),
            std::time::Duration::from_secs(1),
        ));
        release.join().unwrap();
        assert!(!wait_for_modifiers(
            || true,
            || false,
            std::time::Duration::from_millis(10),
        ));
        assert!(!wait_for_modifiers(
            || false,
            || true,
            std::time::Duration::from_secs(1),
        ));
    }

    #[test]
    fn speech_config_follows_the_selected_engine() {
        let mut settings = Settings::default();
        assert!(speech_config(&settings).is_err());
        settings.local_model_path = Some(r"C:\models\ggml-base.bin".into());
        assert!(matches!(
            speech_config(&settings),
            Ok(SpeechConfig::Local { model_id, .. }) if model_id == "ggml-base.bin"
        ));
        settings.engine = Engine::Cloud;
        settings.cloud_provider = "groq".into();
        assert!(matches!(
            speech_config(&settings),
            Ok(SpeechConfig::Cloud { provider, model_id, .. })
                if provider == "groq" && model_id == "whisper-large-v3-turbo"
        ));
        settings.engine = Engine::Remote;
        assert!(speech_config(&settings).is_err());
        settings.remote_endpoint = "https://asr.home.lan/v1/audio/transcriptions".into();
        settings.remote_model = "large-v3".into();
        assert!(matches!(
            speech_config(&settings),
            Ok(SpeechConfig::OpenAiCompatible { .. })
        ));
    }
}
