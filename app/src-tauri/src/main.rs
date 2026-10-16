mod audio;
mod hotkey;
mod platform_input;

use platform_input::{Destination, Verifier};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, image::Image};
use yap_app::coordinator::{Coordinator, Decision, Phase, Settings};
use yap_app::speech::{self, SpeechConfig};

struct Session {
    capture: Option<audio::Capture>,
    coordinator: Coordinator<Option<Destination>>,
    active_id: Option<u64>,
    target: Option<Destination>,
    cancelled: Arc<AtomicBool>,
    selected_index: usize,
    selected_model_path: Option<String>,
    shortcut: String,
    exiting: bool,
    #[cfg(all(debug_assertions, feature = "input-test"))]
    test_audio: audio::Fixture,
}

type SessionState = Arc<Mutex<Session>>;

struct SavedSettings {
    microphone_index: usize,
    microphone_name: Option<String>,
    model_path: Option<String>,
    shortcut: String,
}

impl Default for SavedSettings {
    fn default() -> Self {
        Self {
            microphone_index: 0,
            microphone_name: None,
            model_path: None,
            shortcut: "F8".to_owned(),
        }
    }
}

impl SavedSettings {
    fn serialize(&self) -> Result<String, String> {
        let microphone_name = self.microphone_name.as_deref().unwrap_or("");
        let model_path = self.model_path.as_deref().unwrap_or("");
        if [microphone_name, model_path]
            .iter()
            .any(|value| value.contains(['\r', '\n']))
        {
            return Err("Settings cannot contain line breaks".to_owned());
        }

        Ok(format!(
            "Yap settings v2\nshortcut={}\nmicrophone_index={}\nmicrophone_name={}\nmodel_path={}\n",
            self.shortcut, self.microphone_index, microphone_name, model_path
        ))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let lines = text.lines().collect::<Vec<_>>();
        let (shortcut, index, name, model) = match lines.as_slice() {
            ["Yap settings v2", shortcut, index, name, model] => (shortcut, index, name, model),
            ["Yap settings v1", shortcut, index, name, model, copy_only]
                if matches!(copy_only.strip_prefix("copy_only="), Some("true" | "false")) =>
            {
                (shortcut, index, name, model)
            }
            _ => return Err("Invalid Yap settings file".to_owned()),
        };
        let shortcut = shortcut
            .strip_prefix("shortcut=")
            .ok_or("Invalid shortcut setting")?;
        if !matches!(shortcut, "F8" | "F9" | "F10") {
            return Err("Invalid shortcut setting".to_owned());
        }

        let microphone_index = index
            .strip_prefix("microphone_index=")
            .ok_or("Invalid microphone setting")?
            .parse()
            .map_err(|_| "Invalid microphone setting")?;
        let microphone_name = name
            .strip_prefix("microphone_name=")
            .ok_or("Invalid microphone setting")?;
        let model_path = model
            .strip_prefix("model_path=")
            .ok_or("Invalid model setting")?;
        Ok(Self {
            microphone_index,
            microphone_name: (!microphone_name.is_empty()).then(|| microphone_name.to_owned()),
            model_path: (!model_path.is_empty()).then(|| model_path.to_owned()),
            shortcut: shortcut.to_owned(),
        })
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join("settings.txt"))
        .map_err(|error| error.to_string())
}

