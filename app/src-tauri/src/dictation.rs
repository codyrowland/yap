//! The dictation pipeline: talk key → record → transcribe → insert.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::coordinator::{Decision, JobInfo, Settings as JobSettings};
use yap_app::history::History;
use yap_app::models::Family;
use yap_app::settings::{Engine, Insertion, Settings};
use yap_app::speech::{self, SpeechConfig};

use crate::overlay::{self, Overlay};
use crate::platform_input::{self, Verifier};
use crate::state::{SessionState, Target, lock};
use crate::{audio, clipboard, hotkey, own_window};

/// Tells the settings window (and onboarding) what happened.
pub fn notify(app: &AppHandle, message: impl Into<String>) {
    let message = message.into();
    crate::logfile::write(&format!("notice: {message}"));
    let _ = app.emit_to("main", "dictation-preview", message);
}

/// Builds the engine configuration for one job from the saved settings.
pub fn speech_config(settings: &Settings) -> Result<SpeechConfig, String> {
    match settings.engine {
        Engine::Local => {
            let artifact = PathBuf::from(settings.local_model_path.clone().unwrap_or_default());
            if artifact.as_os_str().is_empty() {
                return Err("Select an on-device model before dictating".to_owned());
            }
            let model = yap_app::models::for_path(&artifact);
            let model_id = model.map_or_else(
                || {
                    artifact
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unselected".to_owned())
                },
                |model| model.id.to_owned(),
            );
            let adapter = match model.map(|model| model.family) {
                Some(Family::Parakeet | Family::Moonshine) => speech::SHERPA_OFFLINE,
                Some(Family::Nemotron) => speech::SHERPA_STREAMING,
                // Unknown files are treated as whisper.cpp models, as before.
                Some(Family::Whisper) | None => speech::WHISPER_CPP,
            };
            Ok(SpeechConfig::Local {
                adapter: adapter.to_owned(),
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

/// The loaded on-device model, kept between dictations: loading a model
/// takes seconds and would otherwise happen on every key release. Cloud and
/// self-hosted clients are cheap and are built fresh each time.
type LoadedEngine = (
    SpeechConfig,
    Option<String>,
    Box<dyn speech::SpeechProvider>,
);
static LOCAL_ENGINE: Mutex<Option<LoadedEngine>> = Mutex::new(None);

/// Returns the cached engine if it matches, or loads it. Holding the cache
/// lock while loading makes a dictation wait for a preload in progress
/// instead of loading the same model twice.
fn take_engine(
    speech: &SpeechConfig,
    language: &Option<String>,
) -> Result<Box<dyn speech::SpeechProvider>, String> {
    if !matches!(speech, SpeechConfig::Local { .. }) {
        return speech::load_provider(speech, language.as_deref());
    }
    let mut cache = LOCAL_ENGINE.lock().map_err(|error| error.to_string())?;
    match cache.take() {
        Some((cached, cached_language, engine))
            if &cached == speech && &cached_language == language =>
        {
            Ok(engine)
        }
        other => {
            // Free the previous model before loading another one.
            drop(other);
            speech::load_provider(speech, language.as_deref())
        }
    }
}

fn keep_engine(
    speech: &SpeechConfig,
    language: Option<String>,
    engine: Box<dyn speech::SpeechProvider>,
) {
    if matches!(speech, SpeechConfig::Local { .. })
        && let Ok(mut cache) = LOCAL_ENGINE.lock()
    {
        *cache = Some((speech.clone(), language, engine));
    }
}

/// Frees the cached on-device model.
pub fn unload() {
    if let Ok(mut cache) = LOCAL_ENGINE.lock() {
        *cache = None;
    }
}

/// Loads the selected on-device model in the background so the first
/// dictation after starting Yap (or switching models) is fast. Switching to
/// a cloud or self-hosted engine frees the cached model.
pub fn preload(settings: &Settings) {
    let Ok(speech) = speech_config(settings) else {
        return;
    };
    let language = settings.language_code();
    std::thread::spawn(move || {
        if !matches!(speech, SpeechConfig::Local { .. }) {
            unload();
            return;
        }
        if let Ok(engine) = take_engine(&speech, &language) {
            keep_engine(&speech, language, engine);
        }
    });
}

pub fn begin_recording(
    session: &SessionState,
    app: &AppHandle,
    destination: Option<Target>,
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
    let live = job
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
            session.coordinator.fail(id);
            if let Some(verifier) = verifier {
                lock(verifier)?.cancel();
            }
            return Err(error);
        }
    };
    let tap = capture.tap();
    session.capture = Some(capture);
    drop(session);
    overlay::track_recording(app, id);
    if let Some((speech, language)) = live {
        start_live(app, id, speech, language, tap);
    }
    Ok(id)
}

/// A streaming engine's session for the recording in progress.
struct Live {
    id: u64,
    session: Box<dyn speech::LiveSession>,
    /// Raw capture samples already fed.
    fed: usize,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn take_live(id: u64) -> Option<Live> {
    let mut live = LIVE.lock().ok()?;
    if live.as_ref().is_some_and(|live| live.id == id) {
        live.take()
    } else {
        None
    }
}

fn clear_live() {
    if let Ok(mut live) = LIVE.lock() {
        *live = None;
    }
}

/// Feeds a streaming engine while recording `id` and shows its partial text
/// in the overlay. The final decode happens in `stop_recording`, which takes
/// the session over. If the model is still loading when the key is released,
/// `stop_recording` falls back to transcribing the whole recording.
fn start_live(
    app: &AppHandle,
    id: u64,
    speech: SpeechConfig,
    language: Option<String>,
    tap: audio::Tap,
) {
    let app = app.clone();
    std::thread::spawn(move || {
        let still_recording = |app: &AppHandle| {
            app.state::<SessionState>()
                .lock()
                .is_ok_and(|session| session.coordinator.is_active(id) && session.capture.is_some())
        };
        let Ok(mut engine) = take_engine(&speech, &language) else {
            return;
        };
        let session = engine.start_live();
        keep_engine(&speech, language, engine);
        let Some(session) = session else {
            return;
        };
        if !still_recording(&app) {
            return;
        }
        if let Ok(mut live) = LIVE.lock() {
            *live = Some(Live {
                id,
                session,
                fed: 0,
            });
        }

        let mut shown = String::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let active = app
                .state::<SessionState>()
                .lock()
                .is_ok_and(|session| session.coordinator.is_active(id));
            let text = {
                let Ok(mut slot) = LIVE.lock() else {
                    return;
                };
                let Some(live) = slot.as_mut().filter(|live| live.id == id) else {
                    // `stop_recording` took the session.
                    return;
                };
                if !active {
                    *slot = None;
                    return;
                }
                let fresh = tap.read_from(live.fed);
                live.fed += fresh.len();
                live.session.feed(tap.sample_rate, &fresh)
            };
            if text != shown {
                overlay::partial(&app, &text);
                shown = text;
            }
        }
    });
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
    let (id, capture, cancelled, cleanup) = {
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
        (id, capture, cancelled, session.settings.cleanup_enabled)
    };
    let recording = capture.stop();
    // A recording that hit the length limit is still transcribed: the limit
    // ends it, like releasing the key.
    let normalized = recording
        .as_ref()
        .ok()
        .map(audio::Recording::normalized_16k);
    let (request, normalized, raw) = {
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
        // Streaming engines finish from the raw capture they were fed.
        let raw = request
            .settings
            .speech
            .streams()
            .then_some((recording.samples, recording.sample_rate));
        (request, normalized, raw)
    };
    let (engine, model) = request.settings.speech.describe();
    overlay::show(&app, Overlay::Transcribing { engine, model });

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

        if let (Some((samples, rate)), Some(live)) = (raw, take_live(id)) {
            let mut session = live.session;
            session.feed(rate, samples.get(live.fed..).unwrap_or_default());
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }
            return Ok(session.finish());
        }

        let speech = &request.settings.speech;
        let language = request.settings.language.clone();
        let mut provider = take_engine(speech, &language)?;
        let result = provider
            .transcribe(&normalized, &cancelled)
            .map(|transcript| transcript.text);
        keep_engine(speech, language, provider);
        result
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
    clear_live();
    let mut session = lock(session)?;
    session.coordinator.cancel_current();
    session.capture = None;
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
        hotkey::finish();
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
            save_history(&app, app_name, text, info);
            hotkey::finish();
        }
        Err(message) => {
            hotkey::finish();
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

fn save_history(app: &AppHandle, app_name: String, text: String, info: JobInfo) {
    let result = app
        .state::<Mutex<History>>()
        .lock()
        .map_err(|error| error.to_string())
        .and_then(|mut history| {
            history.add(
                app_name,
                info.duration_seconds,
                text,
                info.engine,
                info.model,
            )
        });
    match result {
        Ok(()) => {
            let _ = app.emit_to("main", "history-updated", ());
        }
        Err(error) => notify(app, format!("Could not save history: {error}")),
    }
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
            std::time::Duration::from_secs(1),
        )
    })
    .await
    .unwrap_or(false);
    let verifier = Arc::clone(app.state::<Arc<Mutex<Verifier>>>().inner());
    let verified = tauri::async_runtime::spawn_blocking(move || {
        let verified = verifier
            .lock()
            .map_err(|error| error.to_string())
            .and_then(|verifier| verifier.check_reason(id));
        if let Err(why) = &verified {
            crate::logfile::write(&format!("insert {id}: field check failed: {why}"));
        }
        verified.is_ok()
    })
    .await
    .unwrap_or(false);
    let session_state = Arc::clone(app.state::<SessionState>().inner());
    let (decision, method) = {
        let mut session = session_state.lock().ok()?;
        if !session.coordinator.is_active(id) {
            return None;
        }

        let current = platform_input::focused_destination();
        let original = match session.coordinator.destination(id) {
            Some(Some(Target::App(field))) => Some(field),
            _ => None,
        };
        let safe = platform_input::destination_verified(
            original,
            current.as_ref(),
            verified && modifiers_ready,
            platform_input::modifiers_released(),
        );
        let decision = session.coordinator.begin_insert(id, safe);
        (decision, session.settings.insertion)
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
        let still_focused = state
            .lock()
            .is_ok_and(|session| !session.coordinator.closed())
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
            Ok(SpeechConfig::Local { model_id, adapter, .. })
                if model_id == "base" && adapter == "whisper.cpp"
        ));
        settings.local_model_path = Some(r"C:\models\parakeet-tdt-0.6b-v3".into());
        assert!(matches!(
            speech_config(&settings),
            Ok(SpeechConfig::Local { model_id, adapter, .. })
                if model_id == "parakeet-tdt-0.6b-v3" && adapter == "sherpa-onnx"
        ));
        settings.local_model_path = Some(r"C:\models\my-finetune.bin".into());
        assert!(matches!(
            speech_config(&settings),
            Ok(SpeechConfig::Local { adapter, .. }) if adapter == "whisper.cpp"
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
