#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
use windows as imp;

use std::process::ExitStatus;

use tauri::WebviewWindow;
use yap_app::credential_store::CredentialStore;

pub use imp::FILE_MANAGER;
pub use imp::field_watch::FieldWatch;
pub use imp::helper_process::HelperProcess;
pub use imp::input::Destination;
pub use imp::keyboard_hook;

pub const BACKGROUND_ARG: &str = "--background";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowId(isize);

pub enum PasteResult {
    Pasted,
    /// The clipboard held something that cannot be restored; nothing was sent.
    Unavailable,
    Failed,
}

pub fn focused_destination() -> Option<Destination> {
    imp::input::focused_destination()
}

pub fn still_focused(original: &Destination) -> bool {
    modifiers_released() && original.safe_to_insert(focused_destination().as_ref())
}

pub fn foreground_window() -> Option<WindowId> {
    Some(imp::input::foreground_window())
        .filter(|window| *window != 0)
        .map(WindowId)
}

pub fn app_in_front() -> bool {
    imp::input::app_in_front()
}

pub fn modifiers_released() -> bool {
    imp::input::modifiers_released()
}

pub fn type_text(text: &str) -> bool {
    imp::input::type_text(text)
}

pub fn paste(text: &str) -> PasteResult {
    imp::clipboard::paste(text)
}

pub fn undo() -> bool {
    imp::clipboard::undo()
}

pub fn accessibility_ready() -> bool {
    imp::input::accessibility_ready()
}

pub fn mute_speakers() -> Option<String> {
    imp::speakers::mute()
}

pub fn unmute_speakers(device: &str) {
    imp::speakers::unmute(device);
}

pub fn credential_store() -> &'static dyn CredentialStore {
    imp::credentials::store()
}

pub fn set_open_at_login(enabled: bool) -> Result<(), String> {
    imp::autostart::set(enabled)
}

pub fn launched_in_background() -> bool {
    std::env::args().any(|arg| arg == BACKGROUND_ARG)
}

pub fn set_visible_without_focus(window: &WebviewWindow, visible: bool) {
    imp::window::set_visible_without_focus(window, visible);
}

pub fn is_active_window(window: &WebviewWindow) -> bool {
    imp::window::is_active(window)
}

pub fn activate_window(window: &WebviewWindow) {
    imp::window::activate(window);
}

pub fn missing_runtime(exit: &ExitStatus) -> Option<&'static str> {
    imp::helper_process::missing_runtime(exit)
}
