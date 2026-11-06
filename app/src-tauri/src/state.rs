//! Shared app state: the dictation session and the user's settings.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use yap_app::coordinator::Coordinator;
use yap_app::settings::Settings;

use crate::audio;
use crate::platform_input::Destination;

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
    pub coordinator: Coordinator<Option<Target>>,
    pub settings: Settings,
    #[cfg(all(debug_assertions, feature = "input-test"))]
    pub test_audio: audio::Fixture,
}

pub type SessionState = Arc<Mutex<Session>>;
pub type ModelDownloadState = Arc<Mutex<Option<(String, Arc<AtomicBool>)>>>;

impl Session {
    pub fn new(settings: Settings) -> Self {
        Self {
            capture: None,
            coordinator: Coordinator::default(),
            settings,
            #[cfg(all(debug_assertions, feature = "input-test"))]
            test_audio: audio::Fixture::Speech,
        }
    }

    pub fn idle(&self) -> bool {
        self.coordinator.idle()
    }

    pub fn stop_for_exit(&mut self) -> Option<audio::Capture> {
        self.coordinator.shutdown();
        self.capture.take()
    }
}

/// Locks a mutex, turning poisoning into a user-visible error.
pub fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|error| error.to_string())
}
