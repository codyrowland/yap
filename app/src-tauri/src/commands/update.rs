use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::logfile;
use crate::state::SharedSettings;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// An update downloaded in the background, installed when Yap quits.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<(Update, Vec<u8>)>>);

async fn find_update(app: &AppHandle) -> Result<Option<Update>, String> {
    app.updater()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| format!("Couldn't check for updates: {error}"))
}

fn take_pending(app: &AppHandle) -> Option<(Update, Vec<u8>)> {
    app.state::<PendingUpdate>().0.lock().ok()?.take()
}

/// Returns the newer version if one is available.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<String>, String> {
    Ok(find_update(&app).await?.map(|update| update.version))
}

/// The version downloaded in the background and waiting to install.
#[tauri::command]
pub fn pending_update(app: AppHandle) -> Option<String> {
    let pending = app.state::<PendingUpdate>();
    let pending = pending.0.lock().ok()?;
    pending.as_ref().map(|(update, _)| update.version.clone())
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    match take_pending(&app) {
        Some((update, bytes)) => update
            .install(bytes)
            .map_err(|error| format!("Couldn't install the update: {error}"))?,
        None => find_update(&app)
            .await?
            .ok_or("Yap is up to date")?
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|error| format!("Couldn't install the update: {error}"))?,
    }
    app.restart();
}

async fn download_if_newer(app: &AppHandle) -> Result<(), String> {
    let Some(update) = find_update(app).await? else {
        return Ok(());
    };
    if pending_update(app.clone()).as_deref() == Some(update.version.as_str()) {
        return Ok(());
    }

    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|error| format!("Couldn't download the update: {error}"))?;
    let version = update.version.clone();
    if let Ok(mut pending) = app.state::<PendingUpdate>().0.lock() {
        *pending = Some((update, bytes));
    }

    logfile::write(&format!("update {version} downloaded"));
    let _ = app.emit("update-ready", version);
    Ok(())
}

/// Checks shortly after start and then once a day while automatic updates
/// are on.
pub fn start_auto_update(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            if app.state::<SharedSettings>().get().auto_update
                && let Err(error) = tauri::async_runtime::block_on(download_if_newer(&app))
            {
                logfile::write(&error);
            }

            std::thread::sleep(CHECK_INTERVAL);
        }
    });
}

/// Installs a downloaded update on quit. On Windows this starts the installer
/// and exits the process.
pub fn install_pending_on_quit(app: &AppHandle) {
    if !app.state::<SharedSettings>().get().auto_update {
        return;
    }

    if let Some((update, bytes)) = take_pending(app)
        && let Err(error) = update.restart_after_install(false).install(bytes)
    {
        logfile::write(&format!("couldn't install the update: {error}"));
    }
}
