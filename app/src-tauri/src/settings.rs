//! Persisted user settings.
//!
//! Settings are stored as JSON (`settings.json`). Older builds wrote a
//! line-based `settings.txt` (versions 1–5); [`Settings::from_legacy`] imports
//! that format once so existing installs keep their microphone, model, keys and
//! engine. Secrets are never stored here: API keys and auth tokens live in the
//! system credential store.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::Family;
use crate::speech::{self, SpeechConfig, cloud};

pub const CURRENT_VERSION: u32 = 7;
pub const DEFAULT_HOLD_KEY: &str = "Control+Space";
pub const DEFAULT_TOGGLE_KEY: &str = "Control+Shift+Space";

/// Languages offered in Settings › General. `auto` lets the engine detect.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Local,
    Cloud,
    Remote,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Insertion {
    /// Put the text on the clipboard, press Ctrl+V, then restore the clipboard.
    #[default]
    Paste,
    /// Send the text as synthesized Unicode key presses.
    Type,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    pub microphone_index: usize,
    pub microphone_name: Option<String>,
    pub hold_key: String,
    pub toggle_key: String,
    pub engine: Engine,
    pub local_model_path: Option<String>,
    pub cloud_provider: String,
    /// Chosen model per cloud provider, so switching providers keeps choices.
    pub cloud_models: BTreeMap<String, String>,
    /// Base URL for the "Custom OpenAI-compatible API" cloud provider.
    pub custom_base_url: String,
    pub remote_endpoint: String,
    pub remote_model: String,
    /// Interim fallback: read the self-hosted token from this environment
    /// variable when nothing is saved in the credential store.
    pub remote_api_key_env: Option<String>,
    pub insertion: Insertion,
    pub language: String,
    pub show_overlay: bool,
    /// Only the sound waves while recording; errors still show.
    pub minimal_overlay: bool,
    pub open_at_login: bool,
    pub mute_while_recording: bool,
    /// Tidy each transcript with a local language model before inserting.
    pub cleanup_enabled: bool,
    /// Which clean-up model (`cleanup::MODELS`).
    pub cleanup_model: String,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            microphone_index: 0,
            microphone_name: None,
            hold_key: DEFAULT_HOLD_KEY.to_owned(),
            toggle_key: DEFAULT_TOGGLE_KEY.to_owned(),
            engine: Engine::Local,
            local_model_path: None,
            cloud_provider: "openai".to_owned(),
            cloud_models: BTreeMap::new(),
            custom_base_url: String::new(),
            remote_endpoint: String::new(),
            remote_model: String::new(),
            remote_api_key_env: None,
            insertion: Insertion::Paste,
            language: "auto".to_owned(),
            show_overlay: true,
            minimal_overlay: false,
            open_at_login: false,
            mute_while_recording: true,
            cleanup_enabled: false,
            cleanup_model: "qwen3.5-2b".to_owned(),
            theme: Theme::Dark,
        }
    }
}

fn valid_http_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

impl Settings {
    /// The model selected for the current cloud provider, falling back to the
    /// provider's first listed model.
    pub fn cloud_model(&self) -> String {
        self.cloud_models
            .get(&self.cloud_provider)
            .cloned()
            .filter(|model| !model.trim().is_empty())
            .or_else(|| {
                cloud::provider(&self.cloud_provider)
                    .and_then(|provider| provider.models.first())
                    .map(|model| (*model).to_owned())
            })
            .unwrap_or_default()
    }

    /// `None` means let the engine detect the language.
    pub fn language_code(&self) -> Option<String> {
        (self.language != "auto").then(|| self.language.clone())
    }