fn load_settings(app: &AppHandle) -> Result<SavedSettings, String> {
    match std::fs::read_to_string(settings_path(app)?) {
        Ok(text) => SavedSettings::parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SavedSettings::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn save_settings(app: &AppHandle, settings: &SavedSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    std::fs::create_dir_all(path.parent().ok_or("Invalid settings path")?)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, settings.serialize()?).map_err(|error| error.to_string())
}

fn show_indicator(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("indicator") {
        let _ = window.eval("document.getElementById('state').textContent = 'Recording'");
        let _ = window.show();
    }
}

fn hide_indicator(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("indicator") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn microphones() -> Result<Vec<String>, String> {
    audio::microphones()
}

#[tauri::command]
fn current_settings(
    session: State<'_, SessionState>,
) -> Result<(usize, Option<String>, String), String> {
    let session = session.lock().map_err(|error| error.to_string())?;
    Ok((
        session.selected_index,
        session.selected_model_path.clone(),
        session.shortcut.clone(),
    ))
}

#[tauri::command]
fn configure(
    index: usize,
    model_path: Option<String>,
    shortcut: String,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Cannot change settings while dictating or exiting".to_owned());
    }

    let name = audio::microphones()?
        .get(index)
        .cloned()
        .ok_or("Microphone is no longer available")?;
    let saved = SavedSettings {
        microphone_index: index,
        microphone_name: Some(name),
        model_path: model_path.clone(),
        shortcut: shortcut.clone(),
    };
    hotkey::update_shortcut(&shortcut, || save_settings(&app, &saved))?;

    session.selected_index = index;
    session.selected_model_path = model_path;
    session.shortcut = shortcut;
    Ok(())
}

fn begin_recording(
    index: usize,
    model_path: Option<String>,
    session: &SessionState,
    app: &AppHandle,
    destination: Option<Destination>,
    verifier: Option<&Arc<Mutex<Verifier>>>,
) -> Result<u64, String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Dictation is already active or exiting".to_owned());
    }

    session.selected_index = index;
    session.selected_model_path = model_path.clone();
    let artifact = PathBuf::from(model_path.unwrap_or_default());
    #[cfg(all(debug_assertions, feature = "input-test"))]
    let artifact = if input_test_mode() {
        PathBuf::from("test-model.bin")
    } else {
        artifact
    };
    if artifact.as_os_str().is_empty() {
        return Err("Select a local model before dictating".to_owned());
    }

    let model_id = artifact
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unselected".to_owned());
    #[cfg(all(debug_assertions, feature = "input-test"))]
    let model_id =
        if input_test_mode() && matches!(session.test_audio, audio::Fixture::InferenceError) {
            "test-inference-error".to_owned()
        } else {
            model_id
        };
    let settings = Settings {
        microphone_index: index,
        speech: SpeechConfig::Local {
            adapter: "whisper.cpp".to_owned(),
            model_id,
            artifact,
        },
    };
    let id = session
        .coordinator
        .start(settings, destination.clone(), Instant::now())
        .unwrap();
    if let Some(verifier) = verifier {
        verifier
            .lock()
            .map_err(|error| error.to_string())?
            .capture(id, destination.clone());
    }

    #[cfg(all(debug_assertions, feature = "input-test"))]
    let capture = if input_test_mode() {
        if !controlled_test_window() {
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
                verifier.lock().map_err(|lock| lock.to_string())?.cancel();
            }

            return Err(error);
        }
    };
    session.cancelled = Arc::new(AtomicBool::new(false));
    session.active_id = Some(id);
    session.target = destination;
    session.capture = Some(capture);
    drop(session);
    show_indicator(app);
    Ok(id)
}

