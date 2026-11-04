//! First-run setup (design 2a): permission checks, engine setup, talk keys
//! and a practice recording inside Yap.

use tauri::{AppHandle, State};
use yap_app::coordinator::Decision;
use yap_app::settings::Engine;

use crate::config;
use crate::dictation::{begin_recording, speech_config, stop_recording};
use crate::state::{SessionState, lock};
use crate::{audio, platform_input};

#[tauri::command]
pub fn onboarding_needed(app: AppHandle) -> Result<bool, String> {
    let path = config::onboarding_path(&app)?;
    if path.exists() {
        return Ok(true);
    }
    if config::settings_exist(&app)? {
        return Ok(false);
    }

    std::fs::create_dir_all(path.parent().ok_or("Invalid onboarding path")?)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, []).map_err(|error| error.to_string())?;
    Ok(true)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
    microphone: bool,
    microphone_error: Option<String>,
    accessibility: bool,
}

#[tauri::command]
pub fn onboarding_permissions(index: usize) -> PermissionStatus {
    let microphone = audio::Capture::start(index).map(drop);
    PermissionStatus {
        microphone: microphone.is_ok(),
        microphone_error: microphone.err(),
        accessibility: platform_input::accessibility_ready(),
    }
}

#[tauri::command]
pub fn complete_onboarding(session: State<'_, SessionState>, app: AppHandle) -> Result<(), String> {
    let mut session = lock(&session)?;
    if !session.idle() {
        return Err("Finish the recording before completing setup".to_owned());
    }

    if session.settings.engine == Engine::Local
        && !session
            .settings
            .local_model_path
            .as_ref()
            .is_some_and(|path| std::path::Path::new(path).is_file())
    {
        return Err("Select a downloaded model before finishing".to_owned());
    }
    session.settings.validate()?;
    speech_config(&session.settings)?;

    session.trial_focused = false;
    drop(session);
    match std::fs::remove_file(config::onboarding_path(&app)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
pub fn trial_focus(
    enabled: bool,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let mut session = lock(&session)?;
    session.trial_focused =
        enabled && config::onboarding_path(&app)?.is_file() && platform_input::app_is_foreground();
    Ok(())
}

#[tauri::command]
pub fn trial_start(session: State<'_, SessionState>, app: AppHandle) -> Result<u64, String> {
    if !config::onboarding_path(&app)?.is_file() {
        return Err("Onboarding is not active".to_owned());
    }

    let id = begin_recording(session.inner(), &app, None, None)?;
    lock(&session)?.trial_id = Some(id);
    Ok(id)
}

#[tauri::command]
pub async fn trial_end(
    id: u64,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<String, String> {
    if lock(&session)?.trial_id != Some(id) {
        return Err("No practice recording is active".to_owned());
    }

    let result = stop_recording(session.clone(), app.clone(), id).await;
    crate::overlay::hide(&app);
    let mut session = lock(&session)?;
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