    /// The engine configuration for one dictation with these settings.
    pub fn speech_config(&self) -> Result<SpeechConfig, String> {
        match self.engine {
            Engine::Local => {
                let artifact = PathBuf::from(self.local_model_path.clone().unwrap_or_default());
                if artifact.as_os_str().is_empty() {
                    return Err("Select an on-device model before dictating".to_owned());
                }
                let model = crate::models::for_path(&artifact);
                let model_id = model.map_or_else(
                    || {
                        artifact
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "unselected".to_owned())
                    },
                    |model| model.id.to_owned(),
                );
                let adapter = match model.map(|model| model.family) {
                    Some(Family::Parakeet | Family::Moonshine) => speech::SHERPA_OFFLINE,
                    Some(Family::Nemotron) => speech::SHERPA_STREAMING,
                    // Unknown files are treated as whisper.cpp models, as before.
                    Some(Family::Whisper) | None => speech::WHISPER_CPP,
                };
                Ok(SpeechConfig::Local {
                    adapter: adapter.to_owned(),
                    model_id,
                    artifact,
                })
            }
            Engine::Cloud => Ok(SpeechConfig::Cloud {
                provider: self.cloud_provider.clone(),
                model_id: self.cloud_model(),
                base_url: self.custom_base_url.clone(),
            }),
            Engine::Remote => {
                if self.remote_endpoint.trim().is_empty() || self.remote_model.trim().is_empty() {
                    return Err("Enter a server URL and model".to_owned());
                }
                Ok(SpeechConfig::OpenAiCompatible {
                    model_id: self.remote_model.clone(),
                    endpoint: self.remote_endpoint.clone(),
                    api_key_env: self.remote_api_key_env.clone(),
                })
            }
        }
    }

    /// Checks values that do not depend on the platform. Talk-key syntax is
    /// validated by `talk_keys`, with the platform's reserved shortcuts.
    pub fn validate(&self) -> Result<(), String> {
        if !LANGUAGES.iter().any(|(code, _)| *code == self.language) {
            return Err("Unsupported spoken language".to_owned());
        }

        if crate::cleanup::model(&self.cleanup_model).is_none() {
            return Err("Unknown clean-up model".to_owned());
        }

        let provider = cloud::provider(&self.cloud_provider).ok_or("Unknown cloud provider")?;
        for (id, model) in &self.cloud_models {
            if cloud::provider(id).is_none() || model.contains(['\r', '\n']) {
                return Err("Invalid cloud model setting".to_owned());
            }
        }

        if !self.custom_base_url.is_empty() && !valid_http_url(&self.custom_base_url) {
            return Err("Enter a valid base URL for the custom API".to_owned());
        }

        if !self.remote_endpoint.is_empty() && !valid_http_url(&self.remote_endpoint) {
            return Err("Enter a valid server URL".to_owned());
        }

        match self.engine {
            Engine::Local => {}
            Engine::Cloud => {
                if !provider.supported {
                    return Err(format!("{} is not supported yet", provider.label));
                }
                if self.cloud_model().trim().is_empty() {
                    return Err("Select a transcription model".to_owned());
                }
                if provider.id == "custom" && self.custom_base_url.is_empty() {
                    return Err("Enter a base URL for the custom API".to_owned());
                }
            }
            Engine::Remote => {
                if self.remote_endpoint.is_empty() || self.remote_model.trim().is_empty() {
                    return Err("Enter a server URL and model".to_owned());
                }
            }
        }

        Ok(())
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| error.to_string())
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let mut settings: Self =
            serde_json::from_str(text).map_err(|_| "Invalid Yap settings file".to_owned())?;
        if settings.version > CURRENT_VERSION {
            return Err("Settings were written by a newer version of Yap".to_owned());
        }

        if settings.version < 7 {
            settings.migrate_talk_keys();
        }
        settings.version = CURRENT_VERSION;
        settings.validate()?;
        Ok(settings)
    }

    /// Version 7 saves talk keys under W3C key names. Older names are
    /// converted; if either key has no portable name, both go back to the
    /// defaults so they cannot clash.
    fn migrate_talk_keys(&mut self) {
        match (
            crate::talk_keys::migrate_shortcut(&self.hold_key),
            crate::talk_keys::migrate_shortcut(&self.toggle_key),
        ) {
            (Some(hold), Some(toggle)) => {
                self.hold_key = hold;
                self.toggle_key = toggle;
            }
            _ => {
                self.hold_key = DEFAULT_HOLD_KEY.to_owned();
                self.toggle_key = DEFAULT_TOGGLE_KEY.to_owned();
            }
        }
    }

    /// Imports the line-based `settings.txt` written by earlier builds.
    pub fn from_legacy(text: &str) -> Result<Self, String> {
        let lines = text.lines().collect::<Vec<_>>();
        let field = |line: &str, name: &str| -> Result<String, String> {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_owned)
                .ok_or_else(|| format!("Invalid {name} setting"))
        };
        let mut settings = Self::default();
        let (hold, toggle, index, name, model, provider, endpoint, remote_model, key_env, cloud) =
            match lines.as_slice() {
                [
                    version @ ("Yap settings v4" | "Yap settings v5"),
                    hold,
                    toggle,
                    index,
                    name,
                    model,
                    provider,
                    endpoint,
                    remote_model,
                    key_env,
                    rest @ ..,
                ] => {
                    let cloud = match (*version, rest) {
                        ("Yap settings v4", []) => None,
                        ("Yap settings v5", [value]) => Some(field(value, "cloud_model")?),
                        _ => return Err("Invalid Yap settings file".to_owned()),
                    };
                    (
                        *hold,
                        Some(field(toggle, "toggle_shortcut")?),
                        *index,
                        *name,
                        *model,
                        Some(*provider),
                        Some(*endpoint),
                        Some(*remote_model),
                        Some(*key_env),
                        cloud,
                    )
                }
                [
                    "Yap settings v3",
                    hold,
                    index,
                    name,
                    model,
                    provider,
                    endpoint,
                    remote_model,
                    key_env,
                ] => (
                    *hold,
                    None,
                    *index,
                    *name,
                    *model,
                    Some(*provider),
                    Some(*endpoint),
                    Some(*remote_model),
                    Some(*key_env),
                    None,
                ),
                ["Yap settings v2", hold, index, name, model] => (
                    *hold, None, *index, *name, *model, None, None, None, None, None,
                ),
                ["Yap settings v1", hold, index, name, model, copy_only]
                    if matches!(copy_only.strip_prefix("copy_only="), Some("true" | "false")) =>
                {
                    (
                        *hold, None, *index, *name, *model, None, None, None, None, None,
                    )
                }
                _ => return Err("Invalid Yap settings file".to_owned()),
            };

        settings.hold_key = field(hold, "shortcut")?;
        if let Some(toggle) = toggle {
            settings.toggle_key = toggle;
        }
        settings.migrate_talk_keys();
        settings.microphone_index = field(index, "microphone_index")?
            .parse()
            .map_err(|_| "Invalid microphone setting")?;
        settings.microphone_name = Some(field(name, "microphone_name")?).filter(|n| !n.is_empty());
        settings.local_model_path = Some(field(model, "model_path")?).filter(|p| !p.is_empty());
        if let Some(endpoint) = endpoint {
            settings.remote_endpoint = field(endpoint, "endpoint")?;
        }
        if let Some(remote_model) = remote_model {
            settings.remote_model = field(remote_model, "remote_model")?;
        }
        if let Some(key_env) = key_env {
            settings.remote_api_key_env =
                Some(field(key_env, "api_key_env")?).filter(|v| !v.is_empty());
        }
        if let Some(cloud) = cloud {
            settings.cloud_models.insert("openai".to_owned(), cloud);
        }
        settings.engine = match provider
            .map(|line| field(line, "provider"))
            .transpose()?
            .as_deref()
        {
            None | Some("local") => Engine::Local,
            Some("openai-compatible") => Engine::Remote,
            Some("openai") => Engine::Cloud,
            Some(_) => return Err("Invalid speech provider".to_owned()),
        };
        // A legacy remote engine with incomplete fields was never usable; keep
        // the values but fall back to on-device so the file still loads.
        if settings.validate().is_err() {
            settings.engine = Engine::Local;
        }
        settings.validate()?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_keeps_every_field_and_no_secrets() {
        let mut settings = Settings {
            microphone_index: 3,
            microphone_name: Some("USB microphone".into()),
            local_model_path: Some(r"C:\Users\Test\models\ggml-base.bin".into()),
            hold_key: "F9".into(),
            engine: Engine::Cloud,
            cloud_provider: "groq".into(),
            insertion: Insertion::Type,
            language: "de".into(),
            show_overlay: false,
            ..Settings::default()
        };
        settings
            .cloud_models
            .insert("groq".into(), "whisper-large-v3".into());
        let text = settings.to_json().unwrap();
        assert!(text.contains("\"holdKey\": \"F9\""));
        assert!(!text.to_ascii_lowercase().contains("secret"));
        assert_eq!(Settings::from_json(&text).unwrap(), settings);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let settings = Settings::from_json(r#"{"version":6,"holdKey":"F8"}"#).unwrap();
        assert_eq!(settings.hold_key, "F8");
        assert_eq!(settings.toggle_key, DEFAULT_TOGGLE_KEY);
        assert_eq!(settings.insertion, Insertion::Paste);
        assert!(settings.show_overlay);
        assert!(!settings.minimal_overlay);
        assert!(settings.mute_while_recording);
        assert!(!settings.cleanup_enabled);
        assert_eq!(settings.cleanup_model, "qwen3.5-2b");
        assert_eq!(settings.theme, Theme::Dark);
    }

    #[test]
    fn version_6_talk_keys_move_to_w3c_names() {
        let settings =
            Settings::from_json(r#"{"version":6,"holdKey":"Right Alt","toggleKey":"Ctrl+L"}"#)
                .unwrap();
        assert_eq!(settings.version, CURRENT_VERSION);
        assert_eq!(settings.hold_key, "AltRight");
        assert_eq!(settings.toggle_key, "Control+KeyL");
        let settings =
            Settings::from_json(r#"{"version":6,"holdKey":"VK-92","toggleKey":"F8"}"#).unwrap();
        assert_eq!(settings.hold_key, DEFAULT_HOLD_KEY);
        assert_eq!(settings.toggle_key, DEFAULT_TOGGLE_KEY);
        let settings = Settings::from_json(r#"{"version":7,"holdKey":"Right Alt"}"#).unwrap();
        assert_eq!(settings.hold_key, "Right Alt");
    }

    #[test]
    fn rejects_newer_versions_and_invalid_values() {
        assert!(Settings::from_json(r#"{"version":99}"#).is_err());
        assert!(Settings::from_json("not json").is_err());
        assert!(Settings::from_json(r#"{"language":"xx"}"#).is_err());
        assert!(Settings::from_json(r#"{"engine":"remote"}"#).is_err());
        assert!(Settings::from_json(r#"{"remoteEndpoint":"file:///etc"}"#).is_err());
        assert!(Settings::from_json(r#"{"cleanupModel":"gpt-9"}"#).is_err());
    }

    #[test]
    fn cloud_model_defaults_to_the_providers_first_model() {
        let settings = Settings {
            cloud_provider: "groq".into(),
            ..Settings::default()
        };
        assert_eq!(settings.cloud_model(), "whisper-large-v3-turbo");
    }

    #[test]
    fn unsupported_cloud_provider_cannot_be_the_active_engine() {
        let settings = Settings {
            engine: Engine::Cloud,
            cloud_provider: "deepgram".into(),
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
        let custom = Settings {
            engine: Engine::Cloud,
            cloud_provider: "custom".into(),
            ..Settings::default()
        };
        assert!(custom.validate().is_err());
    }

    #[test]
    fn legacy_v1_imports_without_copy_only() {
        for value in ["true", "false"] {
            let old = format!(
                "Yap settings v1\nshortcut=F9\nmicrophone_index=3\nmicrophone_name=USB microphone\nmodel_path=model.bin\ncopy_only={value}\n"
            );
            let settings = Settings::from_legacy(&old).unwrap();
            assert_eq!(settings.microphone_index, 3);
            assert_eq!(settings.microphone_name.as_deref(), Some("USB microphone"));
            assert_eq!(settings.local_model_path.as_deref(), Some("model.bin"));
            assert_eq!(settings.engine, Engine::Local);
            assert_eq!(settings.hold_key, "F9");
            assert_eq!(settings.toggle_key, DEFAULT_TOGGLE_KEY);
        }
    }

    #[test]
    fn legacy_v3_keeps_the_hold_key_and_remote_server() {
        let old = "Yap settings v3\nshortcut=F9\nmicrophone_index=0\nmicrophone_name=Mic\nmodel_path=\nprovider=openai-compatible\nendpoint=https://asr.home.lan/v1/audio/transcriptions\nremote_model=large-v3\napi_key_env=ASR_TOKEN\n";
        let settings = Settings::from_legacy(old).unwrap();
        assert_eq!(settings.hold_key, "F9");
        assert_eq!(settings.engine, Engine::Remote);
        assert_eq!(settings.remote_model, "large-v3");
        assert_eq!(settings.remote_api_key_env.as_deref(), Some("ASR_TOKEN"));
    }

    #[test]
    fn legacy_v5_imports_openai_cloud_model() {
        let old = "Yap settings v5\nshortcut=Ctrl+Space\ntoggle_shortcut=Ctrl+Alt+D\nmicrophone_index=1\nmicrophone_name=Mic\nmodel_path=\nprovider=openai\nendpoint=\nremote_model=\napi_key_env=\ncloud_model=whisper-1\n";
        let settings = Settings::from_legacy(old).unwrap();
        assert_eq!(settings.engine, Engine::Cloud);
        assert_eq!(settings.cloud_provider, "openai");
        assert_eq!(settings.cloud_model(), "whisper-1");
        assert_eq!(settings.hold_key, "Control+Space");
        assert_eq!(settings.toggle_key, "Control+Alt+KeyD");
    }

    #[test]
    fn legacy_incomplete_remote_falls_back_to_local() {
        let old = "Yap settings v3\nshortcut=F9\nmicrophone_index=0\nmicrophone_name=Mic\nmodel_path=\nprovider=openai-compatible\nendpoint=\nremote_model=\napi_key_env=\n";
        assert_eq!(Settings::from_legacy(old).unwrap().engine, Engine::Local);
    }

    #[test]
    fn legacy_rejects_truncated_files() {
        assert!(Settings::from_legacy("Yap settings v1\nshortcut=F1").is_err());
        assert!(Settings::from_legacy("").is_err());
    }

    #[test]
    fn speech_config_follows_the_selected_engine() {
        let mut settings = Settings::default();
        assert!(settings.speech_config().is_err());
        settings.local_model_path = Some(r"C:\models\ggml-base.bin".into());
        assert!(matches!(
            settings.speech_config(),
            Ok(SpeechConfig::Local { model_id, adapter, .. })
                if model_id == "base" && adapter == "whisper.cpp"
        ));
        settings.local_model_path = Some(r"C:\models\parakeet-tdt-0.6b-v3".into());
        assert!(matches!(
            settings.speech_config(),
            Ok(SpeechConfig::Local { model_id, adapter, .. })
                if model_id == "parakeet-tdt-0.6b-v3" && adapter == "sherpa-onnx"
        ));
        settings.local_model_path = Some(r"C:\models\my-finetune.bin".into());
        assert!(matches!(
            settings.speech_config(),
            Ok(SpeechConfig::Local { adapter, .. }) if adapter == "whisper.cpp"
        ));
        settings.engine = Engine::Cloud;
        settings.cloud_provider = "groq".into();
        assert!(matches!(
            settings.speech_config(),
            Ok(SpeechConfig::Cloud { provider, model_id, .. })
                if provider == "groq" && model_id == "whisper-large-v3-turbo"
        ));
        settings.engine = Engine::Remote;
        assert!(settings.speech_config().is_err());
        settings.remote_endpoint = "https://asr.home.lan/v1/audio/transcriptions".into();
        settings.remote_model = "large-v3".into();
        assert!(matches!(
            settings.speech_config(),
            Ok(SpeechConfig::OpenAiCompatible { .. })
        ));
    }
}
