#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub use imp::field_watch::FieldWatch;
pub use imp::input::Destination;
pub use imp::keyboard_hook;

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
