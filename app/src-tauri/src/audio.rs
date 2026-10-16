use std::sync::{Arc, Mutex, mpsc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

const MAX_SECONDS: usize = 30;

pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub limited: bool,
}

impl Recording {
    pub fn normalized_16k(&self) -> Vec<f32> {
        if self.samples.is_empty() || self.sample_rate == 0 {
            return Vec::new();
        }

        let output_len = self.samples.len() as u64 * 16_000 / self.sample_rate as u64;
        let mut output = Vec::with_capacity(output_len as usize);
        for index in 0..output_len {
            let start = index as f64 * self.sample_rate as f64 / 16_000.0;
            let sample = if self.sample_rate > 16_000 {
                let first = start.floor() as usize;
                let end = (((index + 1) as f64 * self.sample_rate as f64 / 16_000.0).floor()
                    as usize)
                    .max(first + 1)
                    .min(self.samples.len());
                self.samples[first..end].iter().sum::<f32>() / (end - first) as f32
            } else {
                let first = start.floor() as usize;
                let next = (first + 1).min(self.samples.len() - 1);
                let fraction = (start - first as f64) as f32;
                self.samples[first] * (1.0 - fraction) + self.samples[next] * fraction
            };
            output.push(if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            });
        }

        output
    }
}

pub fn meaningful_audio(samples: &[f32]) -> bool {
    samples.len() >= 1_600
        && samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32
            >= 0.003_f32.powi(2)
}

struct AudioBuffer {
    samples: Vec<f32>,
    channels: usize,
    max_frames: usize,
    limited: bool,
}

impl AudioBuffer {
    fn append<T: Copy>(&mut self, data: &[T], convert: fn(T) -> f32) {
        for frame in data.chunks_exact(self.channels) {
            if self.samples.len() >= self.max_frames {
                self.limited = true;
                break;
            }

            let mono =
                frame.iter().map(|sample| convert(*sample)).sum::<f32>() / self.channels as f32;
            self.samples.push(mono);
        }
    }
}

pub struct Capture {
    stream: Option<Stream>,
    buffer: Arc<Mutex<AudioBuffer>>,
    errors: mpsc::Receiver<String>,
    sample_rate: u32,
}

fn stream<T: cpal::SizedSample + Copy + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    buffer: Arc<Mutex<AudioBuffer>>,
    errors: mpsc::Sender<String>,
    convert: fn(T) -> f32,
) -> Result<Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.append(data, convert);
                }
            },
            move |error| {
                let _ = errors.send(error.to_string());
            },
            None,
        )
        .map_err(|error| error.to_string())
}

pub fn microphones() -> Result<Vec<String>, String> {
    cpal::default_host()
        .input_devices()
        .map_err(|error| error.to_string())?
        .map(|device| device.name().map_err(|error| error.to_string()))
        .collect()
}

#[cfg(all(debug_assertions, feature = "input-test"))]
#[derive(Clone, Copy)]
pub enum Fixture {
    Speech,
    Silence,
    Bounded,
    CaptureError,
    InferenceError,
}

