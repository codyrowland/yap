//! Paste-based insertion: put the transcript on the clipboard, press Ctrl+V,
//! then put back what the user had copied.
//!
//! Only text can be restored. When the clipboard holds something else (an
//! image, files), pasting would destroy it, so callers fall back to typing.

use std::mem::size_of;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL,
};

use crate::platform::PasteResult;

const VK_V: VIRTUAL_KEY = 0x56;
const VK_Z: VIRTUAL_KEY = 0x5A;
/// How long the target app gets to read the clipboard before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(250);

/// What was on the clipboard before Yap replaced it.
enum Saved {
    Empty,
    Text(Vec<u16>),
}

struct Open;

impl Open {
    fn new() -> Option<Self> {
        // Another app may hold the clipboard briefly; retry for ~100 ms.
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads the clipboard if it is empty or plain text; `None` when it holds
/// anything Yap could not put back.
fn save(_open: &Open) -> Option<Saved> {
    unsafe {
        if CountClipboardFormats() == 0 {
            return Some(Saved::Empty);
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) == 0 {
            return None;
        }

        let handle = GetClipboardData(CF_UNICODETEXT as u32);
        if handle.is_null() {
            return None;
        }
        let pointer = GlobalLock(handle) as *const u16;
        if pointer.is_null() {
            return None;
        }
        let units = GlobalSize(handle) / size_of::<u16>();
        let slice = std::slice::from_raw_parts(pointer, units);
        let end = slice.iter().position(|unit| *unit == 0).unwrap_or(units);
        let mut text = slice[..end].to_vec();
        text.push(0);
        GlobalUnlock(handle);
        Some(Saved::Text(text))
    }
}

/// Replaces the clipboard with `text` (NUL-terminated UTF-16), marked so
/// Windows clipboard history and cloud clipboard skip it.
fn put(_open: &Open, text: &[u16]) -> bool {
    unsafe {
        if EmptyClipboard() == 0 {
            return false;
        }

        let bytes = std::mem::size_of_val(text);
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            return false;
        }
        let pointer = GlobalLock(memory) as *mut u16;
        if pointer.is_null() {
            GlobalFree(memory);
            return false;
        }
        std::ptr::copy_nonoverlapping(text.as_ptr(), pointer, text.len());
        GlobalUnlock(memory);
        if SetClipboardData(CF_UNICODETEXT as u32, memory as HANDLE).is_null() {
            GlobalFree(memory);
            return false;
        }

        for (format, value) in [
            ("ExcludeClipboardContentFromMonitorProcessing", None),
            ("CanIncludeInClipboardHistory", Some(0_u32)),
            ("CanUploadToCloudClipboard", Some(0_u32)),
        ] {
            let id = RegisterClipboardFormatW(wide(format).as_ptr());
            if id == 0 {
                continue;
            }
            let size = if value.is_some() { size_of::<u32>() } else { 1 };
            let memory = GlobalAlloc(GMEM_MOVEABLE, size);
            if memory.is_null() {
                continue;
            }
            let pointer = GlobalLock(memory) as *mut u8;
            if !pointer.is_null() {
                match value {
                    Some(value) => std::ptr::write_unaligned(pointer as *mut u32, value),
                    None => *pointer = 0,
                }
                GlobalUnlock(memory);
            }
            if SetClipboardData(id, memory as HANDLE).is_null() {
                GlobalFree(memory);
            }
        }
        true
    }
}

fn key(code: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: code,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn chord(letter: VIRTUAL_KEY) -> bool {
    let inputs = [
        key(VK_CONTROL, false),
        key(letter, false),
        key(letter, true),
        key(VK_CONTROL, true),
    ];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) == inputs.len() as u32
    }
}

/// Pastes `text` into the focused field and restores the previous clipboard
/// text, unless the user copied something new in the meantime.
pub fn paste(text: &str) -> PasteResult {
    let Some(open) = Open::new() else {
        return PasteResult::Unavailable;
    };
    let Some(saved) = save(&open) else {
        return PasteResult::Unavailable;
    };
    if !put(&open, &wide(text)) {
        return PasteResult::Failed;
    }
    drop(open);
    let ours = unsafe { GetClipboardSequenceNumber() };

    let sent = chord(VK_V);
    std::thread::sleep(PASTE_SETTLE);
    if unsafe { GetClipboardSequenceNumber() } == ours
        && let Some(open) = Open::new()
    {
        match &saved {
            Saved::Empty => unsafe {
                EmptyClipboard();
            },
            Saved::Text(previous) => {
                put(&open, previous);
            }
        }
    }

    if sent {
        PasteResult::Pasted
    } else {
        PasteResult::Failed
    }
}

/// Sends Ctrl+Z to the focused app.
pub fn undo() -> bool {
    chord(VK_Z)
}
