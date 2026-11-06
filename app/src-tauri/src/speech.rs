use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub mod cloud;
pub mod openai_compatible;
#[cfg(feature = "local-sherpa")]
pub mod sherpa;
pub mod stream_input;
#[cfg(feature = "local-whisper")]
pub mod whisper;

/// `SpeechConfig::Local` adapters.
pub const WHISPER_CPP: &str = "whisper.cpp";
pub const SHERPA_OFFLINE: &str = "sherpa-onnx";
pub const SHERPA_STREAMING: &str = "sherpa-onnx-streaming";

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
    /// True for on-device engines that decode while recording.
    pub fn streams(&self) -> bool {
        matches!(self, SpeechConfig::Local { adapter, .. } if adapter == SHERPA_STREAMING)
    }

    /// Engine and model names shown in history and the overlay.
    pub fn describe(&self) -> (String, String) {
        match self {
            SpeechConfig::Local {
                model_id, artifact, ..
            } => (
                "On-device".to_owned(),
                crate::models::for_path(artifact)
                    .map(crate::models::display_name)
                    .unwrap_or_else(|| model_id.clone()),
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

    /// Streaming engines return a session to feed while recording.
    fn start_live(&mut self) -> Option<Box<dyn LiveSession>> {
        None
    }
}

/// A recording being transcribed while it happens.
pub trait LiveSession: Send {
    /// Adds audio (mono, any sample rate) and returns the text so far.
    fn feed(&mut self, sample_rate: u32, samples: &[f32]) -> String;
    /// Ends the audio and returns the final text.
    fn finish(self: Box<Self>) -> String;
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
        } if adapter == WHISPER_CPP => Ok(Box::new(whisper::LocalWhisper::load(
            model_id.clone(),
            artifact,
            language,
        )?)),
        #[cfg(feature = "local-sherpa")]
        SpeechConfig::Local {
            adapter,
            model_id,
            artifact,
        } if adapter == SHERPA_OFFLINE => {
            let family = crate::models::for_path(artifact)
                .map(|model| model.family)
                .ok_or("Unknown on-device model")?;
            Ok(Box::new(sherpa::OfflineSherpa::load(
                family,
                model_id.clone(),
                artifact,
            )?))
        }
        #[cfg(feature = "local-sherpa")]
        SpeechConfig::Local {
            adapter,
            model_id,
            artifact,
        } if adapter == SHERPA_STREAMING => Ok(Box::new(sherpa::StreamingSherpa::load(
            model_id.clone(),
            artifact,
            language,
        )?)),
        SpeechConfig::Local { .. } => {
            Err("Selected on-device engine is not included in this build".to_owned())
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
