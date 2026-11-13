//! Paste-based insertion: put the transcript on the clipboard, press Ctrl+V,
//! then put back what the user had copied.
//!
//! Everything stored as memory blocks is restored (text, images, files, HTML).
//! Formats held as GDI handles (bitmap, metafile, palette) are skipped;
//! Windows rebuilds the bitmap from the saved DIB. When nothing on the
//! clipboard can be restored, callers fall back to typing.

use std::mem::size_of;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardOwner, GetClipboardSequenceNumber, GetOpenClipboardWindow,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
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
const OPEN_TIMEOUT: Duration = Duration::from_secs(1);
const OPEN_RETRY: Duration = Duration::from_millis(10);

struct Format {
    id: u32,
    data: Vec<u8>,
}

struct Open;

impl Open {
    fn new() -> Option<Self> {
        let deadline = Instant::now() + OPEN_TIMEOUT;
        loop {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Self);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(OPEN_RETRY);
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn text_format(text: &str) -> Format {
    Format {
        id: CF_UNICODETEXT as u32,
        data: text
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect(),
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

fn formats(_open: &Open) -> Vec<u32> {
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            return formats;
        }
        formats.push(format);
    }
}

fn describe_contents(open: &Open) -> String {
    let names: Vec<String> = formats(open)
        .into_iter()
        .take(12)
        .map(format_name)
        .collect();
    let owner = window_process_name(unsafe { GetClipboardOwner() });
    format!("copied by {owner}: {}", names.join(", "))
}

fn restorable(format: u32) -> bool {
    !matches!(
        format,
        2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E | 0x200..=0x3FF
    )
}

fn read(_open: &Open, format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }

        let pointer = GlobalLock(handle) as *const u8;
        if pointer.is_null() {
            return None;
        }

        let data = std::slice::from_raw_parts(pointer, GlobalSize(handle)).to_vec();
        GlobalUnlock(handle);
        Some(data)
    }
}

fn save(open: &Open) -> Result<Vec<Format>, String> {
    let saved: Vec<Format> = formats(open)
        .into_iter()
        .filter(|format| restorable(*format))
        .filter_map(|id| read(open, id).map(|data| Format { id, data }))
        .collect();
    if saved.is_empty() && unsafe { CountClipboardFormats() } > 0 {
        return Err(format!(
            "nothing on the clipboard can be restored, {}",
            describe_contents(open)
        ));
    }

    Ok(saved)
}

fn set(id: u32, data: &[u8]) -> bool {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
        if memory.is_null() {
            return false;
        }

        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return false;
        }

        std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
        GlobalUnlock(memory);
        if SetClipboardData(id, memory as HANDLE).is_null() {
            GlobalFree(memory);
            return false;
        }

        true
    }
}

/// Replaces the clipboard with `formats`, marked so Windows clipboard history
/// and cloud clipboard skip it. An empty list just clears the clipboard.
fn put(_open: &Open, formats: &[Format]) -> bool {
    if unsafe { EmptyClipboard() } == 0 {
        return false;
    }

    if formats.is_empty() {
        return true;
    }

    let stored = formats.iter().fold(true, |stored, format| {
        set(format.id, &format.data) && stored
    });
    for (name, value) in [
        ("ExcludeClipboardContentFromMonitorProcessing", &[0u8][..]),
        ("CanIncludeInClipboardHistory", &[0u8; 4][..]),
        ("CanUploadToCloudClipboard", &[0u8; 4][..]),
    ] {
        let id = unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) };
        if id != 0 {
            set(id, value);
        }
    }
    stored
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

/// Pastes `text` into the focused field and restores the previous clipboard,
/// unless the user copied something new in the meantime.
pub fn paste(text: &str) -> PasteResult {
    let Some(open) = Open::new() else {
        let holder = window_process_name(unsafe { GetOpenClipboardWindow() });
        return PasteResult::Unavailable(format!("clipboard busy, held open by {holder}"));
    };
    let saved = match save(&open) {
        Ok(saved) => saved,
        Err(reason) => return PasteResult::Unavailable(reason),
    };
    if !put(&open, &[text_format(text)]) {
        return PasteResult::Failed;
    }
    drop(open);
    let ours = unsafe { GetClipboardSequenceNumber() };

    let sent = chord(VK_V);
    std::thread::sleep(PASTE_SETTLE);
    if unsafe { GetClipboardSequenceNumber() } == ours
        && let Some(open) = Open::new()
    {
        put(&open, &saved);
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
