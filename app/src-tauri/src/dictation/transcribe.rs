//! Turns a finished recording into text.

use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Manager};
use yap_app::coordinator::TranscriptionRequest;

use super::engines::Engines;
use super::live::{self, LiveState};

pub(super) struct Audio {
    pub normalized: Vec<f32>,
    /// Streaming engines finish from the raw capture they were fed.
    pub raw: Option<(Vec<f32>, u32)>,
}

pub(super) fn run(
    app: &AppHandle,
    request: &TranscriptionRequest,
    audio: Audio,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    if let Some((samples, rate)) = &audio.raw
        && let Some(result) = live::finish(
            &app.state::<LiveState>(),
            request.id,
            samples,
            *rate,
            cancelled,
        )
    {
        return result;
    }

    let engines = app.state::<Engines>();
    let speech = &request.settings.speech;
    let language = request.settings.language.clone();
    let mut provider = engines.take(speech, &language)?;
    let result = provider
        .transcribe(&audio.normalized, cancelled)
        .map(|transcript| transcript.text);
    engines.keep(speech, language, provider);
    result
}