async fn stop_recording(
    session: State<'_, SessionState>,
    app: AppHandle,
    expected: u64,
) -> Result<String, String> {
    let session = Arc::clone(session.inner());
    let (id, request, normalized, cancelled) = {
        let mut session = session.lock().map_err(|error| error.to_string())?;
        let id = session
            .active_id
            .ok_or_else(|| "No recording is active".to_owned())?;
        if expected != id {
            return Err("Cancelled".to_owned());
        }

        let capture = session
            .capture
            .take()
            .ok_or_else(|| "No recording is active".to_owned())?;
        hide_indicator(&app);
        let recording = match capture.stop() {
            Ok(recording) => recording,
            Err(error) => {
                session.coordinator.cancel(id);
                session.active_id = None;
                return Err(error);
            }
        };
        if recording.limited {
            session.coordinator.cancel(id);
            session.active_id = None;
            return Err("Recording exceeded 30 seconds; try a shorter utterance".to_owned());
        }

        let normalized = recording.normalized_16k();
        if !audio::meaningful_audio(&normalized) {
            session.coordinator.release(id, false, Instant::now());
            session.active_id = None;
            return Ok("No meaningful audio".to_owned());
        }

        let request = match session.coordinator.release(id, true, Instant::now()) {
            Some(request) => request,
            None => {
                session.active_id = None;
                return Err("Recording timed out".to_owned());
            }
        };
        (id, request, normalized, Arc::clone(&session.cancelled))
    };

    let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        #[cfg(all(debug_assertions, feature = "input-test"))]
        if input_test_mode() {
            std::thread::sleep(std::time::Duration::from_millis(300));
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }
            if matches!(&request.settings.speech, SpeechConfig::Local { model_id, .. } if model_id == "test-inference-error") {
                return Err("Test inference failure".to_owned());
            }
            return Ok("Yap test ✓".to_owned());
        }

        let mut provider = speech::load_provider(&request.settings.speech)?;
        provider
            .transcribe(&normalized, &cancelled)
            .map(|transcript| transcript.text)
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let mut session = session.lock().map_err(|lock| lock.to_string())?;
            if session.active_id == Some(id) {
                session.coordinator.cancel(id);
                session.active_id = None;
            }

            return Err(error.to_string());
        }
    };
    let mut session = session.lock().map_err(|error| error.to_string())?;
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
            Err("Transcription was empty or timed out".to_owned())
        }
        Err(error) => {
            session.coordinator.cancel(id);
            session.active_id = None;
            session.target = None;
            Err(error)
        }
    }
}

fn cancel_recording(session: State<'_, SessionState>, app: AppHandle) -> Result<(), String> {
    hide_indicator(&app);
    let mut session = session.lock().map_err(|error| error.to_string())?;
    session.cancelled.store(true, Ordering::Relaxed);
    if let Some(id) = session.active_id.take() {
        session.coordinator.cancel(id);
    }

    session.capture = None;
    session.target = None;
    drop(session);
    app.state::<Arc<Mutex<Verifier>>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cancel();
    Ok(())
}

impl Session {
    fn stop_for_exit(&mut self) -> Option<audio::Capture> {
        self.exiting = true;
        self.cancelled.store(true, Ordering::Relaxed);
        self.active_id = None;
        self.target = None;
        self.coordinator = Coordinator::default();
        self.capture.take()
    }
}

fn shutdown(app: &AppHandle) {
    let capture = app.state::<SessionState>().lock().unwrap().stop_for_exit();
    drop(capture);
    app.state::<Arc<Mutex<Verifier>>>().lock().unwrap().cancel();
    hotkey::finish();
    hide_indicator(app);
    app.exit(0);
}

fn wait_for_modifiers(
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

async fn insert_transcript(app: AppHandle, id: u64) -> bool {
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
    let session_state = app.state::<SessionState>();
    let decision = {
        let mut session = session_state.lock().unwrap();
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
        decision
    };
    let inserted = match decision {
        Decision::Insert {
            destination: Some(destination),
            text,
            ..
        } => {
            let mut session = session_state.lock().unwrap();
            let submitted = !session.exiting
                && platform_input::destination_verified(
                    Some(&destination),
                    platform_input::focused_destination().as_ref(),
                    true,
                    platform_input::modifiers_released(),
                )
                && platform_input::insert_unicode(&text);
            session.coordinator.insertion_result(id);
            submitted
        }
        _ => false,
    };

    hotkey::finish();
    if !inserted {
        let _ = app.emit_to(
            "main",
            "dictation-preview",
            "Could not insert text at the original cursor",
        );
    }
    inserted
}

#[cfg(all(debug_assertions, feature = "input-test"))]
fn input_test_mode() -> bool {
    std::env::args().any(|arg| arg == "--input-test")
        && std::env::var("YAP_INPUT_TEST").as_deref() == Ok("1")
}

#[cfg(all(debug_assertions, feature = "input-test"))]
fn controlled_test_window() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

    let mut title = [0u16; 128];
    let count = unsafe {
        GetWindowTextW(
            GetForegroundWindow(),
            title.as_mut_ptr(),
            title.len() as i32,
        )
    };
    matches!(
        String::from_utf16_lossy(&title[..count as usize]).as_str(),
        "Yap controlled input test" | "Yap Chrome test focused"
    )
}

