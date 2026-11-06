//! A streaming engine's session for the recording in progress.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use yap_app::speech::{self, SpeechConfig};

use super::engines::Engines;
use crate::state::SessionState;
use crate::{audio, overlay};

struct Live {
    id: u64,
    session: Box<dyn speech::LiveSession>,
    /// Raw capture samples already fed.
    fed: usize,
}

#[derive(Default)]
pub struct LiveState(Mutex<Option<Live>>);

impl LiveState {
    fn take(&self, id: u64) -> Option<Live> {
        let mut live = self.0.lock().ok()?;
        if live.as_ref().is_some_and(|live| live.id == id) {
            live.take()
        } else {
            None
        }
    }

    pub(super) fn clear(&self) {
        if let Ok(mut live) = self.0.lock() {
            *live = None;
        }
    }
}

/// Feeds a streaming engine while recording `id` and shows its partial text
/// in the overlay. The final decode happens in `finish`, which takes the
/// session over. If the model is still loading when the key is released,
/// there is no session to finish and the whole recording is transcribed.
pub(super) fn start(
    app: &AppHandle,
    id: u64,
    speech: SpeechConfig,
    language: Option<String>,
    tap: audio::Tap,
) {
    let app = app.clone();
    std::thread::spawn(move || {
        let still_recording = |app: &AppHandle| {
            app.state::<SessionState>()
                .lock()
                .is_ok_and(|session| session.coordinator.is_active(id) && session.capture.is_some())
        };
        let engines = app.state::<Engines>();
        let Ok(mut engine) = engines.take(&speech, &language) else {
            return;
        };
        let session = engine.start_live();
        engines.keep(&speech, language, engine);
        let Some(session) = session else {
            return;
        };
        if !still_recording(&app) {
            return;
        }
        let state = app.state::<LiveState>();
        if let Ok(mut live) = state.0.lock() {
            *live = Some(Live {
                id,
                session,
                fed: 0,
            });
        }

        let mut shown = String::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let active = app
                .state::<SessionState>()
                .lock()
                .is_ok_and(|session| session.coordinator.is_active(id));
            let text = {
                let Ok(mut slot) = state.0.lock() else {
                    return;
                };
                let Some(live) = slot.as_mut().filter(|live| live.id == id) else {
                    // `finish` took the session.
                    return;
                };
                if !active {
                    *slot = None;
                    return;
                }
                let fresh = tap.read_from(live.fed);
                live.fed += fresh.len();
                live.session.feed(tap.sample_rate, &fresh)
            };
            if text != shown {
                overlay::partial(&app, &text);
                shown = text;
            }
        }
    });
}

pub(super) fn finish(
    live: &LiveState,
    id: u64,
    samples: &[f32],
    sample_rate: u32,
    cancelled: &AtomicBool,
) -> Option<Result<String, String>> {
    let live = live.take(id)?;
    let mut session = live.session;
    session.feed(sample_rate, samples.get(live.fed..).unwrap_or_default());
    if cancelled.load(Ordering::Relaxed) {
        return Some(Err("Cancelled".to_owned()));
    }
    Some(Ok(session.finish()))
}
