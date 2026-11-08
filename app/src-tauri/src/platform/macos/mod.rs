pub mod autostart;
pub mod clipboard;
pub mod credentials;
pub mod field_watch;
pub use super::unix::helper_process;
pub mod input;
pub mod keyboard_hook;
pub mod speakers;
pub mod window;

pub const FILE_MANAGER: &str = "open";

/// Marks key events Yap posts itself, so the talk-key tap ignores them.
const SYNTHETIC_MARKER: i64 = 0x59_41_50;
