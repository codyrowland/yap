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
use yap_app::credential_store;
use yap_app::history::{Entry, History};
use yap_app::models;
use yap_app::speech::{self, SpeechConfig};

struct HistoryMeta {
    duration_seconds: f32,
    engine: String,
    model: String,
}

struct Session {
    capture: Option<audio::Capture>,
    history_meta: Option<HistoryMeta>,
    coordinator: Coordinator<Option<Destination>>,
    active_id: Option<u64>,
    trial_id: Option<u64>,
    trial_focused: bool,
    target: Option<Destination>,
    cancelled: Arc<AtomicBool>,
    selected_index: usize,
    selected_model_path: Option<String>,
    selected_provider: String,
    selected_endpoint: String,
    selected_remote_model: String,
    selected_cloud_model: String,
    selected_api_key_env: Option<String>,
    shortcut: String,
    toggle_shortcut: String,
    exiting: bool,
    #[cfg(all(debug_assertions, feature = "input-test"))]
    test_audio: audio::Fixture,
}

type SessionState = Arc<Mutex<Session>>;
type ModelDownloadState = Arc<Mutex<Option<(String, Arc<AtomicBool>)>>>;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelStatus {
    id: String,
    bytes: u64,
    path: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelProgress {
    id: String,
    downloaded: u64,
    total: u64,
    state: &'static str,
    error: Option<String>,
}

struct SavedSettings {
    microphone_index: usize,
    microphone_name: Option<String>,
    model_path: Option<String>,
    provider: String,
    endpoint: String,
    remote_model: String,
    cloud_model: String,
    api_key_env: Option<String>,
    shortcut: String,
    toggle_shortcut: String,
}

impl Default for SavedSettings {
    fn default() -> Self {
        Self {
            microphone_index: 0,
            microphone_name: None,
            model_path: None,
            provider: "local".to_owned(),
            endpoint: String::new(),
            remote_model: String::new(),
            cloud_model: "gpt-4o-mini-transcribe".to_owned(),
            api_key_env: None,
            shortcut: "Ctrl+Space".to_owned(),
            toggle_shortcut: "Ctrl+Shift+Space".to_owned(),
        }
    }
}

impl SavedSettings {
    fn serialize(&self) -> Result<String, String> {
        let microphone_name = self.microphone_name.as_deref().unwrap_or("");
        let model_path = self.model_path.as_deref().unwrap_or("");
        let api_key_env = self.api_key_env.as_deref().unwrap_or("");
        if [
            microphone_name,
            model_path,
            &self.endpoint,
            &self.remote_model,
            &self.cloud_model,
            api_key_env,
        ]
        .iter()
        .any(|value| value.contains(['\r', '\n']))
        {
            return Err("Settings cannot contain line breaks".to_owned());
        }

        if !matches!(self.provider.as_str(), "local" | "openai-compatible" | "openai") {
            return Err("Invalid speech provider".to_owned());
        }

        hotkey::validate_shortcuts(&self.shortcut, &self.toggle_shortcut)?;
        Ok(format!(
            "Yap settings v5\nshortcut={}\ntoggle_shortcut={}\nmicrophone_index={}\nmicrophone_name={}\nmodel_path={}\nprovider={}\nendpoint={}\nremote_model={}\napi_key_env={}\ncloud_model={}\n",
            self.shortcut,
            self.toggle_shortcut,
            self.microphone_index,
            microphone_name,
            model_path,
            self.provider,
            self.endpoint,
            self.remote_model,
            api_key_env,
            self.cloud_model
        ))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let lines = text.lines().collect::<Vec<_>>();
        let (
            shortcut,
            toggle_shortcut,
            index,
            name,
            model,
            provider,
            endpoint,
            remote_model,
            api_key_env,
            cloud_model,
        ) = match lines.as_slice() {
            [
                version @ ("Yap settings v4" | "Yap settings v5"),
                shortcut,
                toggle_shortcut,
                index,
                name,
                model,
                provider,
                endpoint,
                remote_model,
                api_key_env,
                rest @ ..,
            ] => {
                let cloud_model = match (*version, rest) {
                    ("Yap settings v4", []) => "gpt-4o-mini-transcribe",
                    ("Yap settings v5", [value]) => value
                        .strip_prefix("cloud_model=")
                        .ok_or("Invalid cloud model setting")?,
                    _ => return Err("Invalid Yap settings file".to_owned()),
                };
                let provider = provider
                    .strip_prefix("provider=")
                    .ok_or("Invalid speech provider")?;
                let endpoint = endpoint
                    .strip_prefix("endpoint=")
                    .ok_or("Invalid endpoint setting")?;
                let remote_model = remote_model
                    .strip_prefix("remote_model=")
                    .ok_or("Invalid remote model setting")?;
                let api_key_env = api_key_env
                    .strip_prefix("api_key_env=")
                    .ok_or("Invalid API key setting")?;
                let toggle = toggle_shortcut
                    .strip_prefix("toggle_shortcut=")
                    .ok_or("Invalid toggle shortcut setting")?;
                (
                    shortcut,
                    toggle,
                    index,
                    name,
                    model,
                    provider,
                    endpoint,
                    remote_model,
                    api_key_env,
                    cloud_model,
                )
            }
            [
                "Yap settings v3",
                shortcut,
                index,
                name,
                model,
                provider,
                endpoint,
                remote_model,
                api_key_env,
            ] => {
                let provider = provider
                    .strip_prefix("provider=")
                    .ok_or("Invalid speech provider")?;
                let endpoint = endpoint
                    .strip_prefix("endpoint=")
                    .ok_or("Invalid endpoint setting")?;
                let remote_model = remote_model
                    .strip_prefix("remote_model=")
                    .ok_or("Invalid remote model setting")?;
                let api_key_env = api_key_env
                    .strip_prefix("api_key_env=")
                    .ok_or("Invalid API key setting")?;
                (
                    shortcut,
                    "Ctrl+Shift+Space",
                    index,
                    name,
                    model,
                    provider,
                    endpoint,
                    remote_model,
                    api_key_env,
                    "gpt-4o-mini-transcribe",
                )
            }
            ["Yap settings v2", shortcut, index, name, model] => (
                shortcut,
                "Ctrl+Shift+Space",
                index,
                name,
                model,
                "local",
                "",
                "",
                "",
                "gpt-4o-mini-transcribe",
            ),
            ["Yap settings v1", shortcut, index, name, model, copy_only]
                if matches!(copy_only.strip_prefix("copy_only="), Some("true" | "false")) =>
            {
                (
                    shortcut,
                    "Ctrl+Shift+Space",
                    index,
                    name,
                    model,
                    "local",
                    "",
                    "",
                    "",
                    "gpt-4o-mini-transcribe",
                )
            }
            _ => return Err("Invalid Yap settings file".to_owned()),
        };
        if !matches!(provider, "local" | "openai-compatible" | "openai") {
            return Err("Invalid speech provider".to_owned());
        }
        let shortcut = shortcut
            .strip_prefix("shortcut=")
            .ok_or("Invalid shortcut setting")?;
        hotkey::validate_shortcuts(shortcut, toggle_shortcut)?;

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
            provider: provider.to_owned(),
            endpoint: endpoint.to_owned(),
            remote_model: remote_model.to_owned(),
            cloud_model: cloud_model.to_owned(),
            api_key_env: (!api_key_env.is_empty()).then(|| api_key_env.to_owned()),
            shortcut: shortcut.to_owned(),
            toggle_shortcut: toggle_shortcut.to_owned(),
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

fn onboarding_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join("onboarding_pending"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn onboarding_needed(app: AppHandle) -> Result<bool, String> {
    let path = onboarding_path(&app)?;
    if path.exists() {
        return Ok(true);
    }

    if settings_path(&app)?.exists() {
        return Ok(false);
    }

    std::fs::create_dir_all(path.parent().ok_or("Invalid onboarding path")?)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, []).map_err(|error| error.to_string())?;
    Ok(true)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionStatus {
    microphone: bool,
    microphone_error: Option<String>,
    accessibility: bool,
}

#[tauri::command]
fn onboarding_permissions(index: usize) -> PermissionStatus {
    let microphone = audio::Capture::start(index).map(|capture| drop(capture));
    PermissionStatus {
        microphone: microphone.is_ok(),
        microphone_error: microphone.err(),
        accessibility: platform_input::accessibility_ready(),
    }
}

#[tauri::command]
fn complete_onboarding(session: State<'_, SessionState>, app: AppHandle) -> Result<(), String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    if session.coordinator.phase() != Phase::Idle {
        return Err("Finish the recording before completing setup".to_owned());
    }

    if session.selected_provider == "local"
        && !session
            .selected_model_path
            .as_ref()
            .is_some_and(|path| std::path::Path::new(path).is_file())
    {
        return Err("Select a downloaded model before finishing".to_owned());
    }

    if session.selected_provider == "openai-compatible" {
        let url = reqwest::Url::parse(&session.selected_endpoint)
            .map_err(|_| "Enter a valid transcription URL".to_owned())?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || session.selected_remote_model.trim().is_empty()
        {
            return Err("Enter a valid transcription URL and model".to_owned());
        }
    }

    session.trial_focused = false;
    drop(session);
    match std::fs::remove_file(onboarding_path(&app)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    index: usize,
    model_path: Option<String>,
    shortcut: String,
    toggle_shortcut: String,
    provider: String,
    endpoint: String,
    remote_model: String,
    cloud_model: String,
    api_key_env: Option<String>,
}

#[tauri::command]
fn current_settings(session: State<'_, SessionState>) -> Result<SettingsView, String> {
    let session = session.lock().map_err(|error| error.to_string())?;
    Ok(SettingsView {
        index: session.selected_index,
        model_path: session.selected_model_path.clone(),
        shortcut: session.shortcut.clone(),
        toggle_shortcut: session.toggle_shortcut.clone(),
        provider: session.selected_provider.clone(),
        endpoint: session.selected_endpoint.clone(),
        remote_model: session.selected_remote_model.clone(),
        cloud_model: session.selected_cloud_model.clone(),
        api_key_env: session.selected_api_key_env.clone(),
    })
}

#[tauri::command]
fn configure(
    index: usize,
    model_path: Option<String>,
    shortcut: String,
    toggle_shortcut: String,
    provider: String,
    endpoint: String,
    remote_model: String,
    cloud_model: String,
    api_key_env: Option<String>,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Cannot change settings while dictating or exiting".to_owned());
    }

    if !matches!(provider.as_str(), "local" | "openai-compatible" | "openai") {
        return Err("Invalid speech provider".to_owned());
    }

    if provider == "openai" && cloud_model.trim().is_empty() {
        return Err("Select an OpenAI transcription model".to_owned());
    }

    let name = audio::microphones()?
        .get(index)
        .cloned()
        .ok_or("Microphone is no longer available")?;
    let saved = SavedSettings {
        microphone_index: index,
        microphone_name: Some(name),
        model_path: model_path.clone(),
        provider: provider.clone(),
        endpoint: endpoint.clone(),
        remote_model: remote_model.clone(),
        cloud_model: cloud_model.clone(),
        api_key_env: api_key_env.clone(),
        shortcut: shortcut.clone(),
        toggle_shortcut: toggle_shortcut.clone(),
    };
    hotkey::update_shortcut(&shortcut, &toggle_shortcut, || save_settings(&app, &saved))?;

    session.selected_index = index;
    session.selected_model_path = model_path;
    session.selected_provider = provider;
    session.selected_endpoint = endpoint;
    session.selected_remote_model = remote_model;
    session.selected_cloud_model = cloud_model;
    session.selected_api_key_env = api_key_env;
    session.shortcut = shortcut;
    session.toggle_shortcut = toggle_shortcut;
    Ok(())
}

#[tauri::command]
fn begin_key_capture(kind: String) -> Result<(), String> {
    hotkey::begin_capture(&kind)
}

#[tauri::command]
fn cancel_key_capture() -> Result<(), String> {
    hotkey::cancel_capture()
}

#[tauri::command]
fn complete_key_capture(kind: String, shortcut: String) -> Result<(), String> {
    hotkey::complete_capture(&kind, &shortcut)
}

fn models_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("models"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn model_catalog(app: AppHandle) -> Result<Vec<ModelStatus>, String> {
    let directory = models_path(&app)?;
    Ok(models::WHISPER_MODELS
        .iter()
        .map(|model| ModelStatus {
            id: model.id.to_owned(),
            bytes: model.bytes,
            path: models::installed(model, &directory).then(|| {
                directory
                    .join(model.filename)
                    .to_string_lossy()
                    .into_owned()
            }),
        })
        .collect())
}

#[tauri::command]
fn download_model(
    id: String,
    app: AppHandle,
    downloads: State<'_, ModelDownloadState>,
) -> Result<(), String> {
    if !cfg!(feature = "local-whisper") {
        return Err("Local Whisper is unavailable in this build".to_owned());
    }

    let model = models::model(&id).ok_or("Unknown model")?;
    let directory = models_path(&app)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = downloads.lock().map_err(|error| error.to_string())?;
        if active.is_some() {
            return Err("A model download is already in progress".to_owned());
        }

        *active = Some((id.clone(), Arc::clone(&cancelled)));
    }

    let downloads = Arc::clone(downloads.inner());
    std::thread::spawn(move || {
        let mut last_percent = 0;
        let result = models::download(model, &directory, &cancelled, |downloaded| {
            let percent = downloaded.saturating_mul(100) / model.bytes;
            if percent > last_percent {
                last_percent = percent;
                let _ = app.emit_to(
                    "main",
                    "model-download-progress",
                    ModelProgress {
                        id: id.clone(),
                        downloaded,
                        total: model.bytes,
                        state: "downloading",
                        error: None,
                    },
                );
            }
        });
        let (state, error) = match result {
            Ok(_) => ("ready", None),
            Err(error) if error == "Cancelled" => ("cancelled", None),
            Err(error) => ("error", Some(error)),
        };
        let _ = app.emit_to(
            "main",
            "model-download-progress",
            ModelProgress {
                id,
                downloaded: 0,
                total: model.bytes,
                state,
                error,
            },
        );
        if let Ok(mut active) = downloads.lock() {
            *active = None;
        }
    });
    Ok(())
}

#[tauri::command]
fn cancel_model_download(
    id: String,
    downloads: State<'_, ModelDownloadState>,
) -> Result<(), String> {
    let active = downloads.lock().map_err(|error| error.to_string())?;
    if let Some((current, cancelled)) = active.as_ref() {
        if current == &id {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    Ok(())
}

#[tauri::command]
fn open_models_folder(app: AppHandle) -> Result<(), String> {
    let directory = models_path(&app)?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn history_entries(history: State<'_, Mutex<History>>) -> Result<Vec<Entry>, String> {
    history.lock().map_err(|error| error.to_string())?.entries()
}

#[tauri::command]
fn delete_history(id: u64, history: State<'_, Mutex<History>>) -> Result<(), String> {
    history
        .lock()
        .map_err(|error| error.to_string())?
        .remove(id)
}

#[tauri::command]
fn cloud_credential_status(provider: String) -> Result<bool, String> {
    Ok(credential_store::read(credential_store::cloud_target(&provider)?)?.is_some())
}

#[tauri::command]
fn update_cloud_credential(
    provider: String,
    secret: Option<String>,
    session: State<'_, SessionState>,
) -> Result<(), String> {
    let session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Cannot change credentials while dictating or exiting".to_owned());
    }

    let target = credential_store::cloud_target(&provider)?;
    match secret {
        Some(secret) => credential_store::write(target, &secret),
        None => credential_store::delete(target),
    }
}

#[tauri::command]
fn remote_credential_status(session: State<'_, SessionState>) -> Result<bool, String> {
    let session = session.lock().map_err(|error| error.to_string())?;
    if session.selected_endpoint.is_empty() {
        return Ok(false);
    }

    let target = credential_store::remote_target(&session.selected_endpoint)?;
    Ok(credential_store::read(&target)?.is_some())
}

#[tauri::command]
fn update_remote_credential(
    secret: Option<String>,
    session: State<'_, SessionState>,
) -> Result<(), String> {
    let session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Cannot change credentials while dictating or exiting".to_owned());
    }

    let target = credential_store::remote_target(&session.selected_endpoint)?;
    match secret {
        Some(secret) => credential_store::write(&target, &secret),
        None => credential_store::delete(&target),
    }
}

fn begin_recording(
    index: usize,
    model_path: Option<String>,
    provider: String,
    endpoint: String,
    remote_model: String,
    api_key_env: Option<String>,
    session: &SessionState,
    app: &AppHandle,
    destination: Option<Destination>,
    verifier: Option<&Arc<Mutex<Verifier>>>,
) -> Result<u64, String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    if session.exiting || session.coordinator.phase() != Phase::Idle {
        return Err("Dictation is already active or exiting".to_owned());
    }

    let artifact = PathBuf::from(model_path.unwrap_or_default());
    #[cfg(all(debug_assertions, feature = "input-test"))]
    let artifact = if input_test_mode() {
        PathBuf::from("test-model.bin")
    } else {
        artifact
    };
    if provider == "local" && artifact.as_os_str().is_empty() {
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
    let speech = match provider.as_str() {
        "local" => SpeechConfig::Local {
            adapter: "whisper.cpp".to_owned(),
            model_id,
            artifact,
        },
        "openai-compatible" if !endpoint.trim().is_empty() && !remote_model.trim().is_empty() => {
            SpeechConfig::OpenAiCompatible {
                model_id: remote_model,
                endpoint,
                api_key_env,
            }
        }
        "openai-compatible" => return Err("Enter a transcription URL and model".to_owned()),
        _ => return Err("Invalid speech provider".to_owned()),
    };
    let settings = Settings {
        microphone_index: index,
        speech,
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
    session.history_meta = None;
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
    let (id, capture, cancelled) = {
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
        (id, capture, Arc::clone(&session.cancelled))
    };
    hide_indicator(&app);
    let recording = capture.stop();
    let normalized = recording
        .as_ref()
        .ok()
        .filter(|recording| !recording.limited)
        .map(audio::Recording::normalized_16k);
    let (request, normalized) = {
        let mut session = session.lock().map_err(|error| error.to_string())?;
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
        if recording.limited {
            session.coordinator.cancel(id);
            session.active_id = None;
            return Err("Recording exceeded 30 seconds; try a shorter utterance".to_owned());
        }

        let normalized = normalized.unwrap();
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
        let (engine, model) = match &request.settings.speech {
            SpeechConfig::Local { model_id, .. } => ("On-device", model_id.clone()),
            SpeechConfig::OpenAiCompatible { model_id, .. } => {
                ("OpenAI-compatible", model_id.clone())
            }
        };
        session.history_meta = Some(HistoryMeta {
            duration_seconds: recording.samples.len() as f32 / recording.sample_rate as f32,
            engine: engine.to_owned(),
            model,
        });
        (request, normalized)
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
                session.history_meta = None;
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

#[tauri::command]
fn trial_focus(
    enabled: bool,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let mut session = session.lock().map_err(|error| error.to_string())?;
    session.trial_focused =
        enabled && onboarding_path(&app)?.is_file() && platform_input::app_is_foreground();
    Ok(())
}

#[tauri::command]
fn trial_start(session: State<'_, SessionState>, app: AppHandle) -> Result<u64, String> {
    if !onboarding_path(&app)?.is_file() {
        return Err("Onboarding is not active".to_owned());
    }

    let (index, model_path, provider, endpoint, remote_model, api_key_env) = {
        let session = session.lock().map_err(|error| error.to_string())?;
        (
            session.selected_index,
            session.selected_model_path.clone(),
            session.selected_provider.clone(),
            session.selected_endpoint.clone(),
            session.selected_remote_model.clone(),
            session.selected_api_key_env.clone(),
        )
    };
    let id = begin_recording(
        index,
        model_path,
        provider,
        endpoint,
        remote_model,
        api_key_env,
        session.inner(),
        &app,
        None,
        None,
    )?;
    session.lock().map_err(|error| error.to_string())?.trial_id = Some(id);
    Ok(id)
}

#[tauri::command]
async fn trial_end(
    id: u64,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<String, String> {
    if session.lock().map_err(|error| error.to_string())?.trial_id != Some(id) {
        return Err("No practice recording is active".to_owned());
    }

    let result = stop_recording(session.clone(), app, id).await;
    let mut session = session.lock().map_err(|error| error.to_string())?;
    session.trial_id = None;
    let text = result?;
    let result = session.coordinator.begin_insert(id, true);
    session.active_id = None;
    session.target = None;
    session.history_meta = None;
    match result {
        Decision::Insert { text, .. } => Ok(text),
        Decision::None => Err(text),
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
    session.trial_id = None;
    session.history_meta = None;
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
        self.trial_id = None;
        self.target = None;
        self.history_meta = None;
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
    let (decision, history_meta) = {
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
        (decision, session.history_meta.take())
    };
    let (inserted, entry) = match decision {
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
            drop(session);
            (submitted, submitted.then(|| (destination.app_name(), text)))
        }
        _ => (false, None),
    };

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
            Err(error) => {
                let _ = app.emit_to(
                    "main",
                    "dictation-preview",
                    format!("Could not save history: {error}"),
                );
            }
        }
    }

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
            history_meta: None,
            coordinator: Coordinator::default(),
            active_id: None,
            trial_id: None,
            trial_focused: false,
            target: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            selected_index: 0,
            selected_model_path: None,
            selected_provider: "local".to_owned(),
            selected_endpoint: String::new(),
            selected_remote_model: String::new(),
            selected_api_key_env: None,
            shortcut: "Ctrl+Space".to_owned(),
            toggle_shortcut: "Ctrl+Shift+Space".to_owned(),
            exiting: false,
            #[cfg(all(debug_assertions, feature = "input-test"))]
            test_audio: audio::Fixture::Speech,
        })))
        .manage(Arc::new(Mutex::new(Verifier::start())))
        .manage(Arc::new(Mutex::new(None::<(String, Arc<AtomicBool>)>)))
        .invoke_handler(tauri::generate_handler![
            microphones,
            onboarding_needed,
            onboarding_permissions,
            complete_onboarding,
            trial_start,
            trial_end,
            trial_focus,
            current_settings,
            configure,
            begin_key_capture,
            cancel_key_capture,
            complete_key_capture,
            remote_credential_status,
            update_remote_credential,
            cloud_credential_status,
            update_cloud_credential,
            history_entries,
            delete_history,
            model_catalog,
            download_model,
            cancel_model_download,
            open_models_folder
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
            let history_path = app.path().app_data_dir()?.join("history.json");
            app.manage(Mutex::new(History::open(history_path).map_err(std::io::Error::other)?));
            let history_handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                if history_handle.state::<SessionState>().lock().is_ok_and(|session| session.exiting) {
                    break;
                }

                let result = history_handle.state::<Mutex<History>>().lock()
                    .map_err(|error| error.to_string())
                    .and_then(|mut history| history.expire());
                if result == Ok(true) {
                    let _ = history_handle.emit_to("main", "history-updated", ());
                }
            });
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
                session.selected_provider = saved.provider;
                session.selected_endpoint = saved.endpoint;
                session.selected_remote_model = saved.remote_model;
                session.selected_api_key_env = saved.api_key_env;
                session.shortcut = saved.shortcut;
                session.toggle_shortcut = saved.toggle_shortcut;
            }

            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test_mode() {
                app.state::<SessionState>().lock().unwrap().shortcut = "F8".to_owned();
                start_input_test(app.handle().clone());
            }

            let (shortcut, toggle_shortcut) = {
                let session = app.state::<SessionState>();
                let session = session.lock().unwrap();
                (session.shortcut.clone(), session.toggle_shortcut.clone())
            };
            let (hook, events) = hotkey::start(&shortcut, &toggle_shortcut).map_err(std::io::Error::other)?;
            app.manage(Mutex::new(Some(hook)));
            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test_mode() {
                println!("YAP_TEST hotkey_ready");
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for event in events {
                    match event {
                        hotkey::Event::Pressed(window) => {
                            let same_foreground = window != 0 && platform_input::foreground_window_handle() == window;
                            let practicing = same_foreground && handle.state::<SessionState>().lock().is_ok_and(|session| session.trial_focused)
                                && platform_input::app_is_foreground();
                            if practicing {
                                match trial_start(handle.state::<SessionState>(), handle.clone()) {
                                    Ok(_) => { let _ = handle.emit_to("main", "onboarding-listening", ()); }
                                    Err(error) => {
                                        hotkey::finish();
                                        let _ = handle.emit_to("main", "dictation-preview", error);
                                    }
                                }

                                continue;
                            }

                            let destination = if same_foreground {
                                platform_input::focused_destination()
                            } else {
                                None
                            };
                            let (index, model_path, provider, endpoint, remote_model, api_key_env) = {
                                let state = handle.state::<SessionState>();
                                let session = state.lock().unwrap();
                                (
                                    session.selected_index,
                                    session.selected_model_path.clone(),
                                    session.selected_provider.clone(),
                                    session.selected_endpoint.clone(),
                                    session.selected_remote_model.clone(),
                                    session.selected_api_key_env.clone(),
                                )
                            };
                            let verifier =
                                Arc::clone(handle.state::<Arc<Mutex<Verifier>>>().inner());
                            if let Err(error) = begin_recording(
                                index,
                                model_path,
                                provider,
                                endpoint,
                                remote_model,
                                api_key_env,
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
                            let (id, cancelled, trial_id) = {
                                let state = handle.state::<SessionState>();
                                let session = state.lock().unwrap();
                                (session.active_id, Arc::clone(&session.cancelled), session.trial_id)
                            };
                            if let Some(id) = id {
                                let handle = handle.clone();
                                if trial_id == Some(id) {
                                    tauri::async_runtime::spawn(async move {
                                        match trial_end(id, handle.state::<SessionState>(), handle.clone()).await {
                                            Ok(text) => { let _ = handle.emit_to("main", "onboarding-transcript", text); }
                                            Err(error) => { let _ = handle.emit_to("main", "dictation-preview", error); }
                                        }
                                        hotkey::finish();
                                    });
                                    continue;
                                }

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
            if window.label() == "main" && matches!(event, WindowEvent::Focused(false)) {
                if let Ok(mut session) = window.state::<SessionState>().lock() {
                    session.trial_focused = false;
                }
            }

            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = hotkey::cancel_capture();
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
            ..SavedSettings::default()
        };
        let text = saved.serialize().unwrap();
        assert!(text.starts_with("Yap settings v4\n"));
        assert!(!text.contains("copy_only"));
        assert!(!text.contains("transcript"));
        let restored = SavedSettings::parse(&text).unwrap();
        assert_eq!(restored.microphone_index, 3);
        assert_eq!(restored.microphone_name, saved.microphone_name);
        assert_eq!(restored.model_path, saved.model_path);
        assert_eq!(restored.provider, "local");
        assert_eq!(restored.shortcut, "F9");
    }

    #[test]
    fn settings_view_uses_named_fields() {
        let view = SettingsView {
            index: 1,
            model_path: None,
            shortcut: "F8".into(),
            toggle_shortcut: "Ctrl+Shift+Space".into(),
            provider: "local".into(),
            endpoint: String::new(),
            remote_model: String::new(),
            api_key_env: None,
        };
        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value["index"], 1);
        assert_eq!(value["modelPath"], serde_json::Value::Null);
        assert_eq!(value["apiKeyEnv"], serde_json::Value::Null);
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
            assert_eq!(restored.provider, "local");
            assert_eq!(restored.shortcut, "F9");
            assert!(!restored.serialize().unwrap().contains("copy_only"));
        }
    }

    #[test]
    fn v2_settings_migrate_and_remote_settings_round_trip_without_credentials() {
        let old = "Yap settings v2\nshortcut=F8\nmicrophone_index=1\nmicrophone_name=Mic\nmodel_path=model.bin\n";
        let mut saved = SavedSettings::parse(old).unwrap();
        assert_eq!(saved.provider, "local");
        saved.provider = "openai-compatible".into();
        saved.endpoint = "https://api.openai.com/v1/audio/transcriptions".into();
        saved.remote_model = "whisper-1".into();
        saved.api_key_env = Some("OPENAI_API_KEY".into());
        let text = saved.serialize().unwrap();
        assert!(!text.contains("secret-key"));
        let restored = SavedSettings::parse(&text).unwrap();
        assert_eq!(restored.provider, saved.provider);
        assert_eq!(restored.endpoint, saved.endpoint);
        assert_eq!(restored.remote_model, saved.remote_model);
        assert_eq!(restored.api_key_env, saved.api_key_env);
        assert_eq!(restored.model_path.as_deref(), Some("model.bin"));
    }

    #[test]
    fn v3_settings_migrate_without_changing_the_hold_key() {
        let old = "Yap settings v3\nshortcut=F9\nmicrophone_index=0\nmicrophone_name=Mic\nmodel_path=\nprovider=local\nendpoint=\nremote_model=\napi_key_env=\n";
        let saved = SavedSettings::parse(old).unwrap();
        assert_eq!(saved.shortcut, "F9");
        assert_eq!(saved.toggle_shortcut, "Ctrl+Shift+Space");
        assert!(SavedSettings::parse(&saved.serialize().unwrap()).is_ok());
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
            history_meta: None,
            coordinator: Coordinator::default(),
            active_id: None,
            trial_id: None,
            trial_focused: false,
            target: None,
            cancelled: Arc::clone(&cancelled),
            selected_index: 0,
            selected_model_path: None,
            selected_provider: "local".to_owned(),
            selected_endpoint: String::new(),
            selected_remote_model: String::new(),
            selected_api_key_env: None,
            shortcut: "F8".to_owned(),
            toggle_shortcut: "Ctrl+Shift+Space".to_owned(),
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
