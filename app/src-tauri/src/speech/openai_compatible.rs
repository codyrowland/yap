use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Client, multipart};

use super::{SpeechProvider, Transcript};
use crate::credential_store;

const MAX_RESPONSE_BYTES: u64 = 1_048_576;

pub struct OpenAiCompatible {
    client: Client,
    endpoint: Url,
    model_id: String,
    api_key: Option<String>,
}

impl OpenAiCompatible {
    pub fn new(
        model_id: String,
        endpoint: &str,
        api_key_env: Option<&str>,
    ) -> Result<Self, String> {
        let api_key = credential_store::read(&credential_store::remote_target(endpoint)?)?
            .or_else(|| api_key_env.and_then(|name| std::env::var(name).ok()))
            .filter(|value| !value.trim().is_empty());
        if api_key.is_none() {
            if let Some(name) = api_key_env {
                return Err(format!(
                    "Save an API key or set the {name} environment variable"
                ));
            }
        }

        Self::with_key(model_id, endpoint, api_key)
    }

    pub fn cloud(model_id: String, provider: &str) -> Result<Self, String> {
        let target = credential_store::cloud_target(provider)?;
        let api_key = credential_store::read(target)?
            .filter(|value| !value.trim().is_empty())
            .ok_or("Save an OpenAI API key before dictating")?;
        Self::with_key(
            model_id,
            "https://api.openai.com/v1/audio/transcriptions",
            Some(api_key),
        )
    }

    fn with_key(model_id: String, endpoint: &str, api_key: Option<String>) -> Result<Self, String> {
        if model_id.trim().is_empty() {
            return Err("Select a transcription model".to_owned());
        }

        let endpoint = Url::parse(endpoint).map_err(|_| "Invalid transcription URL".to_owned())?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err("Invalid transcription URL".to_owned());
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(25))
            .build()
            .map_err(|_| "Could not initialize transcription HTTP client".to_owned())?;
        Ok(Self {
            client,
            endpoint,
            model_id,
            api_key,
        })
    }
}

fn wav(samples: &[f32]) -> Result<Vec<u8>, String> {
    let data_bytes = samples
        .len()
        .checked_mul(2)
        .and_then(|len| u32::try_from(len).ok())
        .filter(|len| *len <= u32::MAX - 36)
        .ok_or("Audio is too long")?;
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&32_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        let sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        bytes.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
    }

    Ok(bytes)
}

impl SpeechProvider for OpenAiCompatible {
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

        let audio = multipart::Part::bytes(wav(samples_16k_mono)?)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|_| "Could not prepare audio upload".to_owned())?;
        let form = multipart::Form::new()
            .text("model", self.model_id.clone())
            .part("file", audio);
        let mut request = self.client.post(self.endpoint.clone()).multipart(form);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let response = request
            .send()
            .map_err(|_| "Transcription server request failed or timed out".to_owned())?;
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        if !response.status().is_success() {
            return Err(format!(
                "Transcription server returned {}",
                response.status()
            ));
        }

        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|_| "Could not read transcription response".to_owned())?;
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("Transcription response is too large".to_owned());
        }

        let text = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| value.get("text")?.as_str().map(str::to_owned))
            .filter(|text| !text.trim().is_empty())
            .ok_or("Invalid transcription response")?;
        Ok(Transcript {
            text,
            model_id: self.model_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn wav_encodes_sixteen_kilohertz_mono() {
        let bytes = wav(&[0.0, 1.0]).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[24..28], &16_000_u32.to_le_bytes());
        assert_eq!(&bytes[40..44], &4_u32.to_le_bytes());
        assert_eq!(bytes.len(), 48);
    }

    #[test]
    fn rejects_invalid_endpoints_and_missing_model() {
        assert!(OpenAiCompatible::new("model".into(), "file:///tmp/audio", None).is_err());
        assert!(OpenAiCompatible::new("".into(), "https://example.com", None).is_err());
    }

    fn server_response(
        status: u16,
        body: Vec<u8>,
        delay: Duration,
    ) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/audio/transcriptions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = connection.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= header_end + length {
                    break;
                }
            }

            std::thread::sleep(delay);
            write!(
                connection,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            connection.write_all(&body).unwrap();
            request
        });
        (endpoint, server)
    }

    #[test]
    fn posts_multipart_audio_and_parses_transcript() {
        let (endpoint, server) =
            server_response(200, b"{\"text\":\"hello\"}".to_vec(), Duration::ZERO);
        let mut provider = OpenAiCompatible::new("whisper-1".into(), &endpoint, None).unwrap();
        let transcript = provider
            .transcribe(&[0.01; 1_600], &AtomicBool::new(false))
            .unwrap();
        assert_eq!(transcript.text, "hello");
        let request = server.join().unwrap();
        assert!(request.starts_with(b"POST /v1/audio/transcriptions HTTP/1.1"));
        assert!(
            request
                .windows(b"name=\"model\"".len())
                .any(|part| part == b"name=\"model\"")
        );
        assert!(
            request
                .windows(b"audio.wav".len())
                .any(|part| part == b"audio.wav")
        );
        assert!(request.windows(4).any(|part| part == b"RIFF"));
    }

    #[test]
    fn rejects_authentication_errors_and_invalid_responses() {
        for (status, body, expected) in [
            (401, b"unauthorized".to_vec(), "401"),
            (
                200,
                b"{\"other\":\"value\"}".to_vec(),
                "Invalid transcription response",
            ),
            (
                200,
                vec![b'x'; MAX_RESPONSE_BYTES as usize + 1],
                "too large",
            ),
        ] {
            let (endpoint, server) = server_response(status, body, Duration::ZERO);
            let mut provider = OpenAiCompatible::new("whisper-1".into(), &endpoint, None).unwrap();
            let error = provider
                .transcribe(&[0.01; 1_600], &AtomicBool::new(false))
                .unwrap_err();
            assert!(error.contains(expected), "{error}");
            server.join().unwrap();
        }
    }

    #[test]
    fn rejects_result_after_cancellation() {
        let (endpoint, server) = server_response(
            200,
            b"{\"text\":\"late\"}".to_vec(),
            Duration::from_millis(100),
        );
        let mut provider = OpenAiCompatible::new("whisper-1".into(), &endpoint, None).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || provider.transcribe(&[0.01; 1_600], &cancel));
        std::thread::sleep(Duration::from_millis(30));
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(worker.join().unwrap().unwrap_err(), "Cancelled");
        server.join().unwrap();
    }
}
