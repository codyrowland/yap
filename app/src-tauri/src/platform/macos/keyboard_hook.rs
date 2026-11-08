use std::ffi::c_void;
use std::ptr::{NonNull, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use yap_app::talk_keys::{Key, Modifier, Side};

use super::SYNTHETIC_MARKER;
use crate::talk_key_state;

pub const ALTGR_ADDS_CTRL: bool = false;

pub const RESERVED_SHORTCUTS: &[(&[Modifier], Key)] =
    &[(&[Modifier::Ctrl, Modifier::Meta], Key::Letter('Q'))];

pub const RESERVED_MESSAGE: &str = "macOS reserves this shortcut";

const CAPS_LOCK: u16 = 0x39;

fn key_from_code(code: u16) -> Key {
    let modifier = Key::Modifier;
    match code {
        0x3B => modifier(Modifier::Ctrl, Side::Left),
        0x3E => modifier(Modifier::Ctrl, Side::Right),
        0x3A => modifier(Modifier::Alt, Side::Left),
        0x3D => modifier(Modifier::Alt, Side::Right),
        0x38 => modifier(Modifier::Shift, Side::Left),
        0x3C => modifier(Modifier::Shift, Side::Right),
        0x37 => modifier(Modifier::Meta, Side::Left),
        0x36 => modifier(Modifier::Meta, Side::Right),
        0x00 => Key::Letter('A'),
        0x0B => Key::Letter('B'),
        0x08 => Key::Letter('C'),
        0x02 => Key::Letter('D'),
        0x0E => Key::Letter('E'),
        0x03 => Key::Letter('F'),
        0x05 => Key::Letter('G'),
        0x04 => Key::Letter('H'),
        0x22 => Key::Letter('I'),
        0x26 => Key::Letter('J'),
        0x28 => Key::Letter('K'),
        0x25 => Key::Letter('L'),
        0x2E => Key::Letter('M'),
        0x2D => Key::Letter('N'),
        0x1F => Key::Letter('O'),
        0x23 => Key::Letter('P'),
        0x0C => Key::Letter('Q'),
        0x0F => Key::Letter('R'),
        0x01 => Key::Letter('S'),
        0x11 => Key::Letter('T'),
        0x20 => Key::Letter('U'),
        0x09 => Key::Letter('V'),
        0x0D => Key::Letter('W'),
        0x07 => Key::Letter('X'),
        0x10 => Key::Letter('Y'),
        0x06 => Key::Letter('Z'),
        0x1D => Key::Digit(0),
        0x12 => Key::Digit(1),
        0x13 => Key::Digit(2),
        0x14 => Key::Digit(3),
        0x15 => Key::Digit(4),
        0x17 => Key::Digit(5),
        0x16 => Key::Digit(6),
        0x1A => Key::Digit(7),
        0x1C => Key::Digit(8),
        0x19 => Key::Digit(9),
        0x7A => Key::Function(1),
        0x78 => Key::Function(2),
        0x63 => Key::Function(3),
        0x76 => Key::Function(4),
        0x60 => Key::Function(5),
        0x61 => Key::Function(6),
        0x62 => Key::Function(7),
        0x64 => Key::Function(8),
        0x65 => Key::Function(9),
        0x6D => Key::Function(10),
        0x67 => Key::Function(11),
        0x6F => Key::Function(12),
        0x69 => Key::Function(13),
        0x6B => Key::Function(14),
        0x71 => Key::Function(15),
        0x6A => Key::Function(16),
        0x40 => Key::Function(17),
        0x4F => Key::Function(18),
        0x50 => Key::Function(19),
        0x5A => Key::Function(20),
        0x52 => Key::Numpad(0),
        0x53 => Key::Numpad(1),
        0x54 => Key::Numpad(2),
        0x55 => Key::Numpad(3),
        0x56 => Key::Numpad(4),
        0x57 => Key::Numpad(5),
        0x58 => Key::Numpad(6),
        0x59 => Key::Numpad(7),
        0x5B => Key::Numpad(8),
        0x5C => Key::Numpad(9),
        0x35 => Key::Escape,
        0x31 => Key::Space,
        0x30 => Key::Tab,
        0x24 => Key::Enter,
        0x33 => Key::Backspace,
        0x75 => Key::Delete,
        0x72 => Key::Insert,
        0x73 => Key::Home,
        0x77 => Key::End,
        0x74 => Key::PageUp,
        0x79 => Key::PageDown,
        0x7B => Key::Left,
        0x7E => Key::Up,
        0x7C => Key::Right,
        0x7D => Key::Down,
        0x29 => Key::Semicolon,
        0x18 => Key::Equals,
        0x2B => Key::Comma,
        0x1B => Key::Minus,
        0x2F => Key::Period,
        0x2C => Key::Slash,
        0x32 => Key::Backtick,
        0x21 => Key::LeftBracket,
        0x2A => Key::Backslash,
        0x1E => Key::RightBracket,
        0x27 => Key::Quote,
        0x0A => Key::IntlBackslash,
        CAPS_LOCK => Key::CapsLock,
        0x47 => Key::NumLock,
        0x6E => Key::ContextMenu,
        0x45 => Key::NumpadAdd,
        0x4E => Key::NumpadSubtract,
        0x43 => Key::NumpadMultiply,
        0x4B => Key::NumpadDivide,
        0x41 => Key::NumpadDecimal,
        _ => Key::Native(u32::from(code)),
    }
}

/// Whether a modifier key is now down, from the device-specific bits macOS
/// sets in a flags-changed event for each physical modifier key.
fn modifier_down(code: u16, flags: CGEventFlags) -> Option<bool> {
    let bit = match code {
        0x3B => 0x0001,
        0x38 => 0x0002,
        0x3C => 0x0004,
        0x37 => 0x0008,
        0x36 => 0x0010,
        0x3A => 0x0020,
        0x3D => 0x0040,
        0x3E => 0x2000,
        0x3F => return Some(flags.contains(CGEventFlags::MaskSecondaryFn)),
        _ => return None,
    };
    Some(flags.0 & bit != 0)
}

static TAP: AtomicPtr<CFMachPort> = AtomicPtr::new(null_mut());

fn handle(key: Key, down: bool, up: bool) -> bool {
    talk_key_state::on_key(key, down, up, crate::platform::foreground_window)
        .is_some_and(|outcome| outcome.suppress)
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _info: *mut c_void,
) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(tap) = unsafe { TAP.load(Ordering::Acquire).as_ref() } {
            CGEvent::tap_enable(tap, true);
        }
        return event.as_ptr();
    }

    let current = Some(unsafe { event.as_ref() });
    if CGEvent::integer_value_field(current, CGEventField::EventSourceUserData) == SYNTHETIC_MARKER
    {
        return event.as_ptr();
    }

    let code = CGEvent::integer_value_field(current, CGEventField::KeyboardEventKeycode) as u16;
    let key = key_from_code(code);
    let suppress = if kind == CGEventType::KeyDown {
        handle(key, true, false)
    } else if kind == CGEventType::KeyUp {
        handle(key, false, true)
    } else if kind == CGEventType::FlagsChanged && code == CAPS_LOCK {
        let suppress = handle(key, true, false);
        handle(key, false, true) || suppress
    } else if kind == CGEventType::FlagsChanged {
        match modifier_down(code, CGEvent::flags(current)) {
            Some(down) => handle(key, down, !down),
            None => false,
        }
    } else {
        false
    };

    if suppress { null_mut() } else { event.as_ptr() }
}

