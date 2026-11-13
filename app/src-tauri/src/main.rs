#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Yap — system-wide voice to text for Windows.
//!
//! Module map:
//! - `config`      settings on disk (JSON, legacy import) and applying changes
//! - `state`       the shared dictation session
//! - `dictation`   talk key → record → transcribe → insert
//! - `overlay`     recording overlay window (design 2h)
//! - `tray`        tray icon and popover (design 2i)
//! - `onboarding`  first-run setup (design 2a)
//! - `own_window`  dictation into Yap's own window
//! - `commands`    Settings / History / tray window commands
//! - `talk_key_state` talk-key state shared with the platform keyboard hook
//! - `platform`    the OS interface; `platform/windows` implements it
//! - `audio`       microphone capture

mod assistant;
mod audio;
mod commands;
mod config;
mod dictation;
#[cfg(all(debug_assertions, feature = "input-test"))]
mod input_test;
mod logfile;
mod onboarding;
mod overlay;
mod own_window;
mod platform;
mod speakers;
mod state;
mod talk_key_state;
mod tray;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use yap_app::history::History;

use platform::FieldWatch;
use state::{Session, SessionState, SharedSettings};

pub fn shutdown(app: &AppHandle) {
    if let Ok(mut session) = app.state::<SessionState>().lock() {
        drop(session.stop_for_exit());
    }
    if let Ok(verifier) = app.state::<Arc<Mutex<FieldWatch>>>().lock() {
        verifier.cancel();
    }
    talk_key_state::finish();
    overlay::hide(app);
    assistant::stop();
    commands::update::install_pending_on_quit(app);
    app.exit(0);
}

/// Expires yesterday's history entries while the app stays open.
fn start_history_expiry(handle: AppHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if handle
                .state::<SessionState>()
                .lock()
                .is_ok_and(|session| session.coordinator.closed())
            {
                break;
            }
            let result = handle
                .state::<Mutex<History>>()
                .lock()
                .map_err(|error| error.to_string())
                .and_then(|mut history| history.expire());
            if result == Ok(true) {
                let _ = handle.emit_to("main", "history-updated", ());
            }
        }
    });
}

fn watch_for_sleep() {
    const TICK: std::time::Duration = std::time::Duration::from_secs(10);
    const GAP: std::time::Duration = std::time::Duration::from_secs(60);
    std::thread::spawn(|| {
        let mut last = std::time::SystemTime::now();
        loop {
            std::thread::sleep(TICK);
            let now = std::time::SystemTime::now();
            if let Ok(elapsed) = now.duration_since(last)
                && elapsed > GAP
            {
                logfile::write(&format!(
                    "resumed after {} s without running (sleep or hibernate)",
                    elapsed.as_secs()
                ));
            }

            last = now;
        }
    });
}

fn main() {
    tauri::Builder::default()
        // tao registers raw keyboard/mouse input on our windows by default.
        // With that registration Windows stops calling our own low-level
        // keyboard hook while a Yap window is focused, so the talk key went
        // deaf inside Yap (wry #1761, tauri #13919). We don't use device
        // events, so turn the registration off.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Arc::new(Mutex::new(Session::new())))
        .manage(SharedSettings::default())
        .manage(commands::update::PendingUpdate::default())
        .manage(Arc::new(Mutex::new(FieldWatch::start())))
        .manage(Arc::new(Mutex::new(None::<(String, Arc<AtomicBool>)>)))
        .invoke_handler(tauri::generate_handler![
            commands::settings::microphones,
            commands::settings::current_settings,
            commands::settings::save_settings,
            commands::settings::catalog,
            commands::settings::begin_key_capture,
            commands::settings::cancel_key_capture,
            commands::settings::complete_key_capture,
            commands::credentials::credential_status,
            commands::credentials::update_credential,
            commands::credentials::test_connection,
            commands::models::download_model,
            commands::models::cancel_model_download,
            commands::models::delete_model,
            commands::cleanup::cleanup_catalog,
            commands::cleanup::download_cleanup,
            commands::cleanup::delete_cleanup,
            commands::models::open_models_folder,
            commands::history::history_entries,
            commands::history::delete_history,
            commands::tray::tray_state,
            commands::tray::set_engine,
            commands::tray::open_window,
            commands::tray::quit_app,
            commands::update::check_for_update,
            commands::update::pending_update,
            commands::update::install_update,
            onboarding::onboarding_needed,
            onboarding::onboarding_permissions,
            onboarding::complete_onboarding,
            overlay::overlay_stop,
            overlay::overlay_undo,
            overlay::overlay_drawn,
            overlay::overlay_log,
            own_window::own_window_reply,
        ])
        .setup(|app| {
            logfile::init(app.path().app_data_dir()?.join("yap.log"));
            speakers::restore_after_crash(app.handle());
            tray::install(app)?;
            let history_path = app.path().app_data_dir()?.join("history.json");
            app.manage(Mutex::new(
                History::open(history_path).map_err(std::io::Error::other)?,
            ));
            start_history_expiry(app.handle().clone());
            commands::update::start_auto_update(app.handle().clone());

            let settings = config::load(app.handle()).map_err(std::io::Error::other)?;
            logfile::write(&format!(
                "system: {} {}; webview {}; overlay shown={} minimal={}; microphone \"{}\" (index {})",
                std::env::consts::OS,
                std::env::consts::ARCH,
                tauri::webview_version().unwrap_or_else(|error| format!("unknown ({error})")),
                settings.show_overlay,
                settings.minimal_overlay,
                settings.microphone_name.as_deref().unwrap_or("none saved"),
                settings.microphone_index
            ));
            watch_for_sleep();
            let microphone: audio::MicrophoneState = Box::new(audio::SystemMicrophone);
            let engines = dictation::Engines::system();
            #[cfg(all(debug_assertions, feature = "input-test"))]
            let (settings, microphone, engines) = input_test::fakes(settings, microphone, engines);
            app.manage(microphone);
            app.manage(engines);
            app.manage(dictation::LiveState::default());
            let (hold_key, toggle_key) = (settings.hold_key.clone(), settings.toggle_key.clone());
            dictation::preload(app.handle(), &settings);
            assistant::sync(app.handle(), &settings);
            if let Ok(models_dir) = config::models_path(app.handle()) {
                yap_app::cleanup::remove_retired(&models_dir);
            }

            app.state::<SharedSettings>().set(settings);
            for window in app.config().app.windows.clone() {
                tauri::WebviewWindowBuilder::from_config(app.handle(), &window)?.build()?;
            }

            let (hook, events) =
                talk_key_state::start(&hold_key, &toggle_key).map_err(std::io::Error::other)?;
            app.manage(Mutex::new(Some(hook)));
            logfile::write(&format!(
                "talk keys installed: hold={hold_key} toggle={toggle_key}"
            ));
            #[cfg(all(debug_assertions, feature = "input-test"))]
            input_test::listen(app.handle().clone());
            dictation::run_talk_key_events(app.handle().clone(), events);

            if platform::launched_in_background()
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            Ok(())
        })
        .on_window_event(|window, event| match window.label() {
            "main" => {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let _ = talk_key_state::cancel_capture();
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            overlay::LABEL => match event {
                WindowEvent::Focused(true) => overlay::give_back_focus(window.app_handle()),
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                _ => {}
            },
            tray::LABEL => {
                if matches!(event, WindowEvent::Focused(false)) {
                    let _ = window.hide();
                }
            }
            _ => {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start Yap");
}
