//! Shared app state: the dictation session and the user's settings.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use yap_app::coordinator::{Coordinator, Phase};
use yap_app::settings::Settings;

use crate::audio;
use crate::platform_input::Destination;

pub struct HistoryMeta {
    pub duration_seconds: f32,
    pub engine: String,
    pub model: String,
}

/// Where a dictation's text goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A field in another app, reached through the OS.
    App(Destination),
    /// Yap's own window, which inserts the text itself (`own_window`).
    Yap,
}

/// The last successful insertion, kept briefly so the overlay can undo it.
pub struct LastInsert {
    pub id: u64,
    pub target: Target,
    pub at: Instant,
}

pub struct Session {
    pub capture: Option<audio::Capture>,
    pub history_meta: Option<HistoryMeta>,
    pub coordinator: Coordinator<Option<Target>>,
    pub active_id: Option<u64>,
    pub target: Option<Target>,
    pub cancelled: Arc<AtomicBool>,
    pub settings: Settings,
    pub last_insert: Option<LastInsert>,
    pub exiting: bool,
    #[cfg(all(debug_assertions, feature = "input-test"))]
    pub test_audio: audio::Fixture,
}

pub type SessionState = Arc<Mutex<Session>>;
pub type ModelDownloadState = Arc<Mutex<Option<(String, Arc<AtomicBool>)>>>;

impl Session {
    pub fn new(settings: Settings) -> Self {
        Self {
            capture: None,
            history_meta: None,
            coordinator: Coordinator::default(),
            active_id: None,
            target: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            settings,
            last_insert: None,
            exiting: false,
            #[cfg(all(debug_assertions, feature = "input-test"))]
            test_audio: audio::Fixture::Speech,
        }
    }

    pub fn idle(&self) -> bool {
        !self.exiting && self.coordinator.phase() == Phase::Idle
    }

    pub fn stop_for_exit(&mut self) -> Option<audio::Capture> {
        self.exiting = true;
        self.cancelled.store(true, Ordering::Relaxed);
        self.active_id = None;
        self.target = None;
        self.history_meta = None;
        self.last_insert = None;
        self.coordinator = Coordinator::default();
        self.capture.take()
    }
}

/// Locks a mutex, turning poisoning into a user-visible error.
pub fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use yap_app::coordinator::{Decision, Settings as JobSettings};
    use yap_app::speech::SpeechConfig;

    #[test]
    fn exit_discards_pending_job_and_rejects_late_transcript() {
        let mut session = Session::new(Settings::default());
        let cancelled = Arc::clone(&session.cancelled);
        let now = Instant::now();
        let id = session
            .coordinator
            .start(
                JobSettings {
                    microphone_index: 0,
                    speech: SpeechConfig::Local {
                        adapter: "test".to_owned(),
                        model_id: "test".to_owned(),
                        artifact: PathBuf::new(),
                    },
                    language: None,
                },
                None,
                now,
            )
            .unwrap();
        session.active_id = Some(id);
        session.coordinator.release(id, true, now);
        assert!(session.stop_for_exit().is_none());
        assert!(session.exiting);
        assert!(!session.idle());
        assert!(cancelled.load(Ordering::Relaxed));
        assert_eq!(session.active_id, None);
        assert_eq!(session.coordinator.phase(), Phase::Idle);
        assert!(!session.coordinator.transcript(id, "late".to_owned(), now));
        assert_eq!(session.coordinator.begin_insert(id, true), Decision::None);
    }
}
