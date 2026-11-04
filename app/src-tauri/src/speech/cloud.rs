//! Hosted transcription providers shown in Settings › Cloud API.
//!
//! Providers marked `supported` speak the OpenAI transcription protocol
//! (multipart WAV upload, JSON `text` response) and work today. The rest are
//! listed so the UI matches the design, but cannot be selected until they get
//! protocol-specific adapters.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub id: &'static str,
    pub label: &'static str,
    /// `secret` (credential store), `text` (settings), or `file`.
    pub kind: &'static str,
    pub placeholder: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub supported: bool,
    /// Empty means the model is typed in (custom API).
    pub models: &'static [&'static str],
    pub fields: &'static [Field],
}

const API_KEY: Field = Field {
    id: "apiKey",
    label: "API key",
    kind: "secret",
    placeholder: "",
};

const fn key(placeholder: &'static str) -> Field {
    Field {
        placeholder,
        ..API_KEY
    }
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "openai",
        label: "OpenAI",
        supported: true,
        models: &["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "whisper-1"],
        fields: &[key("sk-…")],
    },
    Provider {
        id: "deepgram",
        label: "Deepgram",
        supported: false,
        models: &["nova-3", "nova-2"],
        fields: &[key("dg_…")],
    },
    Provider {
        id: "assemblyai",
        label: "AssemblyAI",
        supported: false,
        models: &["universal", "slam-1"],
        fields: &[API_KEY],
    },
    Provider {
        id: "groq",
        label: "Groq",
        supported: true,
        models: &["whisper-large-v3-turbo", "whisper-large-v3"],
        fields: &[key("gsk_…")],
    },
    Provider {
        id: "elevenlabs",
        label: "ElevenLabs",
        supported: false,
        models: &["scribe_v1"],
        fields: &[API_KEY],
    },
    Provider {
        id: "google",
        label: "Google Cloud Speech-to-Text",
        supported: false,
        models: &["chirp_2", "latest_long", "latest_short"],
        fields: &[
            Field {
                id: "serviceAccount",
                label: "Service account",
                kind: "file",
                placeholder: "",
            },
            Field {
                id: "projectId",
                label: "Project ID",
                kind: "text",
                placeholder: "my-project",
            },
        ],
    },
    Provider {
        id: "azure",
        label: "Azure AI Speech",
        supported: false,
        models: &["default"],
        fields: &[
            Field {
                id: "apiKey",
                label: "Subscription key",
                kind: "secret",
                placeholder: "",
            },
            Field {
                id: "region",
                label: "Region",
                kind: "text",
                placeholder: "eastus",
            },
        ],
    },
    Provider {
        id: "aws",
        label: "AWS Transcribe",
        supported: false,
        models: &["standard"],
        fields: &[
            Field {
                id: "accessKeyId",
                label: "Access key ID",
                kind: "text",
                placeholder: "AKIA…",
            },
            Field {
                id: "apiKey",
                label: "Secret access key",
                kind: "secret",
                placeholder: "",
            },
            Field {
                id: "region",
                label: "Region",
                kind: "text",
                placeholder: "us-east-1",
            },
        ],
    },
    Provider {
        id: "speechmatics",
        label: "Speechmatics",
        supported: false,
        models: &["enhanced", "standard"],
        fields: &[API_KEY],
    },
    Provider {
        id: "fireworks",
        label: "Fireworks AI",
        supported: true,
        models: &["whisper-v3-turbo", "whisper-v3"],
        fields: &[key("fw_…")],
    },
    Provider {
        id: "custom",
        label: "Custom OpenAI-compatible API",
        supported: true,
        models: &[],
        fields: &[
            Field {
                id: "baseUrl",
                label: "Base URL",
                kind: "text",
                placeholder: "https://api.example.com/v1",
            },
            API_KEY,
        ],
    },
];

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The transcription URL for a supported provider and model. `custom_base_url`
/// is only used by the custom provider.
pub fn endpoint(provider: &str, model: &str, custom_base_url: &str) -> Result<String, String> {
    match provider {
        "openai" => Ok("https://api.openai.com/v1/audio/transcriptions".to_owned()),
        "groq" => Ok("https://api.groq.com/openai/v1/audio/transcriptions".to_owned()),
        // Fireworks serves each Whisper size from its own host.
        "fireworks" if model == "whisper-v3-turbo" => Ok(
            "https://audio-turbo.us-virginia-1.direct.fireworks.ai/v1/audio/transcriptions"
                .to_owned(),
        ),
        "fireworks" => Ok(
            "https://audio-prod.us-virginia-1.direct.fireworks.ai/v1/audio/transcriptions"
                .to_owned(),
        ),
        "custom" if !custom_base_url.trim().is_empty() => Ok(format!(
            "{}/audio/transcriptions",
            custom_base_url.trim().trim_end_matches('/')
        )),
        "custom" => Err("Enter a base URL for the custom API".to_owned()),
        _ => Err("This cloud provider is not supported yet".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_eleven_designed_providers_with_unique_ids() {
        assert_eq!(PROVIDERS.len(), 11);
        let mut ids = PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 11);
    }

    #[test]
    fn every_supported_provider_has_an_endpoint() {
        for provider in PROVIDERS.iter().filter(|p| p.supported) {
            let model = provider.models.first().copied().unwrap_or("m");
            assert!(
                endpoint(provider.id, model, "https://api.example.com/v1/").is_ok(),
                "{}",
                provider.id
            );
        }
        assert!(endpoint("deepgram", "nova-3", "").is_err());
    }

    #[test]
    fn custom_base_url_gets_the_transcriptions_path_once() {
        assert_eq!(
            endpoint("custom", "m", "https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/audio/transcriptions"
        );
        assert!(endpoint("custom", "m", " ").is_err());
    }
}
