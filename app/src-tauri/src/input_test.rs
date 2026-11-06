//! Scripted end-to-end input test (debug builds with `--features input-test`).
//! Driven over stdin by `bin/input_harness.rs`; only acts on Yap's own
//! controlled test windows.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Manager};
use yap_app::coordinator::{Phase, Settings};
use yap_app::speech::SpeechConfig;

use crate::audio;
use crate::dictation::{cancel_recording, insert_transcript};
use crate::platform::{self, FieldWatch};
use crate::state::SessionState;

pub fn active() -> bool {
    std::env::args().any(|arg| arg == "--input-test")
        && std::env::var("YAP_INPUT_TEST").as_deref() == Ok("1")
}

pub fn controlled_window() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

    let mut title = [0u16; 128];
    let count = unsafe {
        GetWindowTextW(
            GetForegroundWindow(),
            title.as_mut_ptr(),
            title.len() as i32,
        )
    };
    matches!(
        String::from_utf16_lossy(&title[..count as usize]).as_str(),
        "Yap controlled input test" | "Yap Chrome test focused"
    )
}

pub fn start(app: AppHandle) {
    use std::io::{BufRead, Write};

    std::thread::spawn(move || {
        let mut last_id = None;
        println!("YAP_TEST ready");
        let _ = std::io::stdout().flush();
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            let result = match line.as_str() {
                "start" => {
                    if !controlled_window() {
                        Err("Controlled test window is not foreground".to_owned())
                    } else {
                        let destination = platform::focused_destination();
                        let target = destination.clone().map(crate::state::Target::App);
                        let state = app.state::<SessionState>();
                        let mut session = state.lock().unwrap();
                        if session.coordinator.phase() != Phase::Idle {
                            Err("Job already active".to_owned())
                        } else {
                            let id = session
                                .coordinator
                                .start(
                                    Settings {
                                        microphone_index: 0,
                                        speech: SpeechConfig::Local {
                                            adapter: "test".to_owned(),
                                            model_id: "test".to_owned(),
                                            artifact: PathBuf::new(),
                                        },
                                        language: None,
                                    },
                                    target.clone(),
                                    Instant::now(),
                                )
                                .unwrap();
                            last_id = Some(id);
                            app.state::<Arc<Mutex<FieldWatch>>>()
                                .lock()
                                .unwrap()
                                .capture(id, destination);
                            Ok("started".to_owned())
                        }
                    }
                }
                "audio_silence"
                | "audio_bounded"
                | "audio_speech"
                | "audio_capture_error"
                | "audio_inference_error" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.coordinator.phase() != Phase::Idle {
                        Err("Job already active".to_owned())
                    } else {
                        session.test_audio = match line.as_str() {
                            "audio_silence" => audio::Fixture::Silence,
                            "audio_bounded" => audio::Fixture::Bounded,
                            "audio_capture_error" => audio::Fixture::CaptureError,
                            "audio_inference_error" => audio::Fixture::InferenceError,
                            _ => audio::Fixture::Speech,
                        };
                        Ok(line.clone())
                    }
                }
                "phase" => {
                    let state = app.state::<SessionState>();
                    let session = state.lock().unwrap();
                    Ok(format!("phase_{:?}", session.coordinator.phase()))
                }
                "release" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.coordinator.active_id().is_some_and(|id| {
                        session
                            .coordinator
                            .release(id, Some(1.0), Instant::now())
                            .is_some()
                    }) {
                        Ok("released".to_owned())
                    } else {
                        Err("Could not release job".to_owned())
                    }
                }
                "transcript" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if session.coordinator.active_id().is_some_and(|id| {
                        session
                            .coordinator
                            .transcript(id, "Yap test ✓".to_owned(), Instant::now())
                    }) {
                        Ok("transcribed".to_owned())
                    } else {
                        Err("Could not transcribe job".to_owned())
                    }
                }
                "submit" => {
                    let state = app.state::<SessionState>();
                    let session = state.lock().unwrap();
                    if session.coordinator.phase() != Phase::Ready {
                        Err("Job is not ready".to_owned())
                    } else {
                        let id = session.coordinator.active_id().unwrap();
                        drop(session);
                        let inserted =
                            tauri::async_runtime::block_on(insert_transcript(app.clone(), id));
                        Ok(if inserted { "inserted" } else { "not_inserted" }.to_owned())
                    }
                }
                "cancel" => cancel_recording(app.state::<SessionState>().inner(), &app)
                    .map(|()| "cancelled".to_owned()),
                "late" => {
                    let state = app.state::<SessionState>();
                    let mut session = state.lock().unwrap();
                    if last_id.is_some_and(|id| {
                        session
                            .coordinator
                            .transcript(id, "late result".to_owned(), Instant::now())
                    }) {
                        Err("Stale transcript was accepted".to_owned())
                    } else {
                        Ok("ignored".to_owned())
                    }
                }
                _ => Err("Unknown test command".to_owned()),
            };
            match result {
                Ok(message) => println!("YAP_TEST {message}"),
                Err(error) => println!("YAP_TEST error: {error}"),
            }
            let _ = std::io::stdout().flush();
        }
    });
}
