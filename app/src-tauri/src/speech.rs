use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[cfg(feature = "local-whisper")]
pub mod whisper;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechConfig {
    Local {
        adapter: String,
        model_id: String,
        artifact: PathBuf,
    },
    SelfHosted {
        adapter: String,
        model_id: String,
        endpoint: String,
    },
}

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

pub fn load_provider(config: &SpeechConfig) -> Result<Box<dyn SpeechProvider>, String> {
    match config {
        #[cfg(feature = "local-whisper")]
        SpeechConfig::Local {
            adapter,
            model_id,
            artifact,
        } if adapter == "whisper.cpp" => Ok(Box::new(whisper::LocalWhisper::load(
            model_id.clone(),
            artifact,
        )?)),
        SpeechConfig::Local { .. } => {
            Err("Selected local speech adapter is unavailable".to_owned())
        }
        SpeechConfig::SelfHosted { .. } => {
            Err("Self-hosted speech is not available in this milestone".to_owned())
        }
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
        assert!(load_provider(&config).is_err());
        assert!(
            load_provider(&SpeechConfig::SelfHosted {
                adapter: "remote".to_owned(),
                model_id: "test".to_owned(),
                endpoint: "https://example.invalid".to_owned(),
            })
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
