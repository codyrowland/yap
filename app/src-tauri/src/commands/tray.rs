use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use yap_app::coordinator::Phase;
use yap_app::models;
use yap_app::settings::{Engine, Settings};
use yap_app::speech::cloud;

use crate::config;
use crate::state::{SessionState, SharedSettings, lock};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineChoice {
    id: Engine,
    label: &'static str,
    detail: String,
    ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayState {
    status: &'static str,
    hold_key: String,
    toggle_key: String,
    engine: Engine,
    engines: Vec<EngineChoice>,
    update_version: Option<String>,
}

fn engine_choices(settings: &Settings) -> Vec<EngineChoice> {
    let local_detail = settings.local_model_path.as_deref().map(|path| {
        let path = std::path::Path::new(path);
        models::for_path(path).map_or_else(
            || {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            },
            models::display_name,
        )
    });
    let provider = cloud::provider(&settings.cloud_provider);
    let remote_host = reqwest::Url::parse(&settings.remote_endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    let ready = |engine| {
        Settings {
            engine,
            ..settings.clone()
        }
        .validate()
        .is_ok()
    };
    vec![
        EngineChoice {
            id: Engine::Local,
            label: "On-device",
            ready: local_detail.is_some(),
            detail: local_detail.unwrap_or_else(|| "No model selected".to_owned()),
        },
        EngineChoice {
            id: Engine::Cloud,
            label: "Cloud API",
            ready: ready(Engine::Cloud),
            detail: provider.map_or_else(
                || "Not set up".to_owned(),
                |provider| format!("{} · {}", provider.label, settings.cloud_model()),
            ),
        },
        EngineChoice {
            id: Engine::Remote,
            label: "Self-hosted",
            ready: ready(Engine::Remote),
            detail: remote_host.unwrap_or_else(|| "Not set up".to_owned()),
        },
    ]
}

#[tauri::command]
pub fn tray_state(
    session: State<'_, SessionState>,
    settings: State<'_, SharedSettings>,
    app: AppHandle,
) -> Result<TrayState, String> {
    let phase = lock(&session)?.coordinator.phase();
    let settings = settings.get();
    let status = match phase {
        Phase::Idle => "Ready",
        Phase::Recording => "Listening",
        Phase::Transcribing | Phase::Ready | Phase::Inserting => "Transcribing",
    };
    Ok(TrayState {
        status,
        hold_key: settings.hold_key.clone(),
        toggle_key: settings.toggle_key.clone(),
        engine: settings.engine,
        engines: engine_choices(&settings),
        update_version: crate::commands::update::pending_update(app),
    })
}

#[tauri::command]
pub fn set_engine(
    engine: Engine,
    session: State<'_, SessionState>,
    app: AppHandle,
) -> Result<(), String> {
    let settings = Settings::clone(&app.state::<SharedSettings>().get());
    let label = match engine {
        Engine::Local => "an on-device model",
        Engine::Cloud => "a cloud provider",
        Engine::Remote => "a self-hosted server",
    };
    let next = Settings { engine, ..settings };
    if engine == Engine::Local && next.local_model_path.is_none() {
        return Err(format!("Set up {label} in Settings first"));
    }
    next.validate()
        .map_err(|_| format!("Set up {label} in Settings first"))?;
    config::apply(&app, session.inner(), next).map(drop)
}

/// Opens the main window on Settings or History.
#[tauri::command]
pub fn open_window(page: String, app: AppHandle) -> Result<(), String> {
    show_main(&app, &page)
}

pub fn show_main(app: &AppHandle, page: &str) -> Result<(), String> {
    let tab = match page {
        "settings" => None,
        "history" => Some("history"),
        _ => return Err("Unknown page".to_owned()),
    };
    if let Some(tray) = app.get_webview_window(crate::tray::LABEL) {
        let _ = tray.hide();
    }
    let window = app
        .get_webview_window("main")
        .ok_or("Main window is missing")?;
    let onboarding = config::onboarding_path(app)?.is_file();
    if !onboarding {
        let current = window.url().map_err(|error| error.to_string())?;
        let on_settings = current.path().ends_with("index.html");
        let script = match (on_settings, tab) {
            (true, None) => None,
            (true, Some(tab)) => Some(format!("location.hash = '{tab}'")),
            (false, None) => Some("location.replace('index.html')".to_owned()),
            (false, Some(tab)) => Some(format!("location.replace('index.html#{tab}')")),
        };
        if let Some(script) = script {
            window.eval(script).map_err(|error| error.to_string())?;
        }
    }
    let _ = window.unminimize();
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::shutdown(&app);
}
