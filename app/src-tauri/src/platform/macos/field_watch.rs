use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFBoolean, CFString, CFType};

use super::input::{
    Destination, Element, element, focused_application, focused_destination, is_password_field,
    settable, text,
};

const TEXT_ROLES: [&str; 3] = ["AXTextField", "AXTextArea", "AXComboBox"];

/// How long the focused field may take to show up in the accessibility
/// tree. Chromium and Electron apps build it only when first asked, so the
/// first query often returns the page instead of the field.
const FIELD_GRACE: Duration = Duration::from_millis(1500);

/// Asks Electron apps to build their accessibility tree; other apps ignore it.
fn enable_accessibility(app: &AXUIElement) {
    let enabled: &CFType = CFBoolean::new(true).as_ref();
    let _ = unsafe {
        app.set_attribute_value(&CFString::from_static_str("AXManualAccessibility"), enabled)
    };
}

/// The focused element of `pid`, if it is a non-password text field. The
/// error says what was found instead.
fn focused_edit(pid: i32) -> Result<Element, String> {
    let (app, focused_pid) = focused_application().ok_or("no focused app")?;
    if focused_pid != pid {
        return Err("focus moved to another app".to_owned());
    }
    let field = element(&app, "AXFocusedUIElement").ok_or("no focused element")?;
    if is_password_field(&field) {
        return Err("focused field is a password field".to_owned());
    }
    let role = text(&field, "AXRole").unwrap_or_default();
    if TEXT_ROLES.contains(&role.as_str()) || settable(&field, "AXSelectedTextRange") {
        return Ok(field);
    }

    Err(format!("focused element is {role}"))
}

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
    element: Option<Element>,
    started: Instant,
    last_miss: String,
}

/// Re-checks the watched field; returns why it is no longer valid.
fn refresh(watch: &mut Watch) -> Result<(), String> {
    if Some(&watch.target) != focused_destination().as_ref() {
        return Err("focus moved to another window or control".to_owned());
    }
    match (&watch.element, focused_edit(watch.target.pid())) {
        (Some(first), Ok(current)) if **first == *current => Ok(()),
        (Some(_), _) => Err("focus moved to another field".to_owned()),
        (None, Ok(current)) => {
            watch.element = Some(current);
            Ok(())
        }
        (None, Err(miss)) if watch.started.elapsed() < FIELD_GRACE => {
            watch.last_miss = miss;
            Ok(())
        }
        (None, Err(miss)) => Err(format!("no text field found: {miss}")),
    }
}

impl FieldWatch {
    pub fn start() -> Self {
        let (commands, requests) = mpsc::channel();
        let (responses, results) = mpsc::channel();
        std::thread::spawn(move || {
            // `Err` holds why the current recording can no longer insert.
            let mut watch: Result<Watch, (u64, String)> = Err((0, "not started".to_owned()));
            loop {
                match requests.recv_timeout(Duration::from_millis(50)) {
                    Ok(Command::Capture(id, target)) => {
                        watch = match target {
                            None => Err((id, "no focused control".to_owned())),
                            Some(target) if target.is_password() => {
                                Err((id, "password field".to_owned()))
                            }
                            Some(target) => {
                                if let Some((app, _)) = focused_application() {
                                    enable_accessibility(&app);
                                }
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
                                Ok(()) if current.element.is_some() => Ok(()),
                                Ok(()) => {
                                    Err(format!("no text field found yet: {}", current.last_miss))
                                }
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
