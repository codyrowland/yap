use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_application_services::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
};

use super::SYNTHETIC_MARKER;

/// How long Yap waits for another app to answer an accessibility query.
const AX_TIMEOUT_SECONDS: f32 = 0.5;
/// Characters per synthetic key event; macOS truncates longer strings.
const TYPING_CHUNK: usize = 20;

pub(super) type Element = CFRetained<AXUIElement>;

#[derive(Clone)]
pub struct Destination {
    pid: i32,
    window: Option<Element>,
    password: bool,
}

// AXUIElement references are plain CF objects; macOS lets any thread use them.
unsafe impl Send for Destination {}
unsafe impl Sync for Destination {}

impl PartialEq for Destination {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.password == other.password
            && match (&self.window, &other.window) {
                (Some(left), Some(right)) => **left == **right,
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for Destination {}

impl std::fmt::Debug for Destination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Destination")
            .field("pid", &self.pid)
            .field("window", &self.window.is_some())
            .field("password", &self.password)
            .finish()
    }
}

impl Destination {
    pub fn app_name(&self) -> String {
        autoreleasepool(|_| {
            NSRunningApplication::runningApplicationWithProcessIdentifier(self.pid)
                .and_then(|app| app.localizedName())
                .map(|name| name.to_string())
                .unwrap_or_else(|| "Unknown app".to_owned())
        })
    }

    pub fn safe_to_insert(&self, current: Option<&Destination>) -> bool {
        !self.password && Some(self) == current
    }

    pub(super) fn is_password(&self) -> bool {
        self.password
    }

    pub(super) fn pid(&self) -> i32 {
        self.pid
    }
}

pub(super) fn attribute(element: &AXUIElement, name: &'static str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = std::ptr::null();
    let error = unsafe {
        element.copy_attribute_value(&CFString::from_static_str(name), NonNull::from(&mut value))
    };
    if error.0 != 0 {
        return None;
    }

    NonNull::new(value.cast_mut()).map(|value| unsafe { CFRetained::from_raw(value) })
}

pub(super) fn element(parent: &AXUIElement, name: &'static str) -> Option<Element> {
    attribute(parent, name)?.downcast::<AXUIElement>().ok()
}

pub(super) fn text(element: &AXUIElement, name: &'static str) -> Option<String> {
    attribute(element, name)?
        .downcast::<CFString>()
        .ok()
        .map(|value| value.to_string())
}

pub(super) fn settable(element: &AXUIElement, name: &'static str) -> bool {
    let mut settable = 0_u8;
    let error = unsafe {
        element.is_attribute_settable(
            &CFString::from_static_str(name),
            NonNull::from(&mut settable),
        )
    };
    error.0 == 0 && settable != 0
}

pub(super) fn is_password_field(element: &AXUIElement) -> bool {
    text(element, "AXSubrole").as_deref() == Some("AXSecureTextField")
}

pub(super) fn focused_application() -> Option<(Element, i32)> {
    let system = unsafe { AXUIElement::new_system_wide() };
    unsafe { system.set_messaging_timeout(AX_TIMEOUT_SECONDS) };
    let app = element(&system, "AXFocusedApplication")?;
    let mut pid = 0;
    if unsafe { app.pid(NonNull::from(&mut pid)) }.0 != 0 {
        return None;
    }

    Some((app, pid))
}

pub fn accessibility_ready() -> bool {
    static PROMPTED: AtomicBool = AtomicBool::new(false);
    if PROMPTED.swap(true, Ordering::Relaxed) {
        return unsafe { AXIsProcessTrusted() };
    }

    let key = unsafe { kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) }
}

fn frontmost_pid() -> i32 {
    autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map_or(0, |app| app.processIdentifier())
    })
}

pub fn foreground_window() -> isize {
    frontmost_pid().max(0) as isize
}

pub fn app_in_front() -> bool {
    frontmost_pid() == std::process::id() as i32
}

pub fn focused_destination() -> Option<Destination> {
    let (app, pid) = focused_application()?;
    let window = element(&app, "AXFocusedWindow");
    let password =
        element(&app, "AXFocusedUIElement").is_some_and(|field| is_password_field(&field));
    Some(Destination {
        pid,
        window,
        password,
    })
}

pub fn modifiers_released() -> bool {
    let held = CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState);
    !held.intersects(
        CGEventFlags::MaskShift
            | CGEventFlags::MaskControl
            | CGEventFlags::MaskAlternate
            | CGEventFlags::MaskCommand,
    )
}

/// A synthetic key event marked as Yap's own.
pub(super) fn key_event(code: u16, down: bool, flags: CGEventFlags) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new_keyboard_event(None, code, down)?;
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::EventSourceUserData,
        SYNTHETIC_MARKER,
    );
    Some(event)
}

fn type_chunk(units: &[u16]) -> bool {
    for down in [true, false] {
        let Some(event) = key_event(0, down, CGEventFlags(0)) else {
            return false;
        };
        unsafe {
            CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr());
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    true
}

pub fn type_text(text: &str) -> bool {
    let mut chunk = Vec::with_capacity(TYPING_CHUNK + 1);
    let mut buffer = [0_u16; 2];
    for character in text.chars() {
        let units = character.encode_utf16(&mut buffer);
        if chunk.len() + units.len() > TYPING_CHUNK {
            if !type_chunk(&chunk) {
                return false;
            }
            chunk.clear();
        }
        chunk.extend_from_slice(units);
    }
    chunk.is_empty() || type_chunk(&chunk)
}
