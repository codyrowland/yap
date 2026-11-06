//! Commands used by the Settings, History and tray windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::coordinator::Phase;
use yap_app::history::{Entry, History};
use yap_app::models::Family;
use yap_app::settings::{Engine, LANGUAGES, Settings};
use yap_app::speech::cloud::{self, Provider};
use yap_app::speech::openai_compatible::OpenAiCompatible;
use yap_app::{credential_store, models};

use crate::config;
use crate::state::{ModelDownloadState, SessionState, SharedSettings, lock};
use crate::{audio, talk_key_state};

// ---------- Settings ----------

#[tauri::command]
pub fn microphones() -> Result<Vec<String>, String> {
    audio::microphones()
}

#[tauri::command]
pub fn current_settings(settings: State<'_, SharedSettings>) -> Result<Settings, String> {
    Ok(Settings::clone(&settings.get()))
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
    models: Vec<LocalModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    id: &'static str,
    family: Family,
    label: &'static str,
    note: &'static str,
    bytes: u64,
    /// Set once downloaded; this is what `localModelPath` stores.
    path: Option<String>,
    /// False when this build was compiled without the family's runtime.
    available: bool,
}

fn family_available(family: Family) -> bool {
    match family {
        Family::Whisper => cfg!(feature = "local-whisper"),
        Family::Parakeet | Family::Moonshine | Family::Nemotron => cfg!(feature = "local-sherpa"),
    }
}

fn local_models(app: &AppHandle) -> Result<Vec<LocalModel>, String> {
    let directory = config::models_path(app)?;
    Ok(models::MODELS
        .iter()
        .map(|model| LocalModel {
            id: model.id,
            family: model.family,
            label: model.label,
            note: model.note,
            bytes: model.bytes,
            path: models::installed(model, &directory).then(|| {
                models::path(model, &directory)
                    .to_string_lossy()
                    .into_owned()
            }),
            available: family_available(model.family),
        })
        .collect())
}

/// Everything the Settings window lists: providers, languages and models.
#[tauri::command]
pub fn catalog(app: AppHandle) -> Result<Catalog, String> {
    Ok(Catalog {
        providers: cloud::PROVIDERS,
        languages: LANGUAGES.to_vec(),
        models: local_models(&app)?,
    })
}

#[tauri::command]
pub fn begin_key_capture(kind: String) -> Result<(), String> {
    crate::logfile::write(&format!("key capture started ({kind})"));
    talk_key_state::begin_capture(&kind)
}

#[tauri::command]
pub fn cancel_key_capture() -> Result<(), String> {
    crate::logfile::write("key capture cancelled");
    talk_key_state::cancel_capture()
}

#[tauri::command]
pub fn complete_key_capture(kind: String, shortcut: String) -> Result<(), String> {
    let result = talk_key_state::complete_capture(&kind, &shortcut);
    crate::logfile::write(&format!(
        "key capture saved ({kind}: {shortcut}): {result:?}"
    ));
    result
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
    settings: State<'_, SharedSettings>,
) -> Result<bool, String> {
    let target = credential_target(&kind, &provider, &settings.get())?;
    Ok(credential_store::read(&target)?.is_some())
}

/// Saves (`Some`) or removes (`None`) a key in Windows Credential Manager.
#[tauri::command]
pub fn update_credential(
    kind: String,
    provider: String,
    secret: Option<String>,
    session: State<'_, SessionState>,
    settings: State<'_, SharedSettings>,
) -> Result<(), String> {
    let session = lock(&session)?;
    if !session.idle() {
        return Err("Cannot change credentials while dictating or exiting".to_owned());
    }

    let target = credential_target(&kind, &provider, &settings.get())?;
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
    settings: State<'_, SharedSettings>,
) -> Result<u64, String> {
    let settings = settings.get();
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
    let model = models::model(&id).ok_or("Unknown model")?;
    if !family_available(model.family) {
        return Err("This model's engine is not included in this build".to_owned());
    }

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
    let path = models::path(model, &directory)
        .to_string_lossy()
        .into_owned();
    if !lock(&session)?.idle() {
        return Err("Cannot delete a model while dictating".to_owned());
    }

    let settings = Settings::clone(&app.state::<SharedSettings>().get());
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
    // Release the model if it is still loaded, so its files can be deleted.
    crate::dictation::unload();
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

// ---------- Clean-up ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupEntry {
    #[serde(flatten)]
    model: &'static yap_app::cleanup::CleanupModel,
    installed: bool,
    /// Bytes a download would fetch now (includes the runtime if missing).
    download_bytes: u64,
}

