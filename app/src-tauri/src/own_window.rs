//! Dictation into Yap's own windows (onboarding's "Try it" box, History
//! search, Settings fields).
//!
//! Text for another app has to go through the OS: find the focused field,
//! watch that it doesn't change, then paste or type. For Yap's own window
//! that detour is unnecessary and unreliable (the overlay appearing shifts
//! focus inside the window). Instead the transcript is handed to the page,
//! which inserts it at the cursor (`ui/dictate.js`) and replies. This needs
//! no platform code, so it works the same on every OS.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// The only Yap window with text fields.
pub const WINDOW: &str = "main";

/// How long the page gets to answer.
const REPLY_LIMIT: Duration = Duration::from_secs(2);

type Reply = Result<(), String>;

/// The request waiting for the page's answer: (dictation id, answer slot).
static PENDING: Mutex<Option<(u64, Sender<Reply>)>> = Mutex::new(None);

#[derive(Clone, Serialize)]
struct Request<'a> {
    id: u64,
    text: Option<&'a str>,
}

/// Whether Yap's window with text fields is the one in front.
pub fn in_front(app: &AppHandle) -> bool {
    app.get_webview_window(WINDOW)
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
}

/// Inserts `text` at the cursor in Yap's window. Blocks until the page
/// answers, so call it off the main thread.
///
/// Yap's window is the destination, so it is given keyboard focus first
/// (if Yap is still the app in front). The overlay's page can take focus
/// when it appears (see `overlay::give_back_focus`); this is the last line
/// of defence. The browser puts the cursor back in the field the user was in
/// when the page regains focus.
pub fn insert(app: &AppHandle, id: u64, text: &str) -> Reply {
    if crate::platform::app_in_front()
        && let Some(window) = app.get_webview_window(WINDOW)
    {
        let webview: &tauri::Webview = window.as_ref();
        let _ = webview.set_focus();
    }
    ask(app, "dictation-insert", id, Some(text))
}

/// Undoes insertion `id` if the field still holds exactly what Yap put
/// there. Blocks like `insert`.
pub fn undo(app: &AppHandle, id: u64) -> Reply {
    ask(app, "dictation-undo", id, None)
}

fn ask(app: &AppHandle, event: &str, id: u64, text: Option<&str>) -> Reply {
    let answer = register(id)?;
    app.emit_to(WINDOW, event, Request { id, text })
        .map_err(|error| error.to_string())?;
    answer
        .recv_timeout(REPLY_LIMIT)
        .map_err(|_| "Yap's window did not respond".to_owned())?
}

/// Replaces any older request: only one dictation runs at a time.
fn register(id: u64) -> Result<Receiver<Reply>, String> {
    let (sender, receiver) = mpsc::channel();
    *PENDING.lock().map_err(|error| error.to_string())? = Some((id, sender));
    Ok(receiver)
}

fn answer(id: u64, error: Option<String>) {
    if let Ok(mut pending) = PENDING.lock()
        && pending.as_ref().is_some_and(|(waiting, _)| *waiting == id)
        && let Some((_, sender)) = pending.take()
    {
        let _ = sender.send(error.map_or(Ok(()), Err));
    }
}

/// The page's answer to `dictation-insert` or `dictation-undo`. `detail`
/// explains a refusal for the log (focus history, element names only).
#[tauri::command]
pub fn own_window_reply(id: u64, error: Option<String>, detail: Option<String>) {
    if let Some(detail) = detail {
        crate::logfile::write(&format!("insert {id}: page: {detail}"));
    }
    answer(id, error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_reach_only_the_request_they_belong_to() {
        let first = register(1).unwrap();
        // A newer request replaces the older one.
        let second = register(2).unwrap();
        answer(1, None);
        assert!(first.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(second.recv_timeout(Duration::from_millis(20)).is_err());

        answer(2, Some("Click into a text box first".to_owned()));
        assert_eq!(
            second.recv_timeout(Duration::from_millis(20)).unwrap(),
            Err("Click into a text box first".to_owned())
        );
        // A late duplicate answer is ignored.
        answer(2, None);

        let third = register(3).unwrap();
        answer(3, None);
        assert_eq!(
            third.recv_timeout(Duration::from_millis(20)).unwrap(),
            Ok(())
        );
    }
}
