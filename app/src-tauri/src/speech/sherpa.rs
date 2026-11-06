//! On-device engines that run on sherpa-onnx.
//!
//! - [`OfflineSherpa`]: Parakeet TDT (transducer) and Moonshine v2 (encoder +
//!   merged decoder). They transcribe the whole recording after release.
//! - [`StreamingSherpa`]: Nemotron streaming transducers. They decode while
//!   you speak; [`SpeechProvider::start_live`] hands out a session that the
//!   dictation pipeline feeds during recording, so the overlay can show
//!   partial text and the final result is ready almost as soon as you stop.
//!
//! Each model folder holds the files listed by `models::required_files`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};

use super::stream_input::StreamInput;
use super::{LiveSession, SpeechProvider, Transcript};
use crate::models::{self, Family};

/// Leaves a core or two for the UI and the app being dictated into.
fn threads() -> i32 {
    std::thread::available_parallelism()
        .map(|count| count.get().saturating_sub(2).clamp(1, 8))
        .unwrap_or(2) as i32
}

fn check_folder(family: Family, folder: &Path) -> Result<(), String> {
    match models::required_files(family)
        .iter()
        .find(|file| !folder.join(file).is_file())
    {
        Some(missing) => Err(format!(
            "The {} model is incomplete ({missing} is missing); download it again in Settings",
            family.name()
        )),
        None => Ok(()),
    }
}

fn file(folder: &Path, name: &str) -> Option<String> {
    Some(folder.join(name).to_string_lossy().into_owned())
}

pub struct OfflineSherpa {
    recognizer: OfflineRecognizer,
    model_id: String,
}

impl OfflineSherpa {
    pub fn load(family: Family, model_id: String, folder: &Path) -> Result<Self, String> {
        check_folder(family, folder)?;
        let mut config = OfflineRecognizerConfig::default();
        match family {
            Family::Parakeet => {
                config.model_config.transducer = OfflineTransducerModelConfig {
                    encoder: file(folder, "encoder.int8.onnx"),
                    decoder: file(folder, "decoder.int8.onnx"),
                    joiner: file(folder, "joiner.int8.onnx"),
                };
                config.model_config.model_type = Some("nemo_transducer".to_owned());
            }
            Family::Moonshine => {
                config.model_config.moonshine = OfflineMoonshineModelConfig {
                    encoder: file(folder, "encoder_model.ort"),
                    merged_decoder: file(folder, "decoder_model_merged.ort"),
                    ..Default::default()
                };
            }
            Family::Whisper | Family::Nemotron => {
                return Err(format!(
                    "{} is not an offline sherpa-onnx model",
                    family.name()
                ));
            }
        }
        config.model_config.tokens = file(folder, "tokens.txt");
        config.model_config.num_threads = threads();
        config.model_config.provider = Some("cpu".to_owned());
        config.decoding_method = Some("greedy_search".to_owned());
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            format!(
                "Could not load the {} model; download it again in Settings",
                family.name()
            )
        })?;
        Ok(Self {
            recognizer,
            model_id,
        })
    }
}

impl SpeechProvider for OfflineSherpa {
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

        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples_16k_mono);
        self.recognizer.decode(&stream);
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let text = stream
            .get_result()
            .map(|result| result.text.trim().to_owned())
            .ok_or("The on-device model returned no result")?;
        Ok(Transcript {
            text,
            model_id: self.model_id.clone(),
        })
    }
}

pub struct StreamingSherpa {
    recognizer: Arc<OnlineRecognizer>,
    model_id: String,
    /// Per-stream language for multilingual models ("auto" to detect).
    language: Option<String>,
}

impl StreamingSherpa {
    /// `language` is only used by models that accept one (Nemotron 3.5).
    pub fn load(model_id: String, folder: &Path, language: Option<&str>) -> Result<Self, String> {
        check_folder(Family::Nemotron, folder)?;
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer = OnlineTransducerModelConfig {
            encoder: file(folder, "encoder.int8.onnx"),
            decoder: file(folder, "decoder.int8.onnx"),
            joiner: file(folder, "joiner.int8.onnx"),
        };
        config.model_config.tokens = file(folder, "tokens.txt");
        config.model_config.num_threads = threads();
        config.model_config.provider = Some("cpu".to_owned());
        config.decoding_method = Some("greedy_search".to_owned());
        config.max_active_paths = 4;
        let recognizer = OnlineRecognizer::create(&config)
            .ok_or("Could not load the Nemotron model; download it again in Settings")?;
        let multilingual = models::model(&model_id).is_some_and(models::accepts_language);
        Ok(Self {
            recognizer: Arc::new(recognizer),
            language: multilingual.then(|| language.unwrap_or("auto").to_owned()),
            model_id,
        })
    }

