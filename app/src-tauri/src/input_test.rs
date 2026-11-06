//! Scripted end-to-end input test (debug builds with `--features input-test`).
//! Driven over stdin by `bin/input_harness.rs`; only acts on Yap's own
//! controlled test windows.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Manager};
use yap_app::coordinator::{Phase, Settings};
use yap_app::settings::{Engine, Insertion};
use yap_app::speech::{SpeechConfig, SpeechProvider, Transcript};

use crate::audio::{self, Capture, Microphone, MicrophoneState};
use crate::dictation::{Engines, cancel_recording, insert_transcript};
use crate::platform::{self, FieldWatch};
use crate::state::SessionState;

#[derive(Clone, Copy)]
enum Fixture {
    Speech,
    Silence,
    Bounded,
    CaptureError,
    InferenceError,
}

static FIXTURE: Mutex<Fixture> = Mutex::new(Fixture::Speech);

fn fixture() -> Fixture {
    *FIXTURE.lock().unwrap()
}

/// Records the current fixture instead of the microphone, and only while a
/// controlled test window is in front.
struct FakeMicrophone;

impl Microphone for FakeMicrophone {
    fn start(&self, _: usize) -> Result<Capture, String> {
        if !controlled_window() {
            return Err("Controlled test window is not foreground".to_owned());
        }

        let fixture = fixture();
        let samples = match fixture {
            Fixture::Silence => vec![0.0; 3_200],
            Fixture::Bounded => vec![0.02; 16_000 * audio::MAX_SECONDS + 1],
            _ => vec![0.02; 3_200],
        };
        let error =
            matches!(fixture, Fixture::CaptureError).then(|| "Test microphone failure".to_owned());
        Ok(Capture::prerecorded(&samples, 16_000, error))
    }
}

struct FakeSpeech;

impl SpeechProvider for FakeSpeech {
    fn transcribe(&mut self, _: &[f32], cancelled: &AtomicBool) -> Result<Transcript, String> {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        if matches!(fixture(), Fixture::InferenceError) {
            return Err("Test inference failure".to_owned());
        }

        Ok(Transcript {
            text: "Yap test ✓".to_owned(),
            model_id: "test".to_owned(),
        })
    }
}

/// In test mode, swaps in the fake microphone and speech engine, the F8
/// talk key, typed insertion and no clean-up.
pub fn fakes(
    settings: yap_app::settings::Settings,
    microphone: MicrophoneState,
    engines: Engines,
) -> (yap_app::settings::Settings, MicrophoneState, Engines) {
    if !active() {
        return (settings, microphone, engines);
    }

    platform::keyboard_hook::accept_injected_keys();
    let settings = yap_app::settings::Settings {
        engine: Engine::Local,
        local_model_path: Some("test-model.bin".to_owned()),
        hold_key: "F8".to_owned(),
        insertion: Insertion::Type,
        cleanup_enabled: false,
        ..settings
    };
    let engines = Engines::new(Box::new(|_, _| Ok(Box::new(FakeSpeech))));
    (settings, Box::new(FakeMicrophone), engines)
}

fn active() -> bool {
    std::env::args().any(|arg| arg == "--input-test")
        && std::env::var("YAP_INPUT_TEST").as_deref() == Ok("1")
}

fn controlled_window() -> bool {
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

/// Runs the harness's commands from stdin. Called once the talk keys work.
pub fn listen(app: AppHandle) {
    use std::io::{BufRead, Write};

    if !active() {
        return;
    }

    std::thread::spawn(move || {
        let mut last_id = None;
        println!("YAP_TEST ready");
        println!("YAP_TEST hotkey_ready");
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
                    let session = state.lock().unwrap();
                    if session.coordinator.phase() != Phase::Idle {
                        Err("Job already active".to_owned())
                    } else {
                        *FIXTURE.lock().unwrap() = match line.as_str() {
                            "audio_silence" => Fixture::Silence,
                            "audio_bounded" => Fixture::Bounded,
                            "audio_capture_error" => Fixture::CaptureError,
                            "audio_inference_error" => Fixture::InferenceError,
                            _ => Fixture::Speech,
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
