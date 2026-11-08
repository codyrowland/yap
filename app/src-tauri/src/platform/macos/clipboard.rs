//! Paste-based insertion: put the transcript on the clipboard, press Cmd+V,
//! then put back what the user had copied.
//!
//! Only text can be restored. When the clipboard holds files or images,
//! pasting would destroy them, so callers fall back to typing.

use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};
use objc2_foundation::{NSArray, NSString};

use super::input::key_event;
use crate::platform::PasteResult;

const KEY_V: u16 = 0x09;
const KEY_Z: u16 = 0x06;
/// How long the target app gets to read the clipboard before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(250);
/// Tells clipboard managers not to record this content (nspasteboard.org).
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";
const NOT_RESTORABLE: [&str; 2] = ["public.file-url", "public.image"];

enum Saved {
    Empty,
    Text(String),
}

fn save(board: &NSPasteboard) -> Option<Saved> {
    if board.types().is_none_or(|types| types.count() == 0) {
        return Some(Saved::Empty);
    }

    let blocked = NSArray::from_retained_slice(&NOT_RESTORABLE.map(NSString::from_str));
    if board.canReadItemWithDataConformingToTypes(&blocked) {
        return None;
    }

    board
        .stringForType(unsafe { NSPasteboardTypeString })
        .map(|text| Saved::Text(text.to_string()))
}

fn put(board: &NSPasteboard, text: &str) -> bool {
    board.clearContents();
    let written =
        board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    board.setString_forType(&NSString::from_str(""), &NSString::from_str(TRANSIENT_TYPE));
    written
}

fn chord(code: u16) -> bool {
    for down in [true, false] {
        let Some(event) = key_event(code, down, CGEventFlags::MaskCommand) else {
            return false;
        };
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    true
}

/// Pastes `text` into the focused field and restores the previous clipboard
/// text, unless the user copied something new in the meantime.
pub fn paste(text: &str) -> PasteResult {
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let Some(saved) = save(&board) else {
            return PasteResult::Unavailable;
        };
        if !put(&board, text) {
            return PasteResult::Failed;
        }
        let ours = board.changeCount();

        let sent = chord(KEY_V);
        std::thread::sleep(PASTE_SETTLE);
        if board.changeCount() == ours {
            match &saved {
                Saved::Empty => {
                    board.clearContents();
                }
                Saved::Text(previous) => {
                    put(&board, previous);
                }
            }
        }

        if sent {
            PasteResult::Pasted
        } else {
            PasteResult::Failed
        }
    })
}

/// Sends Cmd+Z to the focused app.
pub fn undo() -> bool {
    chord(KEY_Z)
}
