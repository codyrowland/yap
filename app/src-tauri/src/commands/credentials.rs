use tauri::State;
use yap_app::credential_store;
use yap_app::settings::Settings;
use yap_app::speech::openai_compatible::OpenAiCompatible;

use crate::platform::credential_store as credentials;
use crate::state::{SessionState, SharedSettings, lock};

fn credential_target(kind: &str, provider: &str, settings: &Settings) -> Result<String, String> {
    match kind {
        "cloud" => credential_store::cloud_target(provider, &settings.custom_base_url),
        "remote" => {
            if settings.remote_endpoint.is_empty() {
                return Err("Enter a server URL first".to_owned());
            }
            credential_store::remote_target(&settings.remote_endpoint)
        }
        _ => Err("Unknown credential".to_owned()),
    }
}

#[tauri::command]
pub fn credential_status(
    kind: String,
    provider: String,
    settings: State<'_, SharedSettings>,
) -> Result<bool, String> {
    let target = credential_target(&kind, &provider, &settings.get())?;
    Ok(credentials().read(&target)?.is_some())
}

/// Saves (`Some`) or removes (`None`) a key in Windows Credential Manager.
#[tauri::command]
pub fn update_credential(
    kind: String,
    provider: String,
    secret: Option<String>,
    session: State<'_, SessionState>,
    settings: State<'_, SharedSettings>,
) -> Result<(), String> {
    let session = lock(&session)?;
    if !session.idle() {
        return Err("Cannot change credentials while dictating or exiting".to_owned());
    }

    let target = credential_target(&kind, &provider, &settings.get())?;
    match secret.map(|secret| secret.trim().to_owned()) {
        Some(secret) if !secret.is_empty() => credentials().write(&target, &secret),
        Some(_) => Err("Enter a key first".to_owned()),
        None => credentials().delete(&target),
    }
}

/// Sends a second of silence to the configured cloud provider or server.
#[tauri::command]
pub async fn test_connection(
    kind: String,
    settings: State<'_, SharedSettings>,
) -> Result<u64, String> {
    let settings = settings.get();
    tauri::async_runtime::spawn_blocking(move || {
        let provider = match kind.as_str() {
            "cloud" => OpenAiCompatible::cloud(
                settings.cloud_model(),
                &settings.cloud_provider,
                &settings.custom_base_url,
                None,
                credentials(),
            )?,
            "remote" => {
                if settings.remote_endpoint.is_empty() || settings.remote_model.trim().is_empty() {
                    return Err("Enter a server URL and model".to_owned());
                }
                OpenAiCompatible::new(
                    settings.remote_model.clone(),
                    &settings.remote_endpoint,
                    settings.remote_api_key_env.as_deref(),
                    None,
                    credentials(),
                )?
            }
            _ => return Err("Unknown engine".to_owned()),
        };
        provider.check().map(|elapsed| elapsed.as_millis() as u64)
    })
    .await
    .map_err(|error| error.to_string())?
}