    fn session(&self) -> Live {
        let stream = self.recognizer.create_stream();
        if let Some(language) = &self.language {
            stream.set_option("language", language);
        }
        Live {
            recognizer: Arc::clone(&self.recognizer),
            stream,
            input: StreamInput::default(),
        }
    }
}

/// One dictation's streaming state. Holds its own handle to the recognizer
/// so the stream never outlives it.
struct Live {
    recognizer: Arc<OnlineRecognizer>,
    stream: OnlineStream,
    /// Keeps every chunk at one sample rate; sherpa-onnx exits the process
    /// if the rate changes within a stream.
    input: StreamInput,
}

impl Live {
    fn text(&self) -> String {
        self.recognizer
            .get_result(&self.stream)
            .map(|result| result.text.trim().to_owned())
            .unwrap_or_default()
    }
}

impl LiveSession for Live {
    fn feed(&mut self, sample_rate: u32, samples: &[f32]) -> String {
        if let Some((rate, chunk)) = self.input.chunk(sample_rate, samples) {
            // sherpa-onnx resamples to 16 kHz itself.
            self.stream.accept_waveform(rate as i32, &chunk);
            while self.recognizer.is_ready(&self.stream) {
                self.recognizer.decode(&self.stream);
            }
        }
        self.text()
    }

    fn finish(self: Box<Self>) -> String {
        let (rate, tail) = self.input.tail();
        self.stream.accept_waveform(rate as i32, &tail);
        self.stream.input_finished();
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        self.text()
    }
}

impl SpeechProvider for StreamingSherpa {
    /// Used when no live session ran (the model was still loading when the
    /// recording ended): feeds the whole recording at once.
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
        let mut live = Box::new(self.session());
        live.feed(16_000, samples_16k_mono);
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }
        Ok(Transcript {
            text: live.finish(),
            model_id: self.model_id.clone(),
        })
    }

    fn start_live(&mut self) -> Option<Box<dyn LiveSession>> {
        Some(Box::new(self.session()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_folders_are_recoverable_errors() {
        let folder = Path::new("missing-model-folder");
        let error = OfflineSherpa::load(Family::Parakeet, "p".into(), folder)
            .err()
            .unwrap();
        assert!(error.contains("encoder.int8.onnx"), "{error}");
        let error = OfflineSherpa::load(Family::Moonshine, "m".into(), folder)
            .err()
            .unwrap();
        assert!(error.contains("encoder_model.ort"), "{error}");
        assert!(StreamingSherpa::load("n".into(), folder, None).is_err());
    }

    /// Set YAP_TEST_MODEL_DIR to a folder holding downloaded models (one
    /// sub-folder per model id) and YAP_TEST_PCM to 16 kHz mono f32 PCM.
    #[test]
    #[ignore = "requires downloaded models and a speech fixture"]
    fn transcribes_with_every_sherpa_family() {
        let root = std::path::PathBuf::from(std::env::var("YAP_TEST_MODEL_DIR").unwrap());
        let pcm = std::fs::read(std::env::var("YAP_TEST_PCM").unwrap()).unwrap();
        let samples = pcm
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        let cancelled = AtomicBool::new(false);
        for id in ["parakeet-tdt-0.6b-v3", "moonshine-tiny-en"] {
            let family = models::model(id).unwrap().family;
            let mut engine = OfflineSherpa::load(family, id.into(), &root.join(id)).unwrap();
            assert!(
                !engine
                    .transcribe(&samples, &cancelled)
                    .unwrap()
                    .text
                    .is_empty()
            );
        }
        let mut engine = StreamingSherpa::load(
            "nemotron-streaming-en-0.6b".into(),
            &root.join("nemotron-streaming-en-0.6b"),
            None,
        )
        .unwrap();
        let mut live = engine.start_live().unwrap();
        for chunk in samples.chunks(1_600) {
            live.feed(16_000, chunk);
        }
        assert!(!live.finish().is_empty());
    }
}