#[cfg(all(debug_assertions, feature = "input-test"))]
fn start_input_test(app: AppHandle) {
    use std::io::{BufRead, Write};

    std::thread::spawn(move || {
        let mut last_id = None;
        println!("YAP_TEST ready");
        let _ = std::io::stdout().flush();
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            let result = match line.as_str() {
                "start" => {
                    if !controlled_test_window() {
                        Err("Controlled test window is not foreground".to_owned())
                    } else {
                        let target = platform_input::focused_destination();
                        let state = app.state::<SessionState>();
                        let mut session = state.lock().unwrap();
                        if session.coordinator.phase() != Phase::Idle {
                            Err("Job already active".to_owned())
                        } else {
                            let id = session
                                .coordinator
                                .start(
                                    Settings {
                                        microphone_index: 0,
                                        speech: SpeechConfig::Local {
                                            adapter: "test".to_owned(),
                                            model_id: "test".to_owned(),
                                            artifact: PathBuf::new(),
                                        },
                                    },
                                    target.clone(),
                                    Instant::now(),
                                )
                                .unwrap();
                            session.active_id = Some(id);
                            session.target = target.clone();
                            last_id = Some(id);
                            app.state::<Arc<Mutex<Verifier>>>()
                                .lock()
                                .unwrap()
                                .capture(id, target);
                            Ok("started".to_owned())
                        }
                    }
                }
                "audio_silence"
                | "audio_bounded"
                | "audio_speech"
                | "audio_capture_error"
                | "audio_inference_error" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.coordinator.phase() != Phase::Idle {
                        Err("Job already active".to_owned())
                    } else {
                        session.test_audio = match line.as_str() {
                            "audio_silence" => audio::Fixture::Silence,
                            "audio_bounded" => audio::Fixture::Bounded,
                            "audio_capture_error" => audio::Fixture::CaptureError,
                            "audio_inference_error" => audio::Fixture::InferenceError,
                            _ => audio::Fixture::Speech,
                        };
                        Ok(line.clone())
                    }
                }
                "phase" => {
                    let state = app.state::<SessionState>();
                    let session = state.lock().unwrap();
                    Ok(format!("phase_{:?}", session.coordinator.phase()))
                }
                "release" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.active_id.is_some_and(|id| {
                        session
                            .coordinator
                            .release(id, true, Instant::now())
                            .is_some()
                    }) {
                        Ok("released".to_owned())
                    } else {
                        Err("Could not release job".to_owned())
                    }
                }
                "transcript" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.active_id.is_some_and(|id| {
                        session
                            .coordinator
                            .transcript(id, "Yap test ✓".to_owned(), Instant::now())
                    }) {
                        Ok("transcribed".to_owned())
                    } else {
                        Err("Could not transcribe job".to_owned())
                    }
                }
                "submit" => {
                    let state = app.state::<SessionState>();
                    let session = state.lock().unwrap();
                    if session.coordinator.phase() != Phase::Ready {
                        Err("Job is not ready".to_owned())
                    } else {
                        let id = session.active_id.unwrap();
                        drop(session);
                        let inserted =
                            tauri::async_runtime::block_on(insert_transcript(app.clone(), id));
                        Ok(if inserted { "inserted" } else { "not_inserted" }.to_owned())
                    }
                }
                "cancel" => cancel_recording(app.state::<SessionState>(), app.clone())
                    .map(|()| "cancelled".to_owned()),
                "late" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if last_id.is_some_and(|id| {
                        session
                            .coordinator
                            .transcript(id, "late result".to_owned(), Instant::now())
                    }) {
                        Err("Stale transcript was accepted".to_owned())
                    } else {
                        Ok("ignored".to_owned())
                    }
                }
                _ => Err("Unknown test command".to_owned()),
            };
            match result {
                Ok(message) => println!("YAP_TEST {message}"),
                Err(error) => println!("YAP_TEST error: {error}"),
            }
            let _ = std::io::stdout().flush();
        }
    });
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(Session {
            capture: None,
            coordinator: Coordinator::default(),
            active_id: None,
            target: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            selected_index: 0,
            selected_model_path: None,
            shortcut: "F8".to_owned(),
            exiting: false,
            #[cfg(all(debug_assertions, feature = "input-test"))]
            test_audio: audio::Fixture::Speech,
        })))
        .manage(Arc::new(Mutex::new(Verifier::start())))
        .invoke_handler(tauri::generate_handler![
            microphones,
            current_settings,
            configure
        ])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Show Yap", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let icon = Image::new_owned([30, 120, 200, 255].repeat(16 * 16), 16, 16);
            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => shutdown(app),
                    _ => {}
                })
                .build(app)?;
            let saved = load_settings(app.handle()).map_err(std::io::Error::other)?;
            let names = audio::microphones().unwrap_or_default();
            let index = saved
                .microphone_name
                .as_ref()
                .map_or(saved.microphone_index, |name| {
                    if names.get(saved.microphone_index) == Some(name) {
                        saved.microphone_index
                    } else {
                        names
                            .iter()
                            .position(|candidate| candidate == name)
                            .unwrap_or(0)
                    }
                });
            {
                let state = app.state::<SessionState>();
                let mut session = state.lock().unwrap();
                session.selected_index = index.min(names.len().saturating_sub(1));
                session.selected_model_path = saved.model_path;
                session.shortcut = saved.shortcut;
            }

            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test_mode() {
                app.state::<SessionState>().lock().unwrap().shortcut = "F8".to_owned();
                start_input_test(app.handle().clone());
            }

            let shortcut = app.state::<SessionState>().lock().unwrap().shortcut.clone();
            let (hook, events) = hotkey::start(&shortcut).map_err(std::io::Error::other)?;
            app.manage(Mutex::new(Some(hook)));
            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test_mode() {
                println!("YAP_TEST hotkey_ready");
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for event in events {
                    match event {
                        hotkey::Event::Pressed(destination) => {
                            let (index, model_path) = {
                                let state = handle.state::<SessionState>();
                                let session = state.lock().unwrap();
                                (session.selected_index, session.selected_model_path.clone())
                            };
                            let verifier =
                                Arc::clone(handle.state::<Arc<Mutex<Verifier>>>().inner());
                            if let Err(error) = begin_recording(
                                index,
                                model_path,
                                handle.state::<SessionState>().inner(),
                                &handle,
                                destination,
                                Some(&verifier),
                            ) {
                                hotkey::finish();
                                let _ = handle.emit_to("main", "dictation-preview", error);
                            }
                        }
                        hotkey::Event::Released => {
                            let (id, cancelled) = {
                                let state = handle.state::<SessionState>();
                                let session = state.lock().unwrap();
                                (session.active_id, Arc::clone(&session.cancelled))
                            };
                            if let Some(id) = id {
                                let handle = handle.clone();
                                tauri::async_runtime::spawn(async move {
                                    if handle.state::<SessionState>().lock().unwrap().active_id
                                        != Some(id)
                                    {
                                        return;
                                    }

                                    let result = stop_recording(
                                        handle.state::<SessionState>(),
                                        handle.clone(),
                                        id,
                                    )
                                    .await;
                                    if cancelled.load(Ordering::Relaxed) {
                                        return;
                                    }

                                    match result {
                                        Ok(_)
                                            if handle
                                                .state::<SessionState>()
                                                .lock()
                                                .unwrap()
                                                .active_id
                                                == Some(id) =>
                                        {
                                            insert_transcript(handle, id).await;
                                        }
                                        Ok(text) => {
                                            handle
                                                .state::<Arc<Mutex<Verifier>>>()
                                                .lock()
                                                .unwrap()
                                                .cancel();
                                            hotkey::finish();
                                            let _ =
                                                handle.emit_to("main", "dictation-preview", text);
                                        }
                                        Err(error) => {
                                            handle
                                                .state::<Arc<Mutex<Verifier>>>()
                                                .lock()
                                                .unwrap()
                                                .cancel();
                                            hotkey::finish();
                                            if error != "Cancelled" {
                                                let _ = handle.emit_to(
                                                    "main",
                                                    "dictation-preview",
                                                    error,
                                                );
                                            }
                                        }
                                    }
                                });
                            }
                        }
                        hotkey::Event::Cancelled => {
                            let _ =
                                cancel_recording(handle.state::<SessionState>(), handle.clone());
                            hotkey::finish();
                            let _ = handle.emit_to("main", "dictation-preview", "Cancelled");
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start Yap");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_without_storing_speech() {
        let saved = SavedSettings {
            microphone_index: 3,
            microphone_name: Some("USB microphone".into()),
            model_path: Some(r"C:\Users\Test\models\ggml-base.bin".into()),
            shortcut: "F9".into(),
        };
        let text = saved.serialize().unwrap();
        assert!(text.starts_with("Yap settings v2\n"));
        assert!(!text.contains("copy_only"));
        assert!(!text.contains("transcript"));
        let restored = SavedSettings::parse(&text).unwrap();
        assert_eq!(restored.microphone_index, 3);
        assert_eq!(restored.microphone_name, saved.microphone_name);
        assert_eq!(restored.model_path, saved.model_path);
        assert_eq!(restored.shortcut, "F9");
    }

    #[test]
    fn old_settings_migrate_without_copy_only() {
        for value in ["true", "false"] {
            let old = format!(
                "Yap settings v1\nshortcut=F9\nmicrophone_index=3\nmicrophone_name=USB microphone\nmodel_path=model.bin\ncopy_only={value}\n"
            );
            let restored = SavedSettings::parse(&old).unwrap();
            assert_eq!(restored.microphone_index, 3);
            assert_eq!(restored.microphone_name.as_deref(), Some("USB microphone"));
            assert_eq!(restored.model_path.as_deref(), Some("model.bin"));
            assert_eq!(restored.shortcut, "F9");
            assert!(!restored.serialize().unwrap().contains("copy_only"));
        }
    }

    #[test]
    fn modifier_wait_releases_or_times_out_without_submitting_after_cancellation() {
        use std::sync::atomic::AtomicBool;

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
    fn exit_discards_pending_job_and_rejects_late_transcript() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut session = Session {
            capture: None,
            coordinator: Coordinator::default(),
            active_id: None,
            target: None,
            cancelled: Arc::clone(&cancelled),
            selected_index: 0,
            selected_model_path: None,
            shortcut: "F8".to_owned(),
            exiting: false,
            #[cfg(all(debug_assertions, feature = "input-test"))]
            test_audio: audio::Fixture::Speech,
        };
        let now = Instant::now();
        let id = session
            .coordinator
            .start(
                Settings {
                    microphone_index: 0,
                    speech: SpeechConfig::Local {
                        adapter: "test".to_owned(),
                        model_id: "test".to_owned(),
                        artifact: PathBuf::new(),
                    },
                },
                None,
                now,
            )
            .unwrap();
        session.active_id = Some(id);
        session.coordinator.release(id, true, now);
        assert!(session.stop_for_exit().is_none());
        assert!(session.exiting);
        assert!(cancelled.load(Ordering::Relaxed));
        assert_eq!(session.active_id, None);
        assert_eq!(session.coordinator.phase(), Phase::Idle);
        assert!(!session.coordinator.transcript(id, "late".to_owned(), now));
        assert_eq!(session.coordinator.begin_insert(id, true), Decision::None);
    }

    #[test]
    fn invalid_settings_do_not_silently_restore_defaults() {
        assert!(SavedSettings::parse("Yap settings v1\nshortcut=F1").is_err());
        assert!(
            SavedSettings {
                model_path: Some("a\nb".into()),
                ..SavedSettings::default()
            }
            .serialize()
            .is_err()
        );
    }
}
