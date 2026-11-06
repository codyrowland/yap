use std::sync::Mutex;

use tauri::State;
use yap_app::history::{Entry, History};

use crate::state::lock;

#[tauri::command]
pub fn history_entries(history: State<'_, Mutex<History>>) -> Result<Vec<Entry>, String> {
    lock(&history)?.entries()
}

#[tauri::command]
pub fn delete_history(id: u64, history: State<'_, Mutex<History>>) -> Result<(), String> {
    lock(&history)?.remove(id)
}
