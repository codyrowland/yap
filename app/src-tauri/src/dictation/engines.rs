//! The loaded on-device model, kept between dictations: loading a model
//! takes seconds and would otherwise happen on every key release. Cloud and
//! self-hosted clients are cheap and are built fresh each time.

use std::sync::Mutex;

use yap_app::settings::Settings;
use yap_app::speech::{self, SpeechConfig};

type LoadedEngine = (
    SpeechConfig,
    Option<String>,
    Box<dyn speech::SpeechProvider>,
);
static LOCAL_ENGINE: Mutex<Option<LoadedEngine>> = Mutex::new(None);

/// Returns the cached engine if it matches, or loads it. Holding the cache
/// lock while loading makes a dictation wait for a preload in progress
/// instead of loading the same model twice.
pub(super) fn take(
    speech: &SpeechConfig,
    language: &Option<String>,
) -> Result<Box<dyn speech::SpeechProvider>, String> {
    if !matches!(speech, SpeechConfig::Local { .. }) {
        return speech::load_provider(speech, language.as_deref());
    }
    let mut cache = LOCAL_ENGINE.lock().map_err(|error| error.to_string())?;
    match cache.take() {
        Some((cached, cached_language, engine))
            if &cached == speech && &cached_language == language =>
        {
            Ok(engine)
        }
        other => {
            // Free the previous model before loading another one.
            drop(other);
            speech::load_provider(speech, language.as_deref())
        }
    }
}

pub(super) fn keep(
    speech: &SpeechConfig,
    language: Option<String>,
    engine: Box<dyn speech::SpeechProvider>,
) {
    if matches!(speech, SpeechConfig::Local { .. })
        && let Ok(mut cache) = LOCAL_ENGINE.lock()
    {
        *cache = Some((speech.clone(), language, engine));
    }
}

/// Frees the cached on-device model.
pub fn unload() {
    if let Ok(mut cache) = LOCAL_ENGINE.lock() {
        *cache = None;
    }
}

/// Loads the selected on-device model in the background so the first
/// dictation after starting Yap (or switching models) is fast. Switching to
/// a cloud or self-hosted engine frees the cached model.
pub fn preload(settings: &Settings) {
    let Ok(speech) = settings.speech_config() else {
        return;
    };
    let language = settings.language_code();
    std::thread::spawn(move || {
        if !matches!(speech, SpeechConfig::Local { .. }) {
            unload();
            return;
        }
        if let Ok(engine) = take(&speech, &language) {
            keep(&speech, language, engine);
        }
    });
}