#[derive(Serialize)]
pub struct CleanupCatalog {
    models: Vec<CleanupEntry>,
    status: crate::assistant::Status,
}

#[tauri::command]
pub fn cleanup_catalog(app: AppHandle) -> Result<CleanupCatalog, String> {
    let directory = config::models_path(&app)?;
    Ok(CleanupCatalog {
        models: yap_app::cleanup::MODELS
            .iter()
            .map(|model| CleanupEntry {
                model,
                installed: yap_app::cleanup::installed(model, &directory),
                download_bytes: yap_app::cleanup::download_size(model, &directory),
            })
            .collect(),
        status: crate::assistant::status(),
    })
}

/// Downloads a clean-up model (and the llama.cpp runtime the first time).
/// Progress uses the model events with the id `cleanup:<id>`.
#[tauri::command]
pub fn download_cleanup(
    id: String,
    app: AppHandle,
    downloads: State<'_, ModelDownloadState>,
) -> Result<(), String> {
    let model = yap_app::cleanup::model(&id).ok_or("Unknown clean-up model")?;
    let directory = config::models_path(&app)?;
    let event_id = format!("cleanup:{id}");
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = lock(&downloads)?;
        if active.is_some() {
            return Err("A model download is already in progress".to_owned());
        }
        *active = Some((event_id.clone(), Arc::clone(&cancelled)));
    }
    let total = yap_app::cleanup::download_size(model, &directory).max(1);
    let downloads = Arc::clone(downloads.inner());
    std::thread::spawn(move || {
        let mut last_percent = 0;
        let result = yap_app::cleanup::download(model, &directory, &cancelled, |downloaded| {
            let percent = downloaded.saturating_mul(100) / total;
            if percent > last_percent {
                last_percent = percent;
                let _ = app.emit(
                    "model-download-progress",
                    ModelProgress {
                        id: event_id.clone(),
                        downloaded,
                        total,
                        state: "downloading",
                        error: None,
                    },
                );
            }
        });
        let (state, error) = match result {
            Ok(()) => ("ready", None),
            Err(error) if error == "Cancelled" => ("cancelled", None),
            Err(error) => ("error", Some(error)),
        };
        if let Ok(mut active) = downloads.lock() {
            *active = None;
        }
        let _ = app.emit(
            "model-download-progress",
            ModelProgress {
                id: event_id,
                downloaded: 0,
                total,
                state,
                error,
            },
        );
    });
    Ok(())
}

#[tauri::command]
pub fn delete_cleanup(
    id: String,
    app: AppHandle,
    settings: State<'_, SharedSettings>,
) -> Result<(), String> {
    let model = yap_app::cleanup::model(&id).ok_or("Unknown clean-up model")?;
    let settings = settings.get();
    if settings.cleanup_enabled && settings.cleanup_model == model.id {
        return Err(
            "Turn off clean-up or pick the other model before deleting this one".to_owned(),
        );
    }

    yap_app::cleanup::remove(model, &config::models_path(&app)?)
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
    let local_detail = settings.local_model_path.as_deref().map(|path| {
        let path = std::path::Path::new(path);
        models::for_path(path).map_or_else(
            || {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            },
            models::display_name,
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
pub fn tray_state(
    session: State<'_, SessionState>,
    settings: State<'_, SharedSettings>,
) -> Result<TrayState, String> {
    let phase = lock(&session)?.coordinator.phase();
    let settings = settings.get();
    let status = match phase {
        Phase::Idle => "Ready",
        Phase::Recording => "Listening",
        Phase::Transcribing | Phase::Ready | Phase::Inserting => "Transcribing",
    };
    Ok(TrayState {
        status,
        hold_key: settings.hold_key.clone(),
        toggle_key: settings.toggle_key.clone(),
        engine: settings.engine,
        engines: engine_choices(&settings),
    })
}

#[tauri::command]
pub fn set_engine(
    engine: Engine,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let settings = Settings::clone(&app.state::<SharedSettings>().get());
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
