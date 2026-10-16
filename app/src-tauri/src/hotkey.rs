use std::mem::zeroed;

use crate::platform_input::{self, Destination};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_REMOVE,
    PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const ESCAPE: u32 = 0x1B;
const F8: u32 = 0x77;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Pressed(Option<Destination>),
    Released,
    Cancelled,
}

struct KeyState {
    held: bool,
    active: bool,
    shortcut: u32,
}

impl Default for KeyState {
    fn default() -> Self {
        Self {
            held: false,
            active: false,
            shortcut: F8,
        }
    }
}

impl KeyState {
    fn handle(&mut self, key: u32, down: bool, up: bool) -> (bool, Option<Event>) {
        if key == ESCAPE && down && self.active {
            self.held = false;
            self.active = false;
            return (true, Some(Event::Cancelled));
        }

        if key == self.shortcut {
            if down && !self.active {
                self.held = true;
                self.active = true;
                return (true, Some(Event::Pressed(None)));
            }

            if up && self.held {
                self.held = false;
                return (true, Some(Event::Released));
            }

            return (true, None);
        }

        (false, None)
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

unsafe extern "system" fn hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
        if key.flags & LLKHF_INJECTED == 0 || accept_injected() {
            let down = message as u32 == WM_KEYDOWN || message as u32 == WM_SYSKEYDOWN;
            let up = message as u32 == WM_KEYUP || message as u32 == WM_SYSKEYUP;
            if let Ok(mut state) = KEY_STATE.get().unwrap().lock() {
                let (suppress, event) = state.handle(key.vkCode, down, up);
                if let Some(event) = event {
                    let event = match event {
                        Event::Pressed(_) => Event::Pressed(platform_input::focused_destination()),
                        other => other,
                    };
                    let _ = SENDER.get().unwrap().send(event);
                }

                if suppress {
                    return 1;
                }
            }
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

fn shortcut_code(shortcut: &str) -> Result<u32, String> {
    match shortcut {
        "F8" => Ok(0x77),
        "F9" => Ok(0x78),
        "F10" => Ok(0x79),
        _ => Err("Unsupported shortcut".to_owned()),
    }
}

pub fn update_shortcut(
    shortcut: &str,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let key = shortcut_code(shortcut)?;
    let mut state = KEY_STATE
        .get()
        .ok_or("Hotkey is not installed")?
        .lock()
        .map_err(|error| error.to_string())?;
    if state.active {
        return Err("Cannot change shortcut while dictating".to_owned());
    }

    persist()?;
    state.shortcut = key;
    Ok(())
}

pub fn finish() {
    if let Some(state) = KEY_STATE.get()
        && let Ok(mut state) = state.lock()
    {
        state.active = false;
        state.held = false;
    }
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

pub fn start(shortcut: &str) -> Result<(Hotkey, mpsc::Receiver<Event>), String> {
    let (sender, receiver) = mpsc::channel();
    let key = shortcut_code(shortcut)?;
    KEY_STATE
        .set(Mutex::new(KeyState {
            shortcut: key,
            ..KeyState::default()
        }))
        .map_err(|_| "Hotkey already installed")?;
    SENDER.set(sender).map_err(|_| "Hotkey already installed")?;
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

    #[test]
    fn hold_release_cancel_and_busy_press_are_distinct() {
        let mut state = KeyState::default();
        assert_eq!(
            state.handle(F8, true, false),
            (true, Some(Event::Pressed(None)))
        );
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(state.handle(F8, false, true), (true, Some(Event::Released)));
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(
            state.handle(ESCAPE, true, false),
            (true, Some(Event::Cancelled))
        );
        state.shortcut = 0x78;
        assert_eq!(state.handle(F8, true, false), (false, None));
        assert_eq!(
            state.handle(0x78, true, false),
            (true, Some(Event::Pressed(None)))
        );
    }
}
