//! First-run setup (design 2a): permission checks, engine setup and talk
//! keys. The "Try it" step uses normal dictation into its own text box.

use tauri::{AppHandle, Manager, State};
use yap_app::settings::Engine;

use crate::config;
use crate::state::{SessionState, SharedSettings, lock};
use crate::{audio, platform};

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
        accessibility: platform::accessibility_ready(),
    }
}

#[tauri::command]
pub fn complete_onboarding(session: State<'_, SessionState>, app: AppHandle) -> Result<(), String> {
    let result = finish_onboarding(&session, &app);
    match &result {
        Ok(()) => crate::logfile::write("onboarding completed"),
        Err(error) => crate::logfile::write(&format!("onboarding could not finish: {error}")),
    }
    result
}

fn finish_onboarding(session: &SessionState, app: &AppHandle) -> Result<(), String> {
    let session = lock(session)?;
    if !session.idle() {
        return Err("Finish the recording before completing setup".to_owned());
    }

    let settings = app.state::<SharedSettings>().get();
    if settings.engine == Engine::Local
        && !settings
            .local_model_path
            .as_ref()
            .is_some_and(|path| yap_app::models::path_installed(std::path::Path::new(path)))
    {
        return Err("Select a downloaded model before finishing".to_owned());
    }
    settings.validate()?;
    settings.speech_config()?;

    drop(session);
    match std::fs::remove_file(config::onboarding_path(app)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
