use std::mem::zeroed;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG,
    PM_REMOVE, PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use yap_app::talk_keys::{Key, Modifier, Side};

use crate::talk_key_state::{self, Event};

pub const ALTGR_ADDS_CTRL: bool = true;

pub const RESERVED_SHORTCUTS: &[(&[Modifier], Key)] = &[
    (&[Modifier::Meta], Key::Letter('L')),
    (&[Modifier::Ctrl, Modifier::Alt], Key::Delete),
];

pub const RESERVED_MESSAGE: &str = "Windows reserves this shortcut";

const CONTROL: u32 = 0x11;
const SHIFT: u32 = 0x10;
const ALT: u32 = 0x12;

fn key_from_vk(vk: u32) -> Key {
    let modifier = Key::Modifier;
    match vk {
        CONTROL | 0xA2 => modifier(Modifier::Ctrl, Side::Left),
        0xA3 => modifier(Modifier::Ctrl, Side::Right),
        ALT | 0xA4 => modifier(Modifier::Alt, Side::Left),
        0xA5 => modifier(Modifier::Alt, Side::Right),
        SHIFT | 0xA0 => modifier(Modifier::Shift, Side::Left),
        0xA1 => modifier(Modifier::Shift, Side::Right),
        0x5B => modifier(Modifier::Meta, Side::Left),
        0x5C => modifier(Modifier::Meta, Side::Right),
        0x1B => Key::Escape,
        0x20 => Key::Space,
        0x09 => Key::Tab,
        0x0D => Key::Enter,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x2D => Key::Insert,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0xBA => Key::Semicolon,
        0xBB => Key::Equals,
        0xBC => Key::Comma,
        0xBD => Key::Minus,
        0xBE => Key::Period,
        0xBF => Key::Slash,
        0xC0 => Key::Backtick,
        0xDB => Key::LeftBracket,
        0xDC => Key::Backslash,
        0xDD => Key::RightBracket,
        0xDE => Key::Quote,
        0xE2 => Key::IntlBackslash,
        0x14 => Key::CapsLock,
        0x90 => Key::NumLock,
        0x91 => Key::ScrollLock,
        0x13 => Key::Pause,
        0x5D => Key::ContextMenu,
        0x6B => Key::NumpadAdd,
        0x6D => Key::NumpadSubtract,
        0x6A => Key::NumpadMultiply,
        0x6F => Key::NumpadDivide,
        0x6E => Key::NumpadDecimal,
        0x30..=0x39 => Key::Digit((vk - 0x30) as u8),
        0x41..=0x5A => Key::Letter(char::from(vk as u8)),
        0x70..=0x87 => Key::Function((vk - 0x6F) as u8),
        0x60..=0x69 => Key::Numpad((vk - 0x60) as u8),
        _ => Key::Native(vk),
    }
}

static ACCEPT_INJECTED: AtomicBool = AtomicBool::new(false);

/// Lets synthetic key presses act as talk keys. Only the input test, which
/// presses them itself, turns this on.
#[cfg_attr(not(feature = "input-test"), allow(dead_code))]
pub fn accept_injected_keys() {
    ACCEPT_INJECTED.store(true, Ordering::Relaxed);
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
        if key.flags & LLKHF_INJECTED == 0 || ACCEPT_INJECTED.load(Ordering::Relaxed) {
            let down = message as u32 == WM_KEYDOWN || message as u32 == WM_SYSKEYDOWN;
            let up = message as u32 == WM_KEYUP || message as u32 == WM_SYSKEYUP;
            let vk = normalize_key(key.vkCode, key.scanCode, key.flags);
            let outcome = talk_key_state::on_key(
                key_from_vk(vk),
                down,
                up,
                crate::platform::foreground_window,
            );
            if let Some(outcome) = outcome {
                if tracing {
                    trace(if outcome.involved {
                        Trace::Key {
                            vk,
                            down,
                            yap_in_front: super::input::app_in_front(),
                            capturing: outcome.capturing,
                            active: outcome.active,
                            suppressed: outcome.suppress,
                            event: outcome.event,
                        }
                    } else {
                        Trace::Other {
                            yap_in_front: super::input::app_in_front(),
                        }
                    });
                }

                if outcome.suppress {
                    return 1;
                }
            } else if tracing {
                trace(Trace::Busy {
                    yap_in_front: super::input::app_in_front(),
                });
            }
        } else if tracing {
            trace(Trace::Injected);
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

pub struct KeyboardHook {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start() -> Result<KeyboardHook, String> {
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
    Ok(KeyboardHook {
        running,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yap_app::talk_keys::Shortcut;

    #[test]
    fn virtual_keys_map_to_w3c_names() {
        for (vk, name) in [
            (0xA5, "AltRight"),
            (CONTROL, "Control"),
            (0xA2, "Control"),
            (0x5B, "Meta"),
            (0x20, "Space"),
            (0x4C, "KeyL"),
            (0x37, "Digit7"),
            (0x77, "F8"),
            (0x63, "Numpad3"),
            (0xDE, "Quote"),
            (0x14, "CapsLock"),
            (0x6B, "NumpadAdd"),
        ] {
            assert_eq!(key_from_vk(vk).name().as_deref(), Some(name));
        }
        assert_eq!(key_from_vk(0x1B), Key::Escape);
        assert_eq!(key_from_vk(0x92), Key::Native(0x92));
    }

    #[test]
    fn reserved_shortcuts_match_either_meta_key() {
        let reserved = |text: &str| {
            let shortcut = Shortcut::parse(text).unwrap();
            RESERVED_SHORTCUTS
                .iter()
                .any(|(modifiers, key)| shortcut.uses(modifiers, *key))
        };
        assert!(reserved("Meta+KeyL"));
        assert!(reserved("MetaRight+KeyL"));
        assert!(reserved("Control+Alt+Delete"));
        assert!(reserved("ControlRight+Alt+Shift+Delete"));
        assert!(!reserved("Control+KeyL"));
        assert!(!reserved("Control+Delete"));
    }
}
