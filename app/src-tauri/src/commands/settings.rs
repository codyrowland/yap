use serde::Serialize;
use tauri::{AppHandle, State};
use yap_app::settings::{LANGUAGES, Settings};
use yap_app::speech::cloud::{self, Provider};

use super::models::{LocalModel, local_models};
use crate::config;
use crate::state::{SessionState, SharedSettings};
use crate::{audio, talk_key_state};

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
