//! Records inserted dictations in History.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use yap_app::coordinator::JobInfo;
use yap_app::history::History;

use super::notify;

pub(super) fn save(app: &AppHandle, app_name: String, text: String, info: JobInfo) {
    let result = app
        .state::<Mutex<History>>()
        .lock()
        .map_err(|error| error.to_string())
        .and_then(|mut history| {
            history.add(
                app_name,
                info.duration_seconds,
                text,
                info.engine,
                info.model,
            )
        });
    match result {
        Ok(()) => {
            let _ = app.emit_to("main", "history-updated", ());
        }
        Err(error) => notify(app, format!("Could not save history: {error}")),
    }
}
