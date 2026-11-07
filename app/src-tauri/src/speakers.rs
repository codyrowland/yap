use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::platform;

const MARKER: &str = "speakers_muted";

pub struct Muted {
    device: String,
    marker: Option<PathBuf>,
}

impl Drop for Muted {
    fn drop(&mut self) {
        platform::unmute_speakers(&self.device);
        if let Some(marker) = &self.marker {
            let _ = std::fs::remove_file(marker);
        }
    }
}

fn marker(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|directory| directory.join(MARKER))
}

pub fn mute(app: &AppHandle) -> Option<Muted> {
    let device = platform::mute_speakers()?;
    let marker = marker(app);
    if let Some(marker) = &marker {
        let _ = std::fs::write(marker, &device);
    }
    Some(Muted { device, marker })
}

pub fn restore_after_crash(app: &AppHandle) {
    let Some(marker) = marker(app) else {
        return;
    };
    if let Ok(device) = std::fs::read_to_string(&marker) {
        platform::unmute_speakers(&device);
        let _ = std::fs::remove_file(&marker);
        crate::logfile::write("speakers unmuted after an unclean exit");
    }
}
