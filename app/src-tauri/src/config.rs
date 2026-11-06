//! Where settings live on disk, and applying a settings change.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager};
use yap_app::settings::Settings;

use crate::state::{SessionState, SharedSettings, lock};
use crate::{audio, autostart, hotkey};

const SETTINGS_FILE: &str = "settings.json";
const LEGACY_SETTINGS_FILE: &str = "settings.txt";

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|error| error.to_string())
}

pub fn onboarding_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("onboarding_pending"))
}

pub fn models_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("models"))
        .map_err(|error| error.to_string())
}

/// True once settings have been saved by any version of Yap.
pub fn settings_exist(app: &AppHandle) -> Result<bool, String> {
    let directory = config_dir(app)?;
    Ok(directory.join(SETTINGS_FILE).exists() || directory.join(LEGACY_SETTINGS_FILE).exists())
}

fn read_settings(directory: &Path) -> Result<Settings, String> {
    match std::fs::read_to_string(directory.join(SETTINGS_FILE)) {
        Ok(text) => return Settings::from_json(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    match std::fs::read_to_string(directory.join(LEGACY_SETTINGS_FILE)) {
        Ok(text) => {
            let settings = Settings::from_legacy(&text)?;
            hotkey::validate_shortcuts(&settings.hold_key, &settings.toggle_key)?;
            write_settings(directory, &settings)?;
            // The old file is left in place (harmless, and lets an older build
            // still start); settings.json takes precedence from now on.
            Ok(settings)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.to_string()),
    }
}

/// Writes through a temporary file so a crash never leaves half a file.
fn write_settings(directory: &Path, settings: &Settings) -> Result<(), String> {
    let text = settings.to_json()?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let temporary = directory.join(format!("{SETTINGS_FILE}.tmp"));
    std::fs::write(&temporary, text).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, directory.join(SETTINGS_FILE)).map_err(|error| error.to_string())
}

/// Loads settings (importing the legacy file if needed) and re-resolves the
/// microphone by name, since device order can change between runs.
pub fn load(app: &AppHandle) -> Result<Settings, String> {
    let mut settings = read_settings(&config_dir(app)?)?;
    hotkey::validate_shortcuts(&settings.hold_key, &settings.toggle_key)?;
    let names = audio::microphones().unwrap_or_default();
    let index = settings
        .microphone_name
        .as_ref()
        .map_or(settings.microphone_index, |name| {
            if names.get(settings.microphone_index) == Some(name) {
                settings.microphone_index
            } else {
                names
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or(0)
            }
        });
    settings.microphone_index = index.min(names.len().saturating_sub(1));
    Ok(settings)
}

/// Validates, persists and activates new settings. Rejected while dictating;
/// holding the session lock keeps a dictation from starting meanwhile.
pub fn apply(
    app: &AppHandle,
    session: &SessionState,
    mut next: Settings,
) -> Result<Settings, String> {
    let session = lock(session)?;
    if !session.idle() {
        return Err("Cannot change settings while dictating or exiting".to_owned());
    }

    next.version = yap_app::settings::CURRENT_VERSION;
    next.validate()?;
    if next.cleanup_enabled {
        let model = yap_app::cleanup::model(&next.cleanup_model).ok_or("Unknown clean-up model")?;
        if !yap_app::cleanup::installed(model, &models_path(app)?) {
            return Err(format!(
                "Download {} before turning on clean-up",
                model.label
            ));
        }
    }
    let shared = app.state::<SharedSettings>();
    let current = shared.get();
    // With no microphone plugged in, other settings can still change; the
    // saved microphone name is kept so it is found again when reconnected.
    let names = audio::microphones().unwrap_or_default();
    if !names.is_empty() {
        next.microphone_name = Some(
            names
                .get(next.microphone_index)
                .cloned()
                .ok_or("Microphone is no longer available")?,
        );
    } else if next.microphone_index != current.microphone_index {
        return Err("No microphone is connected".to_owned());
    }
    let directory = config_dir(app)?;
    hotkey::update_shortcut(&next.hold_key, &next.toggle_key, || {
        write_settings(&directory, &next)
    })?;
    if next.open_at_login != current.open_at_login
        && let Err(error) = autostart::set(next.open_at_login)
    {
        // Keep the rest of the change; report that this part failed.
        next.open_at_login = current.open_at_login;
        let _ = write_settings(&directory, &next);
        shared.set(next);
        drop(session);
        let _ = app.emit("settings-changed", ());
        return Err(error);
    }

    crate::logfile::write(&format!(
        "settings saved: engine={:?} hold={} toggle={} clean-up={}",
        next.engine, next.hold_key, next.toggle_key, next.cleanup_enabled
    ));
    shared.set(next.clone());
    drop(session);
    crate::dictation::preload(&next);
    crate::assistant::sync(app, &next);
    let _ = app.emit("settings-changed", ());
    Ok(next)
}
