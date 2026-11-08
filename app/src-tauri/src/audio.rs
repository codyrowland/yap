use std::sync::{Arc, Mutex, mpsc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

/// Longest single recording. Toggle dictation is meant for longer passages;
/// when the limit is reached the recording stops and is transcribed.
pub const MAX_SECONDS: usize = 300;

pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
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
    /// RMS of the most recent callback, for the overlay's level meter.
    level: f32,
}

impl AudioBuffer {
    fn append<T: Copy>(&mut self, data: &[T], convert: fn(T) -> f32) {
        let start = self.samples.len();
        for frame in data.chunks_exact(self.channels) {
            if self.samples.len() >= self.max_frames {
                self.limited = true;
                break;
            }

            let mono =
                frame.iter().map(|sample| convert(*sample)).sum::<f32>() / self.channels as f32;
            self.samples.push(mono);
        }

        let recent = &self.samples[start..];
        if !recent.is_empty() {
            self.level = (recent.iter().map(|sample| sample * sample).sum::<f32>()
                / recent.len() as f32)
                .sqrt();
        }
    }
}

/// A read-only view of a running capture, for streaming engines that consume
/// audio while the recording is still going.
#[derive(Clone)]
pub struct Tap {
    buffer: Arc<Mutex<AudioBuffer>>,
    pub sample_rate: u32,
}

impl Tap {
    /// Mono samples captured from index `start` onwards (at `sample_rate`).
    pub fn read_from(&self, start: usize) -> Vec<f32> {
        self.buffer
            .lock()
            .map(|buffer| buffer.samples.get(start..).unwrap_or_default().to_vec())
            .unwrap_or_default()
    }
}

/// The cpal stream lives on its own thread: on macOS it cannot move between
/// threads, and the session that owns the capture is shared.
struct Running {
    stop: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Opened = (Stream, Arc<Mutex<AudioBuffer>>, mpsc::Receiver<String>, u32);

pub struct Capture {
    stream: Option<Running>,
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

/// Where recordings come from. The app uses the system's microphones;
/// tests put in a fake one.
pub trait Microphone: Send + Sync {
    fn start(&self, index: usize) -> Result<Capture, String>;
}

pub struct SystemMicrophone;

impl Microphone for SystemMicrophone {
    fn start(&self, index: usize) -> Result<Capture, String> {
        Capture::start(index)
    }
}

pub type MicrophoneState = Box<dyn Microphone>;

impl Capture {
    /// A finished capture of mono `samples`; `error` is reported on stop.
    #[cfg_attr(not(feature = "input-test"), allow(dead_code))]
    pub fn prerecorded(samples: &[f32], sample_rate: u32, error: Option<String>) -> Self {
        let (sender, errors) = mpsc::channel();
        if let Some(error) = error {
            let _ = sender.send(error);
        }
        let mut buffer = AudioBuffer {
            samples: Vec::new(),
            channels: 1,
            max_frames: sample_rate as usize * MAX_SECONDS,
            limited: false,
            level: 0.0,
        };
        buffer.append(samples, |sample| sample);
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(buffer)),
            errors,
            sample_rate,
        }
    }

    fn start(index: usize) -> Result<Self, String> {
        let (ready, opened) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || match Self::open(index) {
            Ok((stream, buffer, errors, sample_rate)) => {
                let _ = ready.send(Ok((buffer, errors, sample_rate)));
                let _ = stopped.recv();
                drop(stream);
            }
            Err(error) => {
                let _ = ready.send(Err(error));
            }
        });
        let (buffer, errors, sample_rate) = opened
            .recv()
            .map_err(|_| "Microphone stopped unexpectedly".to_owned())??;
        Ok(Self {
            stream: Some(Running {
                stop,
                thread: Some(thread),
            }),
            buffer,
            errors,
            sample_rate,
        })
    }

    fn open(index: usize) -> Result<Opened, String> {
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
            level: 0.0,
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
        Ok((stream, buffer, error_receiver, config.sample_rate.0))
    }

    pub fn tap(&self) -> Tap {
        Tap {
            buffer: Arc::clone(&self.buffer),
            sample_rate: self.sample_rate,
        }
    }

    /// Current input level (RMS, 0–1) and whether the length limit was hit.
    pub fn meter(&self) -> (f32, bool) {
        self.buffer
            .lock()
            .map(|buffer| (buffer.level, buffer.limited))
            .unwrap_or((0.0, false))
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
            level: 0.0,
        };
        buffer.append(&[1.0_f32, -1.0, 0.5, 0.5, 0.7, 0.7], |sample| sample);
        assert_eq!(buffer.samples, vec![0.0, 0.5]);
        assert!(buffer.limited);
        assert!((buffer.level - 0.125_f32.sqrt()).abs() < 1e-6);
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
        };
        assert_eq!(recording.normalized_16k(), vec![1.0, -1.0]);
        let recording = Recording {
            samples: vec![0.0, 1.0],
            sample_rate: 8_000,
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
    fn tap_reads_new_samples_without_consuming_them() {
        let buffer = Arc::new(Mutex::new(AudioBuffer {
            samples: Vec::new(),
            channels: 1,
            max_frames: 100,
            limited: false,
            level: 0.0,
        }));
        let tap = Tap {
            buffer: Arc::clone(&buffer),
            sample_rate: 16_000,
        };
        buffer
            .lock()
            .unwrap()
            .append(&[0.1_f32, 0.2, 0.3], |sample| sample);
        assert_eq!(tap.read_from(0), vec![0.1, 0.2, 0.3]);
        assert_eq!(tap.read_from(2), vec![0.3]);
        assert!(tap.read_from(9).is_empty());
        assert_eq!(buffer.lock().unwrap().samples.len(), 3);
    }

    #[test]
    fn partial_frames_are_ignored() {
        let mut buffer = AudioBuffer {
            samples: Vec::new(),
            channels: 2,
            max_frames: 5,
            limited: false,
            level: 0.0,
        };
        buffer.append(&[2_i16, 4, 9], |sample| sample as f32);
        assert_eq!(buffer.samples, vec![3.0]);
        assert!(!buffer.limited);
    }
}
