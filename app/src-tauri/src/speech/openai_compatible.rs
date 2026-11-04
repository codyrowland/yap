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
    language: Option<String>,
}

/// Upper bound for one request. Long toggle recordings can take a while to
/// upload and transcribe; cancellation is still checked around the request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

impl OpenAiCompatible {
    pub fn new(
        model_id: String,
        endpoint: &str,
        api_key_env: Option<&str>,
        language: Option<&str>,
    ) -> Result<Self, String> {
        let api_key = credential_store::read(&credential_store::remote_target(endpoint)?)?
            .or_else(|| api_key_env.and_then(|name| std::env::var(name).ok()))
            .filter(|value| !value.trim().is_empty());
        if api_key.is_none()
            && let Some(name) = api_key_env
        {
            return Err(format!(
                "Save an API key or set the {name} environment variable"
            ));
        }

        Self::with_key(model_id, endpoint, api_key, language)
    }

    pub fn cloud(
        model_id: String,
        provider: &str,
        base_url: &str,
        language: Option<&str>,
    ) -> Result<Self, String> {
        let details = super::cloud::provider(provider)
            .filter(|details| details.supported)
            .ok_or("This cloud provider is not supported yet")?;
        let endpoint = super::cloud::endpoint(provider, &model_id, base_url)?;
        let api_key = credential_store::read(&credential_store::cloud_target(provider, base_url)?)?
            .filter(|value| !value.trim().is_empty());
        // A custom server may not need a key; hosted providers always do.
        if api_key.is_none() && provider != "custom" {
            return Err(format!("Save a {} API key before dictating", details.label));
        }

        Self::with_key(model_id, &endpoint, api_key, language)
    }

    fn with_key(
        model_id: String,
        endpoint: &str,
        api_key: Option<String>,
        language: Option<&str>,
    ) -> Result<Self, String> {
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
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "Could not initialize transcription HTTP client".to_owned())?;
        Ok(Self {
            client,
            endpoint,
            model_id,
            api_key,
            language: language.map(str::to_owned),
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

impl OpenAiCompatible {
    /// Uploads audio and returns the parsed JSON body.
    fn post(
        &self,
        samples_16k_mono: &[f32],
        cancelled: &AtomicBool,
    ) -> Result<serde_json::Value, String> {
        let audio = multipart::Part::bytes(wav(samples_16k_mono)?)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|_| "Could not prepare audio upload".to_owned())?;
        let mut form = multipart::Form::new()
            .text("model", self.model_id.clone())
            .part("file", audio);
        if let Some(language) = &self.language {
            form = form.text("language", language.clone());
        }
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

        match response.status().as_u16() {
            200..=299 => {}
            401 | 403 => {
                return Err(format!(
                    "Transcription server rejected the credentials ({})",
                    response.status()
                ));
            }
            _ => {
                return Err(format!(
                    "Transcription server returned {}",
                    response.status()
                ));
            }
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

        serde_json::from_slice(&body).map_err(|_| "Invalid transcription response".to_owned())
    }

    /// Sends one second of silence to confirm the URL, credentials and model
    /// work. Returns the round-trip time. An empty transcript is fine here.
    pub fn check(&self) -> Result<Duration, String> {
        let started = std::time::Instant::now();
        let body = self.post(&[0.0; 16_000], &AtomicBool::new(false))?;
        if body.get("text").is_some_and(serde_json::Value::is_string) {
            Ok(started.elapsed())
        } else {
            Err("Invalid transcription response".to_owned())
        }
    }
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

        let text = self
            .post(samples_16k_mono, cancelled)?
            .get("text")
            .and_then(|text| text.as_str().map(str::to_owned))
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
        assert!(OpenAiCompatible::new("model".into(), "file:///tmp/audio", None, None).is_err());
        assert!(OpenAiCompatible::new("".into(), "https://example.com", None, None).is_err());
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
        let mut provider =
            OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
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
    fn sends_language_only_when_one_is_chosen() {
        let (endpoint, server) =
            server_response(200, b"{\"text\":\"hola\"}".to_vec(), Duration::ZERO);
        let mut provider =
            OpenAiCompatible::new("whisper-1".into(), &endpoint, None, Some("es")).unwrap();
        provider
            .transcribe(&[0.01; 1_600], &AtomicBool::new(false))
            .unwrap();
        let request = server.join().unwrap();
        let needle = b"name=\"language\"\r\n\r\nes";
        assert!(request.windows(needle.len()).any(|part| part == needle));

        let (endpoint, server) =
            server_response(200, b"{\"text\":\"hi\"}".to_vec(), Duration::ZERO);
        let mut provider =
            OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
        provider
            .transcribe(&[0.01; 1_600], &AtomicBool::new(false))
            .unwrap();
        let request = server.join().unwrap();
        assert!(!request.windows(15).any(|part| part == b"name=\"language\""));
    }

    #[test]
    fn connection_check_accepts_an_empty_transcript() {
        let (endpoint, server) = server_response(200, b"{\"text\":\"\"}".to_vec(), Duration::ZERO);
        let provider = OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
        assert!(provider.check().is_ok());
        server.join().unwrap();
        let (endpoint, server) = server_response(401, b"no".to_vec(), Duration::ZERO);
        let provider = OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
        assert!(provider.check().unwrap_err().contains("credentials"));
        server.join().unwrap();
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
            let mut provider =
                OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
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
        let mut provider =
            OpenAiCompatible::new("whisper-1".into(), &endpoint, None, None).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || provider.transcribe(&[0.01; 1_600], &cancel));
        std::thread::sleep(Duration::from_millis(30));
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(worker.join().unwrap().unwrap_err(), "Cancelled");
        server.join().unwrap();
    }
}
