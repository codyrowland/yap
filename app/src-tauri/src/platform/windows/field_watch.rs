use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    UIA_ComboBoxControlTypeId, UIA_EditControlTypeId, UIA_TextControlTypeId, UIA_ValuePatternId,
};

use super::input::{Destination, focused_destination};

fn same_element(automation: &IUIAutomation, original: &IUIAutomationElement) -> bool {
    unsafe {
        automation
            .GetFocusedElement()
            .ok()
            .and_then(|current| automation.CompareElements(original, &current).ok())
            .is_some_and(|same| same.as_bool())
    }
}

/// The focused UI Automation element, if it is a non-password edit field
/// (a terminal's text area or an editable combo box such as a search box)
/// that has keyboard focus. The error says what was found instead.
fn focused_edit(automation: &IUIAutomation) -> Result<IUIAutomationElement, String> {
    unsafe {
        let focused = automation
            .GetFocusedElement()
            .map_err(|error| format!("no focused element ({error})"))?;
        let kind = focused
            .CurrentControlType()
            .map_err(|error| error.to_string())?;
        let editable_combo = kind == UIA_ComboBoxControlTypeId
            && focused
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .and_then(|value| value.CurrentIsReadOnly())
                .is_ok_and(|read_only| !read_only.as_bool());
        if kind != UIA_EditControlTypeId && kind != UIA_TextControlTypeId && !editable_combo {
            return Err(format!("focused element is control type {}", kind.0));
        }
        if focused
            .CurrentIsPassword()
            .map_or(true, |flag| flag.as_bool())
        {
            return Err("focused field is a password field".to_owned());
        }
        if !focused
            .CurrentHasKeyboardFocus()
            .is_ok_and(|flag| flag.as_bool())
        {
            return Err("focused field has no keyboard focus".to_owned());
        }
        Ok(focused)
    }
}

/// How long a field that isn't a native edit control may take to show up in
/// UI Automation. Chromium-based windows (WebView2, Electron) build their
/// accessibility tree only when first asked, so the first query often
/// returns the page instead of the field.
const FIELD_GRACE: Duration = Duration::from_millis(1500);

enum Command {
    Capture(u64, Option<Destination>),
    Check(u64),
    Cancel,
}

struct Outcome {
    id: u64,
    verified: Result<(), String>,
}

pub struct FieldWatch {
    commands: Sender<Command>,
    results: Receiver<Outcome>,
}

/// What the verifier watches for one recording.
struct Watch {
    id: u64,
    target: Destination,
    element: Option<IUIAutomationElement>,
    started: Instant,
    last_miss: String,
}

impl FieldWatch {
    pub fn start() -> Self {
        let (commands, requests) = mpsc::channel();
        let (responses, results) = mpsc::channel();
        std::thread::spawn(move || unsafe {
            if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                return;
            }

            let automation: windows::core::Result<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
            if let Ok(automation) = automation {
                // `Err` holds why the current recording can no longer insert.
                let mut watch: Result<Watch, (u64, String)> = Err((0, "not started".to_owned()));
                // Re-checks the watched field; returns why it is no longer valid.
                let refresh = |watch: &mut Watch| -> Result<(), String> {
                    if Some(&watch.target) != focused_destination().as_ref() {
                        return Err("focus moved to another window or control".to_owned());
                    }
                    if watch.target.native_edit() {
                        return Ok(());
                    }
                    match &watch.element {
                        Some(first) if same_element(&automation, first) => Ok(()),
                        Some(_) => Err("focus moved to another field".to_owned()),
                        None => match focused_edit(&automation) {
                            Ok(element) => {
                                watch.element = Some(element);
                                Ok(())
                            }
                            Err(miss) if watch.started.elapsed() < FIELD_GRACE => {
                                watch.last_miss = miss;
                                Ok(())
                            }
                            Err(miss) => Err(format!("no text field found: {miss}")),
                        },
                    }
                };
                loop {
                    match requests.recv_timeout(Duration::from_millis(50)) {
                        Ok(Command::Capture(id, target)) => {
                            watch = match target {
                                None => Err((id, "no focused control".to_owned())),
                                Some(target) if target.is_password() => {
                                    Err((id, "password field".to_owned()))
                                }
                                Some(target) => {
                                    let mut next = Watch {
                                        id,
                                        target,
                                        element: None,
                                        started: Instant::now(),
                                        last_miss: String::new(),
                                    };
                                    refresh(&mut next).map(|()| next).map_err(|why| (id, why))
                                }
                            };
                        }
                        Ok(Command::Check(id)) => {
                            let verified = match &mut watch {
                                Ok(current) if current.id == id => match refresh(current) {
                                    Ok(())
                                        if current.target.native_edit()
                                            || current.element.is_some() =>
                                    {
                                        Ok(())
                                    }
                                    Ok(()) => Err(format!(
                                        "no text field found yet: {}",
                                        current.last_miss
                                    )),
                                    Err(why) => Err(why),
                                },
                                Err((watched, why)) if *watched == id => Err(why.clone()),
                                _ => Err("a different recording was being watched".to_owned()),
                            };
                            let _ = responses.send(Outcome { id, verified });
                            watch = Err((0, "not started".to_owned()));
                        }
                        Ok(Command::Cancel) => watch = Err((0, "cancelled".to_owned())),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Ok(current) = &mut watch
                                && let Err(why) = refresh(current)
                            {
                                watch = Err((current.id, why));
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }

            CoUninitialize();
        });
        Self { commands, results }
    }

    pub fn capture(&self, id: u64, target: Option<Destination>) {
        let _ = self.commands.send(Command::Capture(id, target));
    }

    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel);
    }

    /// Whether recording `id` may insert; the error says why not.
    pub fn check(&self, id: u64) -> Result<(), String> {
        self.commands
            .send(Command::Check(id))
            .map_err(|_| "verifier stopped".to_owned())?;
        loop {
            let outcome = self
                .results
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "verifier timed out".to_owned())?;
            // Skip answers to earlier checks that arrived too late.
            if outcome.id == id {
                return outcome.verified;
            }
        }
    }
}
