//! Shared app state: the dictation session and the user's settings.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use yap_app::coordinator::Coordinator;
use yap_app::settings::Settings;

use crate::audio;
use crate::platform::Destination;
use crate::speakers::Muted;

/// Where a dictation's text goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A field in another app, reached through the OS.
    App(Destination),
    /// Yap's own window, which inserts the text itself (`own_window`).
    Yap,
}

pub struct Session {
    pub capture: Option<audio::Capture>,
    pub muted: Option<Muted>,
    pub coordinator: Coordinator<Option<Target>>,
}

pub type SessionState = Arc<Mutex<Session>>;
pub type ModelDownloadState = Arc<Mutex<Option<(String, Arc<AtomicBool>)>>>;

impl Session {
    pub fn new() -> Self {
        Self {
            capture: None,
            muted: None,
            coordinator: Coordinator::default(),
        }
    }

    pub fn idle(&self) -> bool {
        self.coordinator.idle()
    }

    pub fn stop_for_exit(&mut self) -> Option<audio::Capture> {
        self.coordinator.shutdown();
        self.muted = None;
        self.capture.take()
    }
}

/// The user's settings, kept apart from the dictation session so reading
/// them never waits for a dictation. Changes replace the whole value, and
/// readers get a snapshot that later changes don't touch.
#[derive(Default)]
pub struct SharedSettings(RwLock<Arc<Settings>>);

impl SharedSettings {
    pub fn get(&self) -> Arc<Settings> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn set(&self, settings: Settings) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(settings);
    }
}

/// Locks a mutex, turning poisoning into a user-visible error.
pub fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_snapshot_survives_a_change() {
        let shared = SharedSettings::default();
        let before = shared.get();
        shared.set(Settings {
            show_overlay: !before.show_overlay,
            ..Settings::default()
        });
        assert_eq!(before.show_overlay, Settings::default().show_overlay);
        assert_eq!(shared.get().show_overlay, !before.show_overlay);
    }
}