fn create_tap() -> Option<objc2_core_foundation::CFRetained<CFMachPort>> {
    let mask = (1_u64 << CGEventType::KeyDown.0)
        | (1_u64 << CGEventType::KeyUp.0)
        | (1_u64 << CGEventType::FlagsChanged.0);
    unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(callback),
            null_mut(),
        )
    }
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

/// Starts the talk-key event tap. Until Yap has the Accessibility
/// permission macOS refuses the tap, so it is retried every second.
pub fn start() -> Result<KeyboardHook, String> {
    let running = Arc::new(AtomicBool::new(true));
    let enabled = Arc::clone(&running);
    let thread = std::thread::spawn(move || {
        let mut waiting_logged = false;
        let tap = loop {
            if !enabled.load(Ordering::Acquire) {
                return;
            }
            if let Some(tap) = create_tap() {
                break tap;
            }
            if !waiting_logged {
                crate::logfile::write("talk keys: waiting for the Accessibility permission");
                waiting_logged = true;
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        TAP.store(
            objc2_core_foundation::CFRetained::as_ptr(&tap).as_ptr(),
            Ordering::Release,
        );
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
            crate::logfile::write("talk keys: could not attach the event tap");
            return;
        };
        let Some(run_loop) = CFRunLoop::current() else {
            return;
        };
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&tap, true);
        crate::logfile::write("talk keys: event tap started");
        while enabled.load(Ordering::Acquire) {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.25, false);
        }

        CGEvent::tap_enable(&tap, false);
        TAP.store(null_mut(), Ordering::Release);
        crate::logfile::write("talk-key hook stopped");
    });
    Ok(KeyboardHook {
        running,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes_map_to_w3c_names() {
        for (code, name) in [
            (0x3D, "AltRight"),
            (0x3B, "Control"),
            (0x37, "Meta"),
            (0x36, "MetaRight"),
            (0x31, "Space"),
            (0x25, "KeyL"),
            (0x1A, "Digit7"),
            (0x64, "F8"),
            (0x55, "Numpad3"),
            (0x27, "Quote"),
            (0x39, "CapsLock"),
            (0x45, "NumpadAdd"),
        ] {
            assert_eq!(key_from_code(code).name().as_deref(), Some(name));
        }
        assert_eq!(key_from_code(0x35), Key::Escape);
        assert_eq!(key_from_code(0x3F), Key::Native(0x3F));
    }

    #[test]
    fn modifier_state_comes_from_the_device_bits() {
        let right_option = CGEventFlags(CGEventFlags::MaskAlternate.0 | 0x40);
        assert_eq!(modifier_down(0x3D, right_option), Some(true));
        assert_eq!(modifier_down(0x3A, right_option), Some(false));
        assert_eq!(modifier_down(0x31, right_option), None);
    }
}
