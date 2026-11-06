use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};

use yap_app::talk_keys::{self, Key, Shortcut, TalkKey, TalkKeys};

use crate::platform::WindowId;
use crate::platform::keyboard_hook::{self, KeyboardHook};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Pressed(Option<WindowId>),
    Released,
    Cancelled,
}

pub struct KeyOutcome {
    pub suppress: bool,
    pub event: Option<Event>,
    pub involved: bool,
    pub capturing: bool,
    pub active: bool,
}

static STATE: OnceLock<Mutex<TalkKeys>> = OnceLock::new();
static SENDER: OnceLock<mpsc::Sender<Event>> = OnceLock::new();

fn parse(text: &str) -> Result<Shortcut, String> {
    let shortcut = Shortcut::parse(text)?;
    if let Key::Native(code) = shortcut.key()
        && !keyboard_hook::native_key_known(code)
    {
        return Err("Unsupported shortcut".to_owned());
    }

    if keyboard_hook::RESERVED_SHORTCUTS
        .iter()
        .any(|(modifiers, key)| shortcut.uses(modifiers, *key))
    {
        return Err(keyboard_hook::RESERVED_MESSAGE.to_owned());
    }

    Ok(shortcut)
}

fn parse_pair(hold: &str, toggle: &str) -> Result<(Shortcut, Shortcut), String> {
    let (hold, toggle) = (parse(hold)?, parse(toggle)?);
    talk_keys::check_pair(hold, toggle, keyboard_hook::ALTGR_ADDS_CTRL)?;
    Ok((hold, toggle))
}

fn state() -> Result<MutexGuard<'static, TalkKeys>, String> {
    STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())
}

pub fn validate(hold: &str, toggle: &str) -> Result<(), String> {
    parse_pair(hold, toggle).map(drop)
}

pub fn start(hold: &str, toggle: &str) -> Result<(KeyboardHook, mpsc::Receiver<Event>), String> {
    let (hold, toggle) = parse_pair(hold, toggle)?;
    let (sender, receiver) = mpsc::channel();
    STATE
        .set(Mutex::new(TalkKeys::new(
            hold,
            toggle,
            keyboard_hook::ALTGR_ADDS_CTRL,
        )))
        .map_err(|_| "Hotkey already installed")?;
    SENDER.set(sender).map_err(|_| "Hotkey already installed")?;
    Ok((keyboard_hook::start()?, receiver))
}

pub fn on_key(
    key: Key,
    down: bool,
    up: bool,
    foreground: impl FnOnce() -> Option<WindowId>,
) -> Option<KeyOutcome> {
    let mut state = STATE.get()?.try_lock().ok()?;
    let (suppress, event) = state.handle(key, down, up);
    let event = event.map(|event| match event {
        talk_keys::Event::Pressed => Event::Pressed(foreground()),
        talk_keys::Event::Released => Event::Released,
        talk_keys::Event::Cancelled => Event::Cancelled,
    });
    if let Some(event) = &event
        && let Some(sender) = SENDER.get()
    {
        let _ = sender.send(event.clone());
    }
    Some(KeyOutcome {
        suppress,
        event,
        involved: state.involves(key),
        capturing: state.capturing(),
        active: state.active(),
    })
}

pub fn begin_capture(kind: &str) -> Result<(), String> {
    let which = TalkKey::from_name(kind).ok_or("Unknown talk key")?;
    state()?.begin_capture(which)
}

pub fn cancel_capture() -> Result<(), String> {
    state()?.cancel_capture();
    Ok(())
}

pub fn complete_capture(kind: &str, shortcut: &str) -> Result<(), String> {
    parse(shortcut)?;
    let which = TalkKey::from_name(kind).ok_or("Unknown talk key")?;
    state()?.complete_capture(which)
}

pub fn update_shortcut(
    hold: &str,
    toggle: &str,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let (hold, toggle) = parse_pair(hold, toggle)?;
    let mut state = state()?;
    if state.busy() {
        return Err("Cannot change shortcut while dictating or capturing".to_owned());
    }

    persist()?;
    state.set_shortcuts(hold, toggle);
    Ok(())
}

pub fn finish() {
    if let Ok(mut state) = state() {
        state.finish();
    }
}

pub fn toggle_active() -> bool {
    state().is_ok_and(|state| state.toggle_active())
}

pub fn request_stop() -> bool {
    let Ok(mut state) = state() else {
        return false;
    };
    if !state.request_stop() {
        return false;
    }

    drop(state);
    SENDER
        .get()
        .is_some_and(|sender| sender.send(Event::Released).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_reserved_unknown_and_conflicting_keys() {
        assert!(validate("Right Alt", "Ctrl+Shift+Space").is_ok());
        assert!(validate("VK-92", "F8").is_ok());
        assert_eq!(
            validate("Meta+L", "F8"),
            Err(keyboard_hook::RESERVED_MESSAGE.to_owned())
        );
        assert!(validate("F8", "Ctrl+Alt+Delete").is_err());
        assert!(validate("VK-41", "F8").is_err());
        assert!(validate("VK-1B", "F8").is_err());
        assert!(validate("Ctrl+Space", "Ctrl+Space").is_err());
        assert!(validate("Right Alt", "Ctrl+Right Alt").is_err());
    }
}
