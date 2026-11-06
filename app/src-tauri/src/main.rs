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
//! - `hotkey`, `platform_input`, `clipboard`, `audio`, `autostart`: Windows I/O

mod assistant;
mod audio;
mod autostart;
mod clipboard;
mod commands;
mod config;
mod dictation;
mod hotkey;
#[cfg(all(debug_assertions, feature = "input-test"))]
mod input_test;
mod logfile;
mod onboarding;
mod overlay;
mod own_window;
mod platform_input;
mod state;
mod tray;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use yap_app::history::History;
use yap_app::settings::Settings;

use platform_input::Verifier;
use state::{Session, SessionState};

pub fn shutdown(app: &AppHandle) {
    if let Ok(mut session) = app.state::<SessionState>().lock() {
        drop(session.stop_for_exit());
    }
    if let Ok(verifier) = app.state::<Arc<Mutex<Verifier>>>().lock() {
        verifier.cancel();
    }
    hotkey::finish();
    overlay::hide(app);
    assistant::stop();
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

fn main() {
    tauri::Builder::default()
        // tao registers raw keyboard/mouse input on our windows by default.
        // With that registration Windows stops calling our own low-level
        // keyboard hook while a Yap window is focused, so the talk key went
        // deaf inside Yap (wry #1761, tauri #13919). We don't use device
        // events, so turn the registration off.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .manage(Arc::new(Mutex::new(Session::new(Settings::default()))))
        .manage(Arc::new(Mutex::new(Verifier::start())))
        .manage(Arc::new(Mutex::new(None::<(String, Arc<AtomicBool>)>)))
        .invoke_handler(tauri::generate_handler![
            commands::microphones,
            commands::current_settings,
            commands::save_settings,
            commands::catalog,
            commands::begin_key_capture,
            commands::cancel_key_capture,
            commands::complete_key_capture,
            commands::credential_status,
            commands::update_credential,
            commands::test_connection,
            commands::download_model,
            commands::cancel_model_download,
            commands::delete_model,
            commands::cleanup_catalog,
            commands::download_cleanup,
            commands::delete_cleanup,
            commands::open_models_folder,
            commands::history_entries,
            commands::delete_history,
            commands::tray_state,
            commands::set_engine,
            commands::open_window,
            commands::quit_app,
            onboarding::onboarding_needed,
            onboarding::onboarding_permissions,
            onboarding::complete_onboarding,
            overlay::overlay_stop,
            overlay::overlay_undo,
            own_window::own_window_reply,
        ])
        .setup(|app| {
            logfile::init(app.path().app_data_dir()?.join("yap.log"));
            tray::install(app)?;
            let history_path = app.path().app_data_dir()?.join("history.json");
            app.manage(Mutex::new(
                History::open(history_path).map_err(std::io::Error::other)?,
            ));
            start_history_expiry(app.handle().clone());

            #[cfg_attr(not(all(debug_assertions, feature = "input-test")), allow(unused_mut))]
            let mut settings = config::load(app.handle()).map_err(std::io::Error::other)?;
            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test::active() {
                settings.hold_key = "F8".to_owned();
                settings.insertion = yap_app::settings::Insertion::Type;
            }
            let (hold_key, toggle_key) = (settings.hold_key.clone(), settings.toggle_key.clone());
            #[cfg(all(debug_assertions, feature = "input-test"))]
            let preload = !input_test::active();
            #[cfg(not(all(debug_assertions, feature = "input-test")))]
            let preload = true;
            if preload {
                dictation::preload(&settings);
                assistant::sync(app.handle(), &settings);
            }
            app.state::<SessionState>()
                .lock()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .settings = settings;

            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test::active() {
                input_test::start(app.handle().clone());
            }

            let (hook, events) =
                hotkey::start(&hold_key, &toggle_key).map_err(std::io::Error::other)?;
            app.manage(Mutex::new(Some(hook)));
            logfile::write(&format!(
                "talk keys installed: hold={hold_key} toggle={toggle_key}"
            ));
            #[cfg(all(debug_assertions, feature = "input-test"))]
            if input_test::active() {
                println!("YAP_TEST hotkey_ready");
            }
            dictation::run_hotkey_events(app.handle().clone(), events);

            if autostart::launched_in_background()
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            Ok(())
        })
        .on_window_event(|window, event| match window.label() {
            "main" => {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let _ = hotkey::cancel_capture();
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
