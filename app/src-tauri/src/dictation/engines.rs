//! The loaded on-device model, kept between dictations: loading a model
//! takes seconds and would otherwise happen on every key release. Cloud and
//! self-hosted clients are cheap and are built fresh each time.

use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use yap_app::settings::Settings;
use yap_app::speech::{self, SpeechConfig, SpeechProvider};

use crate::platform;

/// Builds a speech engine. The app loads real ones; tests put in fakes.
pub type Loader = Box<
    dyn Fn(&SpeechConfig, Option<&str>) -> Result<Box<dyn SpeechProvider>, String> + Send + Sync,
>;

type LoadedEngine = (SpeechConfig, Option<String>, Box<dyn SpeechProvider>);

pub struct Engines {
    load: Loader,
    local: Mutex<Option<LoadedEngine>>,
}

impl Engines {
    pub fn new(load: Loader) -> Self {
        Self {
            load,
            local: Mutex::new(None),
        }
    }

    pub fn system() -> Self {
        Self::new(Box::new(|speech, language| {
            speech::load_provider(speech, language, platform::credential_store())
        }))
    }

    /// Returns the cached engine if it matches, or loads it. Holding the
    /// cache lock while loading makes a dictation wait for a preload in
    /// progress instead of loading the same model twice.
    pub(super) fn take(
        &self,
        speech: &SpeechConfig,
        language: &Option<String>,
    ) -> Result<Box<dyn SpeechProvider>, String> {
        if !matches!(speech, SpeechConfig::Local { .. }) {
            return (self.load)(speech, language.as_deref());
        }
        let mut cache = self.local.lock().map_err(|error| error.to_string())?;
        match cache.take() {
            Some((cached, cached_language, engine))
                if &cached == speech && &cached_language == language =>
            {
                Ok(engine)
            }
            other => {
                // Free the previous model before loading another one.
                drop(other);
                (self.load)(speech, language.as_deref())
            }
        }
    }

    pub(super) fn keep(
        &self,
        speech: &SpeechConfig,
        language: Option<String>,
        engine: Box<dyn SpeechProvider>,
    ) {
        if matches!(speech, SpeechConfig::Local { .. })
            && let Ok(mut cache) = self.local.lock()
        {
            *cache = Some((speech.clone(), language, engine));
        }
    }

    /// Frees the cached on-device model.
    pub fn unload(&self) {
        if let Ok(mut cache) = self.local.lock() {
            *cache = None;
        }
    }
}

/// Loads the selected on-device model in the background so the first
/// dictation after starting Yap (or switching models) is fast. Switching to
/// a cloud or self-hosted engine frees the cached model.
pub fn preload(app: &AppHandle, settings: &Settings) {
    let Ok(speech) = settings.speech_config() else {
        return;
    };
    let language = settings.language_code();
    let app = app.clone();
    std::thread::spawn(move || {
        let engines = app.state::<Engines>();
        if !matches!(speech, SpeechConfig::Local { .. }) {
            engines.unload();
            return;
        }
        if let Ok(engine) = engines.take(&speech, &language) {
            engines.keep(&speech, language, engine);
        }
    });
}

pub fn unload(app: &AppHandle) {
    app.state::<Engines>().unload();
}