impl Capture {
    #[cfg(all(debug_assertions, feature = "input-test"))]
    pub fn fixture(kind: Fixture) -> Self {
        let (sender, errors) = mpsc::channel();
        if matches!(kind, Fixture::CaptureError) {
            let _ = sender.send("Test microphone failure".to_owned());
        }
        let mut buffer = AudioBuffer {
            samples: Vec::new(),
            channels: 1,
            max_frames: 16_000 * MAX_SECONDS,
            limited: false,
        };
        let samples = match kind {
            Fixture::Speech | Fixture::CaptureError | Fixture::InferenceError => vec![0.02; 3_200],
            Fixture::Silence => vec![0.0; 3_200],
            Fixture::Bounded => vec![0.02; buffer.max_frames + 1],
        };
        buffer.append(&samples, |sample| sample);
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(buffer)),
            errors,
            sample_rate: 16_000,
        }
    }

    pub fn start(index: usize) -> Result<Self, String> {
        let device = cpal::default_host()
            .input_devices()
            .map_err(|error| error.to_string())?
            .nth(index)
            .ok_or_else(|| "Microphone is no longer available".to_owned())?;
        let supported = device
            .default_input_config()
            .map_err(|error| error.to_string())?;
        let config: StreamConfig = supported.config();
        if config.channels == 0 || config.sample_rate.0 == 0 || config.sample_rate.0 > 192_000 {
            return Err("Unsupported microphone format".to_owned());
        }

        let buffer = Arc::new(Mutex::new(AudioBuffer {
            samples: Vec::new(),
            channels: config.channels as usize,
            max_frames: config.sample_rate.0 as usize * MAX_SECONDS,
            limited: false,
        }));
        let (errors, error_receiver) = mpsc::channel();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => {
                stream::<f32>(&device, &config, buffer.clone(), errors, |sample| sample)
            }
            SampleFormat::I16 => {
                stream::<i16>(&device, &config, buffer.clone(), errors, |sample| {
                    sample as f32 / 32768.0
                })
            }
            SampleFormat::U16 => {
                stream::<u16>(&device, &config, buffer.clone(), errors, |sample| {
                    (sample as f32 - 32768.0) / 32768.0
                })
            }
            _ => return Err("Unsupported microphone sample format".to_owned()),
        }?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Self {
            stream: Some(stream),
            buffer,
            errors: error_receiver,
            sample_rate: config.sample_rate.0,
        })
    }

    pub fn stop(self) -> Result<Recording, String> {
        drop(self.stream);
        if let Ok(error) = self.errors.try_recv() {
            return Err(error);
        }

        let mut buffer = self.buffer.lock().map_err(|error| error.to_string())?;
        Ok(Recording {
            samples: std::mem::take(&mut buffer.samples),
            sample_rate: self.sample_rate,
            limited: buffer.limited,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_channels_to_mono_and_respects_limit() {
        let mut buffer = AudioBuffer {
            samples: Vec::new(),
            channels: 2,
            max_frames: 2,
            limited: false,
        };
        buffer.append(&[1.0_f32, -1.0, 0.5, 0.5, 0.7, 0.7], |sample| sample);
        assert_eq!(buffer.samples, vec![0.0, 0.5]);
        assert!(buffer.limited);
    }

    #[test]
    #[ignore = "requires a connected, permitted microphone"]
    fn selected_microphone_captures_samples() {
        let devices = microphones().unwrap();
        assert!(!devices.is_empty(), "no microphones available");
        let capture = Capture::start(0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let recording = capture.stop().unwrap();
        assert!(!recording.samples.is_empty());
        assert!(recording.sample_rate > 0);
    }

    #[test]
    fn normalizes_to_sixteen_kilohertz() {
        let recording = Recording {
            samples: vec![1.0, 1.0, 1.0, -1.0, -1.0, -1.0],
            sample_rate: 48_000,
            limited: false,
        };
        assert_eq!(recording.normalized_16k(), vec![1.0, -1.0]);
        let recording = Recording {
            samples: vec![0.0, 1.0],
            sample_rate: 8_000,
            limited: false,
        };
        assert_eq!(recording.normalized_16k(), vec![0.0, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn short_or_silent_audio_is_not_meaningful() {
        assert!(!meaningful_audio(&[0.5; 1_599]));
        assert!(!meaningful_audio(&[0.0; 1_600]));
        assert!(meaningful_audio(&[0.01; 1_600]));
    }

    #[test]
    fn partial_frames_are_ignored() {
        let mut buffer = AudioBuffer {
            samples: Vec::new(),
            channels: 2,
            max_frames: 5,
            limited: false,
        };
        buffer.append(&[2_i16, 4, 9], |sample| sample as f32);
        assert_eq!(buffer.samples, vec![3.0]);
        assert!(!buffer.limited);
    }
}
