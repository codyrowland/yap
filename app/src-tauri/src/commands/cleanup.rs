use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::models::ModelProgress;
use crate::config;
use crate::state::{ModelDownloadState, SharedSettings, lock};

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
    if !cfg!(windows) {
        return Err("Clean-up is not available on this system yet".to_owned());
    }

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
