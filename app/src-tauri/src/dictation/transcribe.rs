//! Turns a finished recording into text.

use std::sync::atomic::AtomicBool;

use yap_app::coordinator::TranscriptionRequest;

use super::{engines, live};

pub(super) struct Audio {
    pub normalized: Vec<f32>,
    /// Streaming engines finish from the raw capture they were fed.
    pub raw: Option<(Vec<f32>, u32)>,
}

pub(super) fn run(
    request: &TranscriptionRequest,
    audio: Audio,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    #[cfg(all(debug_assertions, feature = "input-test"))]
    if crate::input_test::active() {
        use std::sync::atomic::Ordering;
        use yap_app::speech::SpeechConfig;

        std::thread::sleep(std::time::Duration::from_millis(300));
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }
        if matches!(&request.settings.speech, SpeechConfig::Local { model_id, .. } if model_id == "test-inference-error")
        {
            return Err("Test inference failure".to_owned());
        }
        return Ok("Yap test ✓".to_owned());
    }

    if let Some((samples, rate)) = &audio.raw
        && let Some(result) = live::finish(request.id, samples, *rate, cancelled)
    {
        return result;
    }

    let speech = &request.settings.speech;
    let language = request.settings.language.clone();
    let mut provider = engines::take(speech, &language)?;
    let result = provider
        .transcribe(&audio.normalized, cancelled)
        .map(|transcript| transcript.text);
    engines::keep(speech, language, provider);
    result
}
