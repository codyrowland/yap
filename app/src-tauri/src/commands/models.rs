use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use yap_app::models::{self, Family};
use yap_app::settings::{Engine, Settings};

use crate::config;
use crate::state::{ModelDownloadState, SessionState, SharedSettings, lock};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LocalModel {
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

pub(super) fn local_models(app: &AppHandle) -> Result<Vec<LocalModel>, String> {
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub state: &'static str,
    pub error: Option<String>,
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
    crate::dictation::unload(&app);
    models::remove(model, &directory)
}

#[tauri::command]
pub fn open_models_folder(app: AppHandle) -> Result<(), String> {
    let directory = config::models_path(&app)?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    std::process::Command::new(crate::platform::FILE_MANAGER)
        .arg(directory)
        .spawn()
        .map(drop)
        .map_err(|error| error.to_string())
}
