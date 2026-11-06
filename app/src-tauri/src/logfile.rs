//! A small diagnostic log, `yap.log` in Yap's data folder, so problems on a
//! user's machine can be traced: talk-key decisions, errors shown to the
//! user, and engine start-ups. Never contains transcripts or credentials.
//! Rotated to `yap.old.log` at 1 MB.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 1_048_576;

static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut current) = PATH.lock() {
        *current = Some(path);
    }
    write(&format!("Yap {} started", env!("CARGO_PKG_VERSION")));
}

pub fn write(message: &str) {
    let Ok(guard) = PATH.lock() else {
        return;
    };
    let Some(path) = guard.as_ref() else {
        return;
    };
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_file_name("yap.old.log"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{} {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            message.replace(['\r', '\n'], " ")
        );
    }
}
