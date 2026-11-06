use std::mem::zeroed;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, MSG, PM_REMOVE, PeekMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const ESCAPE: u32 = 0x1B;
const SPACE: u32 = 0x20;
const CONTROL: u32 = 0x11;
const SHIFT: u32 = 0x10;
const ALT: u32 = 0x12;
const META: u32 = 0x5B;

fn modifier(key: u32) -> Option<(usize, usize)> {
    match key {
        CONTROL | 0xA2 => Some((0, 0)),
        0xA3 => Some((0, 1)),
        ALT | 0xA4 => Some((1, 0)),
        0xA5 => Some((1, 1)),
        SHIFT | 0xA0 => Some((2, 0)),
        0xA1 => Some((2, 1)),
        META => Some((3, 0)),
        0x5C => Some((3, 1)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Shortcut {
    key: u32,
    modifiers: [u8; 4],
}

impl Shortcut {
    fn key_matches(self, key: u32) -> bool {
        self.key == key
            || (self.key == CONTROL && modifier(key).is_some_and(|(group, _)| group == 0))
            || (self.key == ALT && modifier(key).is_some_and(|(group, _)| group == 1))
            || (self.key == SHIFT && modifier(key).is_some_and(|(group, _)| group == 2))
            || (self.key == META && modifier(key).is_some_and(|(group, _)| group == 3))
    }

    fn matches(self, key: u32, pressed: &[bool; 8]) -> bool {
        self.key_matches(key)
            && self
                .modifiers
                .iter()
                .enumerate()
                .all(|(group, requirement)| {
                    if modifier(key).is_some_and(|(primary, _)| primary == group) {
                        return true;
                    }

                    let left = pressed[group * 2];
                    let right = pressed[group * 2 + 1];
                    if group == 0
                        && *requirement == 0
                        && (self.modifiers[1] == 2 || self.key == 0xA5)
                        && pressed[3]
                    {
                        return true;
                    }

                    match requirement {
                        0 => !left && !right,
                        1 => left || right,
                        2 => right,
                        _ => false,
                    }
                })
    }

    fn modifiers_held(self, pressed: &[bool; 8]) -> bool {
        self.modifiers
            .iter()
            .enumerate()
            .all(|(group, requirement)| match requirement {
                0 => true,
                1 => pressed[group * 2] || pressed[group * 2 + 1],
                2 => pressed[group * 2 + 1],
                _ => false,
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Pressed(isize),
    Released,
    Cancelled,
}

struct KeyState {
    held: bool,
    active: bool,
    shortcut: Shortcut,
    toggle_shortcut: Shortcut,
    modifiers: [bool; 8],
    capturing: Option<&'static str>,
    capture_started: Instant,
    toggle_held: bool,
    toggle_active: bool,
    suppress_release: bool,
}

impl Default for KeyState {
    fn default() -> Self {
        Self {
            held: false,
            active: false,
            shortcut: shortcut_code("F8").unwrap(),
            toggle_shortcut: shortcut_code("Ctrl+Shift+Space").unwrap(),
            modifiers: [false; 8],
            capturing: None,
            capture_started: Instant::now(),
            toggle_held: false,
            toggle_active: false,
            suppress_release: false,
        }
    }
}

impl KeyState {
    fn handle(&mut self, key: u32, down: bool, up: bool) -> (bool, Option<Event>) {
        if let Some((group, side)) = modifier(key)
            && (down || up)
        {
            self.modifiers[group * 2 + side] = down;
        }

        if self.capturing.is_some() && self.capture_started.elapsed() > Duration::from_secs(60) {
            self.capturing = None;
        }

        if self.capturing.is_some() {
            return (false, None);
        }

        if key == ESCAPE && down && self.active {
            self.suppress_release = self.held;
            self.held = false;
            self.active = false;
            self.toggle_active = false;
            return (true, Some(Event::Cancelled));
        }

        if self.held && !self.shortcut.modifiers_held(&self.modifiers) {
            self.held = false;
            self.suppress_release = true;
            return (false, Some(Event::Released));
        }

        if self.shortcut.key_matches(key) && self.suppress_release {
            if up {
                self.suppress_release = false;
            }

            return (true, None);
        }

        if self.shortcut.key_matches(key) && up && self.held {
            self.held = false;
            return (true, Some(Event::Released));
        }

        if self.toggle_shortcut.key_matches(key) && self.toggle_held {
            if up {
                self.toggle_held = false;
            }

            return (true, None);
        }

        if down && self.shortcut.matches(key, &self.modifiers) {
            if !self.active {
                self.held = true;
                self.active = true;
                return (true, Some(Event::Pressed(0)));
            }

            return (true, None);
        }

        if down && self.toggle_shortcut.matches(key, &self.modifiers) {
            self.toggle_held = true;
            if !self.active {
                self.active = true;
                self.toggle_active = true;
                return (true, Some(Event::Pressed(0)));
            }

            if self.toggle_active {
                self.toggle_active = false;
                return (true, Some(Event::Released));
            }

            return (true, None);
        }

        (false, None)
    }
}

impl KeyState {
    fn request_stop(&mut self) -> bool {
        if !self.active || !(self.held || self.toggle_active) {
            return false;
        }

        if self.held {
            self.held = false;
            self.suppress_release = true;
        }
        self.toggle_active = false;
        true
    }
}

static KEY_STATE: OnceLock<Mutex<KeyState>> = OnceLock::new();
static SENDER: OnceLock<mpsc::Sender<Event>> = OnceLock::new();

#[cfg(all(debug_assertions, feature = "input-test"))]
fn accept_injected() -> bool {
    std::env::args().any(|arg| arg == "--input-test")
        && std::env::var("YAP_INPUT_TEST").as_deref() == Ok("1")
}

#[cfg(not(all(debug_assertions, feature = "input-test")))]
fn accept_injected() -> bool {
    false
}

fn normalize_key(key: u32, scan: u32, flags: u32) -> u32 {
    match key {
        CONTROL => {
            if flags & LLKHF_EXTENDED != 0 {
                0xA3
            } else {
                0xA2
            }
        }
        SHIFT => {
            if scan == 0x36 {
                0xA1
            } else {
                0xA0
            }
        }
        ALT => {
            if flags & LLKHF_EXTENDED != 0 {
                0xA5
            } else {
                0xA4
            }
        }
        _ => key,
    }
}

/// Debug builds trace what the hook does with talk-key-related keys, so a
/// key that never starts a recording can be explained. Only modifier keys,
/// Escape and the two talk keys are named; every other key is just counted,
/// so no typed text ever reaches the log. Traces go through a channel to a
/// logger thread: the hook itself never touches the disk.
#[derive(Debug)]
enum Trace {
    Key {
        vk: u32,
        down: bool,
        yap_in_front: bool,
        capturing: bool,
        active: bool,
        suppressed: bool,
        event: Option<Event>,
    },
    Other {
        yap_in_front: bool,
    },
    Injected,
    Busy {
        yap_in_front: bool,
    },
}

static TRACE: OnceLock<mpsc::Sender<Trace>> = OnceLock::new();

fn trace(entry: Trace) {
    if let Some(sender) = TRACE.get() {
        let _ = sender.send(entry);
    }
}

fn yap_in_front() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        let mut process = 0;
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(window, &mut process);
        !window.is_null() && process == std::process::id()
    }
}

fn start_trace_logger() {
    if !cfg!(debug_assertions) {
        return;
    }
    let (sender, receiver) = mpsc::channel::<Trace>();
    if TRACE.set(sender).is_err() {
        return;
    }
    std::thread::spawn(move || {
        let (mut other, mut other_front, mut injected, mut busy) = (0_u32, 0_u32, 0_u32, 0_u32);
        loop {
            let entry = receiver.recv_timeout(Duration::from_secs(3));
            if (entry.is_err() || matches!(entry, Ok(Trace::Key { .. })))
                && other + injected + busy > 0
            {
                crate::logfile::write(&format!(
                    "hook: {other} other keys ({other_front} while Yap in front), {injected} injected skipped, {busy} dropped while busy"
                ));
                (other, other_front, injected, busy) = (0, 0, 0, 0);
            }
            match entry {
                Ok(Trace::Key {
                    vk,
                    down,
                    yap_in_front,
                    capturing,
                    active,
                    suppressed,
                    event,
                }) => crate::logfile::write(&format!(
                    "hook: key 0x{vk:02X} {} yap_in_front={yap_in_front} capturing={capturing} active={active} suppressed={suppressed} event={event:?}",
                    if down { "down" } else { "up" }
                )),
                Ok(Trace::Other { yap_in_front }) => {
                    other += 1;
                    other_front += u32::from(yap_in_front);
                }
                Ok(Trace::Injected) => injected += 1,
                Ok(Trace::Busy { yap_in_front }) => {
                    busy += 1;
                    other_front += u32::from(yap_in_front);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}

unsafe extern "system" fn hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
        let tracing = TRACE.get().is_some();
        if key.flags & LLKHF_INJECTED == 0 || accept_injected() {
            let down = message as u32 == WM_KEYDOWN || message as u32 == WM_SYSKEYDOWN;
            let up = message as u32 == WM_KEYUP || message as u32 == WM_SYSKEYUP;
            if let Ok(mut state) = KEY_STATE.get().unwrap().try_lock() {
                let physical = normalize_key(key.vkCode, key.scanCode, key.flags);
                let (suppress, event) = state.handle(physical, down, up);
                let event = event.map(|event| match event {
                    Event::Pressed(_) => Event::Pressed(unsafe { GetForegroundWindow() } as isize),
                    other => other,
                });
                if tracing {
                    let relevant = physical == ESCAPE
                        || modifier(physical).is_some()
                        || state.shortcut.key_matches(physical)
                        || state.toggle_shortcut.key_matches(physical);
                    trace(if relevant {
                        Trace::Key {
                            vk: physical,
                            down,
                            yap_in_front: yap_in_front(),
                            capturing: state.capturing.is_some(),
                            active: state.active,
                            suppressed: suppress,
                            event: event.clone(),
                        }
                    } else {
                        Trace::Other {
                            yap_in_front: yap_in_front(),
                        }
                    });
                }
                if let Some(event) = event {
                    let _ = SENDER.get().unwrap().send(event);
                }

                if suppress {
                    return 1;
                }
            } else if tracing {
                trace(Trace::Busy {
                    yap_in_front: yap_in_front(),
                });
            }
        } else if tracing {
            trace(Trace::Injected);
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

fn key_name(key: u32) -> Option<String> {
    let name = match key {
        CONTROL | 0xA2 => "Ctrl",
        0xA3 => "Right Ctrl",
        ALT | 0xA4 => "Alt",
        0xA5 => "Right Alt",
        SHIFT | 0xA0 => "Shift",
        0xA1 => "Right Shift",
        META => "Meta",
        0x5C => "Right Meta",
        SPACE => "Space",
        0x09 => "Tab",
        0x0D => "Enter",
        0x08 => "Backspace",
        0x2E => "Delete",
        0x2D => "Insert",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0xBA => "Semicolon",
        0xBB => "Equals",
        0xBC => "Comma",
        0xBD => "Minus",
        0xBE => "Period",
        0xBF => "Slash",
        0xC0 => "Backtick",
        0xDB => "LeftBracket",
        0xDC => "Backslash",
        0xDD => "RightBracket",
        0xDE => "Quote",
        ESCAPE => return None,
        0x30..=0x39 | 0x41..=0x5A => return char::from_u32(key).map(|key| key.to_string()),
        0x70..=0x87 => return Some(format!("F{}", key - 0x6F)),
        0x60..=0x69 => return Some(format!("Numpad{}", key - 0x60)),
        0 | 0xFF => return None,
        _ => return Some(format!("VK-{key:02X}")),
    };
    Some(name.to_owned())
}

fn key_code(name: &str) -> Option<u32> {
    let key = match name {
        "Ctrl" => CONTROL,
        "Right Ctrl" => 0xA3,
        "Alt" => ALT,
        "Right Alt" => 0xA5,
        "Shift" => SHIFT,
        "Right Shift" => 0xA1,
        "Meta" => META,
        "Right Meta" => 0x5C,
        "Space" => SPACE,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Backspace" => 0x08,
        "Delete" => 0x2E,
        "Insert" => 0x2D,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "Semicolon" => 0xBA,
        "Equals" => 0xBB,
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Backtick" => 0xC0,
        "LeftBracket" => 0xDB,
        "Backslash" => 0xDC,
        "RightBracket" => 0xDD,
        "Quote" => 0xDE,
        _ => {
            if name.len() == 1 {
                let key = name.as_bytes()[0];
                if key.is_ascii_uppercase() || key.is_ascii_digit() {
                    return Some(key as u32);
                }
            }

            if let Some(number) = name.strip_prefix('F')
                && let Ok(number) = number.parse::<u32>()
                && (1..=24).contains(&number)
            {
                return Some(0x6F + number);
            }

            if let Some(number) = name.strip_prefix("Numpad")
                && let Ok(number) = number.parse::<u32>()
                && number <= 9
            {
                return Some(0x60 + number);
            }

            if let Some(hex) = name.strip_prefix("VK-")
                && hex.len() == 2
                && let Ok(key) = u32::from_str_radix(hex, 16)
                && key_name(key).as_deref() == Some(name)
            {
                return Some(key);
            }

            return None;
        }
    };
    Some(key)
}

fn shortcut_code(shortcut: &str) -> Result<Shortcut, String> {
    let mut tokens = shortcut.split('+').collect::<Vec<_>>();
    let name = tokens.pop().ok_or("Unsupported shortcut")?;
    let key = key_code(name).ok_or("Unsupported shortcut")?;
    let mut modifiers = [0_u8; 4];
    for token in tokens {
        let code = key_code(token).ok_or("Unsupported modifier")?;
        let (group, side) = modifier(code).ok_or("Unsupported modifier")?;
        if modifiers[group] != 0 || modifier(key).is_some_and(|(primary, _)| primary == group) {
            return Err("Duplicate shortcut modifier".to_owned());
        }

        modifiers[group] = if side == 1 { 2 } else { 1 };
    }

    if (key == 0x4C && modifiers[3] != 0) || (key == 0x2E && modifiers[0] != 0 && modifiers[1] != 0)
    {
        return Err("Windows reserves this shortcut".to_owned());
    }

    Ok(Shortcut { key, modifiers })
}

pub fn validate_shortcuts(shortcut: &str, toggle_shortcut: &str) -> Result<(), String> {
    let hold = shortcut_code(shortcut)?;
    let toggle = shortcut_code(toggle_shortcut)?;
    for (first, second) in [(hold, toggle), (toggle, hold)] {
        if let Some((group, _)) = modifier(first.key)
            && second.modifiers[group] != 0
        {
            return Err(
                "Talk keys conflict: a modifier-only key starts the other combination".to_owned(),
            );
        }
    }

    for key in 1..255 {
        if hold.key_matches(key) && toggle.key_matches(key) {
            for combination in 0..256_u16 {
                let pressed = std::array::from_fn(|index| combination & (1 << index) != 0);
                if hold.matches(key, &pressed) && toggle.matches(key, &pressed) {
                    return Err("Talk keys must be different".to_owned());
                }
            }
        }
    }

    Ok(())
}

pub fn begin_capture(kind: &str) -> Result<(), String> {
    let kind = match kind {
        "hold" => "hold",
        "toggle" => "toggle",
        _ => return Err("Unknown talk key".to_owned()),
    };
    let mut state = KEY_STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())?;
    if state.active || state.capturing.is_some() {
        return Err("Cannot change talk keys while recording or capturing".to_owned());
    }

    state.capturing = Some(kind);
    state.capture_started = Instant::now();
    Ok(())
}

pub fn cancel_capture() -> Result<(), String> {
    let mut state = KEY_STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())?;
    state.capturing = None;
    Ok(())
}

pub fn complete_capture(kind: &str, shortcut: &str) -> Result<(), String> {
    shortcut_code(shortcut)?;
    let mut state = KEY_STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())?;
    let current = state.capturing.ok_or("Talk-key capture is not active")?;
    if current != kind {
        return Err("Wrong talk key is being captured".to_owned());
    }

    state.capturing = None;
    Ok(())
}

pub fn update_shortcut(
    shortcut: &str,
    toggle_shortcut: &str,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    validate_shortcuts(shortcut, toggle_shortcut)?;
    let key = shortcut_code(shortcut)?;
    let toggle_key = shortcut_code(toggle_shortcut)?;
    let mut state = KEY_STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())?;
    if state.active || state.capturing.is_some() {
        return Err("Cannot change shortcut while dictating or capturing".to_owned());
    }

    persist()?;
    state.shortcut = key;
    state.toggle_shortcut = toggle_key;
    state.modifiers = [false; 8];
    Ok(())
}

pub fn finish() {
    if let Some(state) = KEY_STATE.get()
        && let Ok(mut state) = state.lock()
    {
        state.active = false;
        state.held = false;
        state.toggle_active = false;
    }
}

/// Whether the active recording was started with the toggle key (it keeps
/// running until stopped) rather than the hold key.
pub fn toggle_active() -> bool {
    KEY_STATE
        .get()
        .and_then(|state| state.lock().ok())
        .is_some_and(|state| state.active && state.toggle_active)
}

/// Ends the active recording as if the talk key had been released: used by
/// the overlay's Stop button and when the recording reaches its length
/// limit. A hold key that is still down is ignored until it is let go.
pub fn request_stop() -> bool {
    let Some(state) = KEY_STATE.get() else {
        return false;
    };
    let Ok(mut state) = state.lock() else {
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

pub struct Hotkey {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start(
    shortcut: &str,
    toggle_shortcut: &str,
) -> Result<(Hotkey, mpsc::Receiver<Event>), String> {
    validate_shortcuts(shortcut, toggle_shortcut)?;
    let (sender, receiver) = mpsc::channel();
    let key = shortcut_code(shortcut)?;
    let toggle_key = shortcut_code(toggle_shortcut)?;
    KEY_STATE
        .set(Mutex::new(KeyState {
            shortcut: key,
            toggle_shortcut: toggle_key,
            ..KeyState::default()
        }))
        .map_err(|_| "Hotkey already installed")?;
    SENDER.set(sender).map_err(|_| "Hotkey already installed")?;
    start_trace_logger();
    let running = Arc::new(AtomicBool::new(true));
    let enabled = Arc::clone(&running);
    let (ready, started) = mpsc::channel();
    let thread = std::thread::spawn(move || unsafe {
        let installed = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), std::ptr::null_mut(), 0);
        if installed.is_null() {
            let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
            return;
        }

        let _ = ready.send(Ok(()));
        while enabled.load(Ordering::Acquire) {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        UnhookWindowsHookEx(installed);
        crate::logfile::write("talk-key hook stopped");
    });
    started.recv().map_err(|error| error.to_string())??;
    Ok((
        Hotkey {
            running,
            thread: Some(thread),
        },
        receiver,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const F8: u32 = 0x77;

    #[test]
    fn request_stop_ends_hold_or_toggle_once_and_swallows_the_hold_key_up() {
        let mut state = KeyState {
            shortcut: shortcut_code("F8").unwrap(),
            ..KeyState::default()
        };
        assert!(!state.request_stop());
        assert_eq!(
            state.handle(F8, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert!(state.request_stop());
        assert!(!state.request_stop());
        assert_eq!(state.handle(F8, false, true), (true, None));

        let toggle = shortcut_code("Ctrl+Shift+Space").unwrap();
        state.active = false;
        state.toggle_shortcut = toggle;
        for key in [0xA2, 0xA0] {
            state.handle(key, true, false);
        }
        assert_eq!(
            state.handle(0x20, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert!(state.toggle_active);
        assert!(state.request_stop());
        assert!(!state.toggle_active);
    }

    #[test]
    fn hold_release_cancel_and_busy_press_are_distinct() {
        let mut state = KeyState::default();
        assert_eq!(
            state.handle(F8, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(state.handle(F8, false, true), (true, Some(Event::Released)));
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(
            state.handle(ESCAPE, true, false),
            (true, Some(Event::Cancelled))
        );
        state.shortcut = shortcut_code("F9").unwrap();
        assert_eq!(state.handle(F8, true, false), (false, None));
        assert_eq!(
            state.handle(0x78, true, false),
            (true, Some(Event::Pressed(0)))
        );
    }

    #[test]
    fn toggle_ignores_repeats_and_releases_then_stops_on_second_press() {
        let mut state = KeyState::default();
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(state.handle(SHIFT, true, false), (false, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert_eq!(state.handle(SPACE, true, false), (true, None));
        assert_eq!(state.handle(SPACE, false, true), (true, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Released))
        );
        assert_eq!(state.handle(SPACE, false, true), (true, None));
        finish();
        assert!(validate_shortcuts("Ctrl+Space", "Ctrl+Space").is_err());
    }

    #[test]
    fn parses_right_modifiers_and_rejects_reserved_or_conflicting_keys() {
        let mut state = KeyState {
            capturing: Some("hold"),
            ..KeyState::default()
        };
        assert_eq!(state.handle(0x5C, true, false), (false, None));
        assert_eq!(state.handle(0xA3, true, false), (false, None));
        assert_eq!(state.handle(0x7B, true, false), (false, None));
        assert!(!state.active);
        let shortcut = shortcut_code("Right Ctrl+Right Meta+F12").unwrap();
        assert!(shortcut.matches(0x7B, &state.modifiers));
        assert!(!shortcut.matches(
            0x7B,
            &[true, false, false, false, false, false, true, false]
        ));
        assert!(validate_shortcuts("Ctrl", "Ctrl+Space").is_err());
        assert_eq!(shortcut_code("L").unwrap().key, 0x4C);
        assert!(validate_shortcuts("Meta+L", "F8").is_err());
        assert!(validate_shortcuts("Ctrl+Alt+Delete", "F8").is_err());
    }

    #[test]
    fn right_alt_hold_accepts_windows_synthetic_ctrl() {
        let mut state = KeyState {
            shortcut: shortcut_code("Right Alt").unwrap(),
            ..KeyState::default()
        };
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(
            state.handle(0xA5, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert_eq!(
            state.handle(0xA5, false, true),
            (true, Some(Event::Released))
        );
        assert_eq!(state.handle(CONTROL, false, true), (false, None));
        assert!(validate_shortcuts("Right Alt", "Ctrl+Right Alt").is_err());
    }

    #[test]
    fn right_alt_alone_toggles_with_or_without_windows_synthetic_ctrl() {
        assert!(validate_shortcuts("Ctrl+Space", "Right Alt").is_ok());
        for synthetic_ctrl in [false, true] {
            let mut state = KeyState {
                shortcut: shortcut_code("Ctrl+Space").unwrap(),
                toggle_shortcut: shortcut_code("Right Alt").unwrap(),
                ..KeyState::default()
            };
            if synthetic_ctrl {
                assert_eq!(state.handle(CONTROL, true, false), (false, None));
            }

            assert_eq!(
                state.handle(0xA5, true, false),
                (true, Some(Event::Pressed(0)))
            );
            assert_eq!(state.handle(0xA5, false, true), (true, None));
            assert_eq!(
                state.handle(0xA5, true, false),
                (true, Some(Event::Released))
            );
        }
    }

    #[test]
    fn right_alt_toggle_accepts_windows_synthetic_ctrl() {
        let mut state = KeyState {
            shortcut: shortcut_code("Ctrl+Space").unwrap(),
            toggle_shortcut: shortcut_code("Right Alt+Down").unwrap(),
            ..KeyState::default()
        };
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(state.handle(0xA5, true, false), (false, None));
        assert_eq!(
            state.handle(0x28, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert_eq!(state.handle(0x28, false, true), (true, None));
        assert_eq!(state.handle(0xA5, false, true), (false, None));
        assert_eq!(state.handle(CONTROL, false, true), (false, None));
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(state.handle(0xA5, true, false), (false, None));
        assert_eq!(
            state.handle(0x28, true, false),
            (true, Some(Event::Released))
        );
        assert!(validate_shortcuts("Right Alt+Down", "Ctrl+Right Alt+Down").is_err());
    }

    #[test]
    fn sequential_capture_does_not_activate_or_choose_first_key() {
        let mut state = KeyState {
            capturing: Some("hold"),
            ..KeyState::default()
        };
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(state.handle(CONTROL, false, true), (false, None));
        assert_eq!(state.handle(0x4C, true, false), (false, None));
        assert_eq!(state.handle(0x4C, false, true), (false, None));
        assert_eq!(state.capturing, Some("hold"));
        assert!(!state.active);
        state.capturing = None;
        state.shortcut = shortcut_code("Ctrl+L").unwrap();
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(
            state.handle(0x4C, true, false),
            (true, Some(Event::Pressed(0)))
        );
    }

    #[test]
    fn hold_releases_when_modifier_released_and_suppresses_space_key_up() {
        let mut state = KeyState {
            shortcut: shortcut_code("Ctrl+Space").unwrap(),
            ..KeyState::default()
        };
        assert_eq!(state.handle(CONTROL, true, false), (false, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Pressed(0)))
        );
        assert_eq!(
            state.handle(CONTROL, false, true),
            (false, Some(Event::Released))
        );
        assert_eq!(state.handle(SPACE, true, false), (true, None));
        assert_eq!(state.handle(SPACE, false, true), (true, None));
    }
}
