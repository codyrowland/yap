//! Commands used by the Settings, History and tray windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::coordinator::Phase;
use yap_app::history::{Entry, History};
use yap_app::settings::{Engine, LANGUAGES, Settings};
use yap_app::speech::cloud::{self, Provider};
use yap_app::speech::openai_compatible::OpenAiCompatible;
use yap_app::{credential_store, models};

use crate::config;
use crate::state::{ModelDownloadState, SessionState, lock};
use crate::{audio, hotkey};

// ---------- Settings ----------

#[tauri::command]
pub fn microphones() -> Result<Vec<String>, String> {
    audio::microphones()
}

#[tauri::command]
pub fn current_settings(session: State<'_, SessionState>) -> Result<Settings, String> {
    Ok(lock(&session)?.settings.clone())
}

/// Saves the full settings object. The UI always sends every field, so a
/// change on one tab never resets another.
#[tauri::command]
pub fn save_settings(
    settings: Settings,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<Settings, String> {
    config::apply(&app, session.inner(), settings)
}

#[derive(Serialize)]
pub struct Catalog {
    providers: &'static [Provider],
    languages: Vec<(&'static str, &'static str)>,
    whisper: Vec<WhisperModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModel {
    id: &'static str,
    bytes: u64,
    path: Option<String>,
}

fn whisper_models(app: &AppHandle) -> Result<Vec<WhisperModel>, String> {
    let directory = config::models_path(app)?;
    Ok(models::WHISPER_MODELS
        .iter()
        .map(|model| WhisperModel {
            id: model.id,
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

/// Everything the Settings window lists: providers, languages and models.
#[tauri::command]
pub fn catalog(app: AppHandle) -> Result<Catalog, String> {
    Ok(Catalog {
        providers: cloud::PROVIDERS,
        languages: LANGUAGES.to_vec(),
        whisper: whisper_models(&app)?,
    })
}

#[tauri::command]
pub fn begin_key_capture(kind: String) -> Result<(), String> {
    hotkey::begin_capture(&kind)
}

#[tauri::command]
pub fn cancel_key_capture() -> Result<(), String> {
    hotkey::cancel_capture()
}

#[tauri::command]
pub fn complete_key_capture(kind: String, shortcut: String) -> Result<(), String> {
    hotkey::complete_capture(&kind, &shortcut)
}

// ---------- Credentials ----------

fn credential_target(kind: &str, provider: &str, settings: &Settings) -> Result<String, String> {
    match kind {
        "cloud" => credential_store::cloud_target(provider, &settings.custom_base_url),
        "remote" => {
            if settings.remote_endpoint.is_empty() {
                return Err("Enter a server URL first".to_owned());
            }
            credential_store::remote_target(&settings.remote_endpoint)
        }
        _ => Err("Unknown credential".to_owned()),
    }
}

#[tauri::command]
pub fn credential_status(
    kind: String,
    provider: String,
    session: State<'_, SessionState>,
) -> Result<bool, String> {
    let target = credential_target(&kind, &provider, &lock(&session)?.settings)?;
    Ok(credential_store::read(&target)?.is_some())
}

/// Saves (`Some`) or removes (`None`) a key in Windows Credential Manager.
#[tauri::command]
pub fn update_credential(
    kind: String,
    provider: String,
    secret: Option<String>,
    session: State<'_, SessionState>,
) -> Result<(), String> {
    let session = lock(&session)?;
    if !session.idle() {
        return Err("Cannot change credentials while dictating or exiting".to_owned());
    }

    let target = credential_target(&kind, &provider, &session.settings)?;
    match secret.map(|secret| secret.trim().to_owned()) {
        Some(secret) if !secret.is_empty() => credential_store::write(&target, &secret),
        Some(_) => Err("Enter a key first".to_owned()),
        None => credential_store::delete(&target),
    }
}

/// Sends a second of silence to the configured cloud provider or server.
#[tauri::command]
pub async fn test_connection(
    kind: String,
    session: State<'_, SessionState>,
) -> Result<u64, String> {
    let settings = lock(&session)?.settings.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let provider = match kind.as_str() {
            "cloud" => OpenAiCompatible::cloud(
                settings.cloud_model(),
                &settings.cloud_provider,
                &settings.custom_base_url,
                None,
            )?,
            "remote" => {
                if settings.remote_endpoint.is_empty() || settings.remote_model.trim().is_empty() {
                    return Err("Enter a server URL and model".to_owned());
                }
                OpenAiCompatible::new(
                    settings.remote_model.clone(),
                    &settings.remote_endpoint,
                    settings.remote_api_key_env.as_deref(),
                    None,
                )?
            }
            _ => return Err("Unknown engine".to_owned()),
        };
        provider.check().map(|elapsed| elapsed.as_millis() as u64)
    })
    .await
    .map_err(|error| error.to_string())?
}

// ---------- On-device models ----------

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelProgress {
    id: String,
    downloaded: u64,
    total: u64,
    state: &'static str,
    error: Option<String>,
}

#[tauri::command]
pub fn download_model(
    id: String,
    app: AppHandle,
    downloads: State<'_, ModelDownloadState>,
) -> Result<(), String> {
    if !cfg!(feature = "local-whisper") {
        return Err("Local Whisper is unavailable in this build".to_owned());
    }

    let model = models::model(&id).ok_or("Unknown model")?;
    let directory = config::models_path(&app)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = lock(&downloads)?;
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
                let _ = app.emit(
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
        if let Ok(mut active) = downloads.lock() {
            *active = None;
        }
        let _ = app.emit(
            "model-download-progress",
            ModelProgress {
                id,
                downloaded: 0,
                total: model.bytes,
                state,
                error,
            },
        );
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_model_download(
    id: String,
    downloads: State<'_, ModelDownloadState>,
) -> Result<(), String> {
    if let Some((current, cancelled)) = lock(&downloads)?.as_ref()
        && current == &id
    {
        cancelled.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Deletes a downloaded model. If it was the selected model, the selection
/// is cleared (and dictation will ask for a model).
#[tauri::command]
pub fn delete_model(
    id: String,
    app: AppHandle,
    session: State<'_, SessionState>,
) -> Result<(), String> {
    let model = models::model(&id).ok_or("Unknown model")?;
    let directory = config::models_path(&app)?;
    let path = directory
        .join(model.filename)
        .to_string_lossy()
        .into_owned();
    let settings = {
        let session = lock(&session)?;
        if !session.idle() {
            return Err("Cannot delete a model while dictating".to_owned());
        }
        session.settings.clone()
    };
    if settings.local_model_path.as_deref() == Some(path.as_str()) {
        if settings.engine == Engine::Local {
            return Err("Choose another model before deleting the one in use".to_owned());
        }
        config::apply(
            &app,
            session.inner(),
            Settings {
                local_model_path: None,
                ..settings
            },
        )?;
    }
    models::remove(model, &directory)
}

#[tauri::command]
pub fn open_models_folder(app: AppHandle) -> Result<(), String> {
    let directory = config::models_path(&app)?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(drop)
        .map_err(|error| error.to_string())
}

// ---------- History ----------

#[tauri::command]
pub fn history_entries(history: State<'_, Mutex<History>>) -> Result<Vec<Entry>, String> {
    lock(&history)?.entries()
}

#[tauri::command]
pub fn delete_history(id: u64, history: State<'_, Mutex<History>>) -> Result<(), String> {
    lock(&history)?.remove(id)
}

// ---------- Tray popover ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineChoice {
    id: Engine,
    label: &'static str,
    detail: String,
    ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayState {
    status: &'static str,
    hold_key: String,
    toggle_key: String,
    engine: Engine,
    engines: Vec<EngineChoice>,
}

fn engine_choices(settings: &Settings) -> Vec<EngineChoice> {
    let local_detail = settings
        .local_model_path
        .as_deref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .map(|name| {
            let name = name.to_string_lossy();
            format!(
                "Whisper {}",
                name.trim_start_matches("ggml-").trim_end_matches(".bin")
            )
        });
    let provider = cloud::provider(&settings.cloud_provider);
    let remote_host = reqwest::Url::parse(&settings.remote_endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    let ready = |engine| {
        Settings {
            engine,
            ..settings.clone()
        }
        .validate()
        .is_ok()
    };
    vec![
        EngineChoice {
            id: Engine::Local,
            label: "On-device",
            ready: local_detail.is_some(),
            detail: local_detail.unwrap_or_else(|| "No model selected".to_owned()),
        },
        EngineChoice {
            id: Engine::Cloud,
            label: "Cloud API",
            ready: ready(Engine::Cloud),
            detail: provider.map_or_else(
                || "Not set up".to_owned(),
                |provider| format!("{} · {}", provider.label, settings.cloud_model()),
            ),
        },
        EngineChoice {
            id: Engine::Remote,
            label: "Self-hosted",
            ready: ready(Engine::Remote),
            detail: remote_host.unwrap_or_else(|| "Not set up".to_owned()),
        },
    ]
}

#[tauri::command]
pub fn tray_state(session: State<'_, SessionState>) -> Result<TrayState, String> {
    let session = lock(&session)?;
    let status = match session.coordinator.phase() {
        Phase::Idle => "Ready",
        Phase::Recording => "Listening",
        Phase::Transcribing | Phase::Ready | Phase::Inserting => "Transcribing",
    };
    Ok(TrayState {
        status,
        hold_key: session.settings.hold_key.clone(),
        toggle_key: session.settings.toggle_key.clone(),
        engine: session.settings.engine,
        engines: engine_choices(&session.settings),
    })
}

#[tauri::command]
pub fn set_engine(
    engine: Engine,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let settings = lock(&session)?.settings.clone();
    let label = match engine {
        Engine::Local => "an on-device model",
        Engine::Cloud => "a cloud provider",
        Engine::Remote => "a self-hosted server",
    };
    let next = Settings { engine, ..settings };
    if engine == Engine::Local && next.local_model_path.is_none() {
        return Err(format!("Set up {label} in Settings first"));
    }
    next.validate()
        .map_err(|_| format!("Set up {label} in Settings first"))?;
    config::apply(&app, session.inner(), next).map(drop)
}

/// Opens the main window on Settings or History.
#[tauri::command]
pub fn open_window(page: String, app: AppHandle) -> Result<(), String> {
    show_main(&app, &page)
}

pub fn show_main(app: &AppHandle, page: &str) -> Result<(), String> {
    let file = match page {
        "settings" => "index.html",
        "history" => "history.html",
        _ => return Err("Unknown page".to_owned()),
    };
    if let Some(tray) = app.get_webview_window(crate::tray::LABEL) {
        let _ = tray.hide();
    }
    let window = app
        .get_webview_window("main")
        .ok_or("Main window is missing")?;
    let onboarding = config::onboarding_path(app)?.is_file();
    if !onboarding {
        let current = window.url().map_err(|error| error.to_string())?;
        if !current.path().ends_with(file) {
            window
                .eval(format!("location.replace('{file}')"))
                .map_err(|error| error.to_string())?;
        }
    }
    let _ = window.unminimize();
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::shutdown(&app);
}
