//! Prepares audio for a streaming recognizer.
//!
//! sherpa-onnx fixes a stream's sample rate on the first audio it receives
//! and terminates the whole process (`exit(-1)`) if a later call uses a
//! different rate. So every chunk for one stream, including the lead-in and
//! tail silence, must use the same rate. `StreamInput` enforces that: the
//! first real audio sets the rate, the lead-in silence is generated at that
//! rate, and anything at another rate is resampled before it is passed on.
//! This module has no sherpa-onnx dependency so it is tested everywhere.

/// Silence fed before the first words. Nemotron drops the start of speech
/// that begins in its very first chunk (seen with sherpa-onnx's own sample
/// clips); half a second of lead-in fixes it.
pub const LEAD_IN_SECONDS: f32 = 0.5;
/// Silence fed after the last words so the final chunk is decoded.
pub const TAIL_SECONDS: f32 = 0.8;

#[derive(Debug, Default)]
pub struct StreamInput {
    rate: Option<u32>,
}

fn silence(rate: u32, seconds: f32) -> Vec<f32> {
    vec![0.0; (rate as f32 * seconds) as usize]
}

/// Linear-interpolation resampler; only used if a caller ever mixes rates.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    let length = (samples.len() as u64 * to as u64 / from as u64).max(1) as usize;
    (0..length)
        .map(|index| {
            let position = index as f64 * from as f64 / to as f64;
            let first = (position.floor() as usize).min(samples.len() - 1);
            let next = (first + 1).min(samples.len() - 1);
            let fraction = (position - first as f64) as f32;
            samples[first] * (1.0 - fraction) + samples[next] * fraction
        })
        .collect()
}

impl StreamInput {
    /// The rate every chunk is sent at, once known.
    pub fn rate(&self) -> Option<u32> {
        self.rate
    }

    /// Returns what to send for `samples` captured at `sample_rate`: the
    /// first call also carries the lead-in silence. Empty input sends nothing.
    pub fn chunk(&mut self, sample_rate: u32, samples: &[f32]) -> Option<(u32, Vec<f32>)> {
        if samples.is_empty() || sample_rate == 0 {
            return None;
        }
        match self.rate {
            Some(rate) => Some((rate, resample(samples, sample_rate, rate))),
            None => {
                self.rate = Some(sample_rate);
                let mut chunk = silence(sample_rate, LEAD_IN_SECONDS);
                chunk.extend_from_slice(samples);
                Some((sample_rate, chunk))
            }
        }
    }

    /// The tail silence, at the stream's rate (16 kHz if no audio was sent).
    pub fn tail(&self) -> (u32, Vec<f32>) {
        let rate = self.rate.unwrap_or(16_000);
        (rate, silence(rate, TAIL_SECONDS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chunk_uses_the_first_audio_rate() {
        let mut input = StreamInput::default();
        assert_eq!(input.chunk(48_000, &[]), None);
        assert_eq!(input.rate(), None);

        let (rate, first) = input.chunk(48_000, &[0.5; 10]).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(first.len(), 24_000 + 10);
        assert!(first[..24_000].iter().all(|sample| *sample == 0.0));

        let (rate, second) = input.chunk(48_000, &[0.5; 30]).unwrap();
        assert_eq!((rate, second.len()), (48_000, 30));

        // A different rate is converted, never passed through.
        let (rate, converted) = input.chunk(16_000, &[0.5; 10]).unwrap();
        assert_eq!((rate, converted.len()), (48_000, 30));

        let (rate, tail) = input.tail();
        assert_eq!((rate, tail.len()), (48_000, 38_400));
    }

    #[test]
    fn tail_without_audio_defaults_to_sixteen_kilohertz() {
        assert_eq!(StreamInput::default().tail().0, 16_000);
    }

    #[test]
    fn resampling_keeps_level_and_duration() {
        let out = resample(&[0.25; 160], 16_000, 48_000);
        assert_eq!(out.len(), 480);
        assert!(out.iter().all(|sample| (*sample - 0.25).abs() < 1e-6));
        assert_eq!(resample(&[0.1, 0.2], 44_100, 44_100), vec![0.1, 0.2]);
    }
}
