use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{SpeechProvider, Transcript};

pub struct LocalWhisper {
    context: WhisperContext,
    model_id: String,
}

impl LocalWhisper {
    pub fn load(model_id: String, artifact: &Path) -> Result<Self, String> {
        if !artifact.is_file() {
            return Err("Select an installed whisper.cpp model file".to_owned());
        }

        whisper_rs::install_logging_hooks();
        let context =
            WhisperContext::new_with_params(artifact, WhisperContextParameters::default())
                .map_err(|error| error.to_string())?;
        Ok(Self { context, model_id })
    }
}

impl SpeechProvider for LocalWhisper {
    fn transcribe(
        &mut self,
        samples_16k_mono: &[f32],
        cancelled: &AtomicBool,
    ) -> Result<Transcript, String> {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        if samples_16k_mono.len() < 1_600 {
            return Err("No meaningful audio".to_owned());
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|error| error.to_string())?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state
            .full(params, samples_16k_mono)
            .map_err(|error| error.to_string())?;
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let text = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<String>();
        Ok(Transcript {
            text: text.trim().to_owned(),
            model_id: self.model_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_is_a_recoverable_error() {
        assert!(
            LocalWhisper::load("missing".to_owned(), Path::new("missing-whisper-model.bin"))
                .is_err()
        );
    }

    #[test]
    #[ignore = "requires a selected model and synthetic speech fixture"]
    fn transcribes_synthetic_speech() {
        let model = std::env::var("YAP_TEST_MODEL").unwrap();
        let pcm = std::fs::read(std::env::var("YAP_TEST_PCM").unwrap()).unwrap();
        let samples = pcm
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        let mut provider = LocalWhisper::load("base".to_owned(), Path::new(&model)).unwrap();
        let result = provider
            .transcribe(&samples, &AtomicBool::new(false))
            .unwrap();
        assert!(
            result.text.to_ascii_lowercase().contains("player health"),
            "synthetic speech was not recognized: {}",
            result.text
        );
    }
}
