use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub mod cloud;
pub mod openai_compatible;
#[cfg(feature = "local-whisper")]
pub mod whisper;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechConfig {
    Local {
        adapter: String,
        model_id: String,
        artifact: PathBuf,
    },
    OpenAiCompatible {
        model_id: String,
        endpoint: String,
        api_key_env: Option<String>,
    },
    Cloud {
        provider: String,
        model_id: String,
        /// Only used by the custom OpenAI-compatible provider.
        base_url: String,
    },
}

impl SpeechConfig {
    /// Engine and model names shown in history and the overlay.
    pub fn describe(&self) -> (String, String) {
        match self {
            SpeechConfig::Local { model_id, .. } => (
                "On-device".to_owned(),
                format!(
                    "Whisper {}",
                    model_id
                        .trim_start_matches("ggml-")
                        .trim_end_matches(".bin")
                ),
            ),
            SpeechConfig::OpenAiCompatible { model_id, .. } => {
                ("Self-hosted".to_owned(), model_id.clone())
            }
            SpeechConfig::Cloud {
                provider, model_id, ..
            } => (
                cloud::provider(provider)
                    .map_or(provider.as_str(), |provider| provider.label)
                    .to_owned(),
                model_id.clone(),
            ),
        }
    }
}

#[derive(Debug)]
pub struct Transcript {
    pub text: String,
    pub model_id: String,
}

pub trait SpeechProvider: Send {
    fn transcribe(
        &mut self,
        samples_16k_mono: &[f32],
        cancelled: &AtomicBool,
    ) -> Result<Transcript, String>;
}

/// `language` is an ISO-639-1 code, or `None` to let the engine detect it.
pub fn load_provider(
    config: &SpeechConfig,
    language: Option<&str>,
) -> Result<Box<dyn SpeechProvider>, String> {
    match config {
        #[cfg(feature = "local-whisper")]
        SpeechConfig::Local {
            adapter,
            model_id,
            artifact,
        } if adapter == "whisper.cpp" => Ok(Box::new(whisper::LocalWhisper::load(
            model_id.clone(),
            artifact,
            language,
        )?)),
        SpeechConfig::Local { .. } => {
            Err("Selected local speech adapter is unavailable".to_owned())
        }
        SpeechConfig::OpenAiCompatible {
            model_id,
            endpoint,
            api_key_env,
        } => Ok(Box::new(openai_compatible::OpenAiCompatible::new(
            model_id.clone(),
            endpoint,
            api_key_env.as_deref(),
            language,
        )?)),
        SpeechConfig::Cloud {
            provider,
            model_id,
            base_url,
        } => Ok(Box::new(openai_compatible::OpenAiCompatible::cloud(
            model_id.clone(),
            provider,
            base_url,
            language,
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    struct FakeSpeech;

    impl SpeechProvider for FakeSpeech {
        fn transcribe(&mut self, _: &[f32], cancelled: &AtomicBool) -> Result<Transcript, String> {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }

            Ok(Transcript {
                text: "test transcript".to_owned(),
                model_id: "fake".to_owned(),
            })
        }
    }

    #[test]
    fn unsupported_adapter_never_falls_back_to_another_location() {
        let config = SpeechConfig::Local {
            adapter: "unknown".to_owned(),
            model_id: "test".to_owned(),
            artifact: "missing.bin".into(),
        };
        assert!(load_provider(&config, None).is_err());
        assert!(
            load_provider(
                &SpeechConfig::Cloud {
                    provider: "unknown".to_owned(),
                    model_id: "test".to_owned(),
                    base_url: String::new(),
                },
                None
            )
            .is_err()
        );
        assert!(
            load_provider(
                &SpeechConfig::OpenAiCompatible {
                    model_id: "test".to_owned(),
                    endpoint: "not a URL".to_owned(),
                    api_key_env: None,
                },
                None
            )
            .is_err()
        );
    }

    #[test]
    fn provider_contract_does_not_require_a_specific_runtime() {
        let mut provider: Box<dyn SpeechProvider> = Box::new(FakeSpeech);
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            provider.transcribe(&[0.1], &cancelled).unwrap().text,
            "test transcript"
        );
        cancelled.store(true, Ordering::Relaxed);
        assert!(provider.transcribe(&[0.1], &cancelled).is_err());
    }
}
