//! Runs the clean-up model: a `llama-server` helper process bound to
//! 127.0.0.1 with a random API key, tied to Yap (`platform::HelperProcess`)
//! so it is killed if Yap exits or crashes. Started in the background when
//! clean-up is turned on (or at launch), stopped when it is turned off.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use yap_app::cleanup::{self, CleanupModel};
use yap_app::settings::Settings;

use crate::config;
use crate::platform::{self, HelperProcess};

const START_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    Off,
    Starting,
    Ready,
    Error { message: String },
}

struct Server {
    model_id: &'static str,
    process: HelperProcess,
    port: u16,
    key: String,
    ready: bool,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static STATUS: Mutex<Status> = Mutex::new(Status::Off);

fn set_status(app: &AppHandle, status: Status) {
    crate::logfile::write(&format!("clean-up: {status:?}"));
    if let Ok(mut current) = STATUS.lock() {
        *current = status.clone();
    }
    let _ = app.emit("cleanup-status", status);
}

pub fn status() -> Status {
    STATUS
        .lock()
        .map(|status| status.clone())
        .unwrap_or(Status::Off)
}

fn random_key() -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    hasher.update(std::process::id().to_le_bytes());
    let local = 0_u8;
    hasher.update((&local as *const u8 as usize).to_le_bytes());
    format!("{:x}", hasher.finalize())
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|_| "No free local port for clean-up".to_owned())
}

fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().saturating_sub(2).clamp(1, 8))
        .unwrap_or(2)
}

fn spawn(model: &'static CleanupModel, models_dir: &Path) -> Result<Server, String> {
    let runtime = cleanup::runtime_dir(models_dir);
    let port = free_port()?;
    let key = random_key();
    let log = File::create(cleanup::directory(models_dir).join("server.log")).ok();
    let mut command = Command::new(runtime.join(cleanup::SERVER_EXE));
    command
        .current_dir(&runtime)
        .arg("-m")
        .arg(cleanup::model_path(model, models_dir))
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["--api-key", &key])
        .args([
            "-c",
            "4096",
            "-np",
            "1",
            "-t",
            &threads().to_string(),
            "--jinja",
            // Clean-up needs no reasoning; end any "thinking" immediately.
            "--reasoning-budget",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map_or_else(Stdio::null, Stdio::from));
    let process = HelperProcess::spawn(command)
        .map_err(|error| format!("Could not start clean-up: {error}"))?;
    Ok(Server {
        model_id: model.id,
        process,
        port,
        key,
        ready: false,
    })
}

/// Waits until the server has loaded the model (`/health` returns 200).
fn wait_ready(port: u16, key: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        {
            let mut server = SERVER.lock().map_err(|error| error.to_string())?;
            let Some(server) = server.as_mut().filter(|server| server.port == port) else {
                return Err("Cancelled".to_owned());
            };
            if let Ok(Some(exit)) = server.process.try_wait() {
                return Err(if let Some(runtime) = platform::missing_runtime(&exit) {
                    format!("Clean-up needs {runtime}. Install it, then turn clean-up on again.")
                } else {
                    format!(
                        "Clean-up stopped while loading ({exit}); see server.log in the models folder"
                    )
                });
            }
        }
        if client
            .get(format!("http://127.0.0.1:{port}/health"))
            .bearer_auth(key)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("Clean-up model took too long to load".to_owned())
}

pub fn stop() {
    if let Ok(mut server) = SERVER.lock() {
        *server = None;
    }
}

/// Starts, restarts or stops the server to match `settings`.
pub fn sync(app: &AppHandle, settings: &Settings) {
    let wanted = settings
        .cleanup_enabled
        .then(|| cleanup::model(&settings.cleanup_model))
        .flatten();
    let Some(model) = wanted else {
        stop();
        set_status(app, Status::Off);
        return;
    };
    if SERVER.lock().is_ok_and(|server| {
        server
            .as_ref()
            .is_some_and(|server| server.model_id == model.id)
    }) {
        return;
    }
    let Ok(models_dir) = config::models_path(app) else {
        return;
    };
    stop();
    if !cleanup::installed(model, &models_dir) {
        set_status(
            app,
            Status::Error {
                message: format!("Download {} to use clean-up", model.label),
            },
        );
        return;
    }
    set_status(app, Status::Starting);
    let server = match spawn(model, &models_dir) {
        Ok(server) => server,
        Err(message) => {
            set_status(app, Status::Error { message });
            return;
        }
    };
    let port = server.port;
    let key = server.key.clone();
    if let Ok(mut slot) = SERVER.lock() {
        *slot = Some(server);
    }
    let app = app.clone();
    std::thread::spawn(move || match wait_ready(port, &key) {
        Ok(()) => {
            if let Ok(mut slot) = SERVER.lock()
                && let Some(server) = slot.as_mut().filter(|server| server.port == port)
            {
                server.ready = true;
                drop(slot);
                set_status(&app, Status::Ready);
            }
        }
        Err(message) if message == "Cancelled" => {}
        Err(message) => {
            if let Ok(mut slot) = SERVER.lock()
                && slot.as_ref().is_some_and(|server| server.port == port)
            {
                *slot = None;
            }
            set_status(&app, Status::Error { message });
        }
    });
}

/// Cleans `text`, waiting up to 20 s for a server that is still loading.
/// Errors leave it to the caller to insert the original text.
pub fn clean(text: &str) -> Result<String, String> {
    let started = Instant::now();
    let (port, key) = loop {
        let ready = SERVER
            .lock()
            .map_err(|error| error.to_string())?
            .as_ref()
            .map(|server| (server.ready, server.port, server.key.clone()));
        match ready {
            Some((true, port, key)) => break (port, key),
            Some((false, ..)) if started.elapsed() < Duration::from_secs(20) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Some(_) => return Err("Clean-up model is still loading".to_owned()),
            None => return Err("Clean-up is not running".to_owned()),
        }
    };
    // Generation time grows with length; allow ~1 s per 40 words on top.
    let timeout = Duration::from_secs(15 + text.split_whitespace().count() as u64 / 40);
    cleanup::clean(&format!("http://127.0.0.1:{port}"), &key, text, timeout)
}
