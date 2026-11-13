//! Paste-based insertion: put the transcript on the clipboard, press Ctrl+V,
//! then put back what the user had copied.
//!
//! Only text can be restored. When the clipboard holds something else (an
//! image, files), pasting would destroy it, so callers fall back to typing.

use std::mem::size_of;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardOwner, GetClipboardSequenceNumber, GetOpenClipboardWindow,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL,
};

use super::input::window_process_name;
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

fn format_name(format: u32) -> String {
    let standard = match format {
        1 => "text",
        2 => "bitmap",
        3 => "metafile",
        7 => "OEM text",
        8 => "DIB image",
        13 => "unicode text",
        14 => "enhanced metafile",
        15 => "files",
        16 => "locale",
        17 => "DIBV5 image",
        _ => "",
    };
    if !standard.is_empty() {
        return standard.to_owned();
    }

    let mut name = [0u16; 128];
    let len = unsafe { GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32) };
    if len > 0 {
        String::from_utf16_lossy(&name[..len as usize])
    } else {
        format!("format {format}")
    }
}

fn describe_contents(_open: &Open) -> String {
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 || formats.len() == 12 {
            break;
        }
        formats.push(format_name(format));
    }
    let owner = window_process_name(unsafe { GetClipboardOwner() });
    format!("copied by {owner}: {}", formats.join(", "))
}

/// Reads the clipboard if it is empty or plain text; otherwise says why it
/// can't be put back.
fn save(open: &Open) -> Result<Saved, String> {
    unsafe {
        if CountClipboardFormats() == 0 {
            return Ok(Saved::Empty);
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) == 0 {
            return Err(format!(
                "clipboard holds no text, {}",
                describe_contents(open)
            ));
        }

        let handle = GetClipboardData(CF_UNICODETEXT as u32);
        if handle.is_null() {
            return Err(format!(
                "could not read the clipboard text, {}",
                describe_contents(open)
            ));
        }
        let pointer = GlobalLock(handle) as *const u16;
        if pointer.is_null() {
            return Err(format!(
                "could not lock the clipboard text, {}",
                describe_contents(open)
            ));
        }
        let units = GlobalSize(handle) / size_of::<u16>();
        let slice = std::slice::from_raw_parts(pointer, units);
        let end = slice.iter().position(|unit| *unit == 0).unwrap_or(units);
        let mut text = slice[..end].to_vec();
        text.push(0);
        GlobalUnlock(handle);
        Ok(Saved::Text(text))
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
        let holder = window_process_name(unsafe { GetOpenClipboardWindow() });
        return PasteResult::Unavailable(format!("clipboard busy, held open by {holder}"));
    };
    let saved = match save(&open) {
        Ok(saved) => saved,
        Err(reason) => return PasteResult::Unavailable(reason),
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
