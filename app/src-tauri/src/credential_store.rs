/// Where API keys are kept. Each OS implements this with its own secret
/// store; tests use an in-memory one.
pub trait CredentialStore: Send + Sync {
    fn read(&self, target: &str) -> Result<Option<String>, String>;
    fn write(&self, target: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, target: &str) -> Result<(), String>;
}

pub fn remote_target(endpoint: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| "Invalid transcription URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Invalid transcription URL".to_owned());
    }

    Ok(format!("Yap/transcription/{url}"))
}

/// One credential per cloud provider. The custom API's key is tied to its
/// base URL so switching URLs never sends an old key to a new host.
pub fn cloud_target(provider: &str, custom_base_url: &str) -> Result<String, String> {
    match crate::speech::cloud::provider(provider) {
        Some(provider) if provider.id == "custom" => {
            let base = remote_target(custom_base_url.trim())
                .map_err(|_| "Enter a valid base URL for the custom API".to_owned())?;
            Ok(base.replacen("Yap/transcription/", "Yap/cloud/custom/", 1))
        }
        Some(provider) => Ok(format!("Yap/cloud/{}", provider.id)),
        None => Err("Unsupported cloud provider".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_per_provider_and_per_url() {
        assert!(remote_target("file:///tmp/audio").is_err());
        assert_eq!(cloud_target("openai", "").unwrap(), "Yap/cloud/openai");
        assert_eq!(cloud_target("groq", "").unwrap(), "Yap/cloud/groq");
        assert!(cloud_target("unknown", "").is_err());
        assert!(cloud_target("custom", "").is_err());
        assert_ne!(
            cloud_target("custom", "https://a.example/v1").unwrap(),
            cloud_target("custom", "https://b.example/v1").unwrap()
        );
        assert_ne!(
            remote_target("https://a.example/v1/audio/transcriptions").unwrap(),
            remote_target("https://b.example/v1/audio/transcriptions").unwrap()
        );
    }
}
