//! "Open at login": a value under the current user's Run key that starts Yap
//! in the tray (`--background`) when they sign in.

use crate::platform::BACKGROUND_ARG;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Yap";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn set(enabled: bool) -> Result<(), String> {
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Err("Could not open the Windows startup settings".to_owned());
    }

    let name = wide(VALUE_NAME);
    let status = if enabled {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let command = wide(&format!("\"{}\" {BACKGROUND_ARG}", exe.display()));
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        match unsafe { RegDeleteValueW(key, name.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
            other => other,
        }
    };
    unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return Err("Could not update the Windows startup settings".to_owned());
    }

    Ok(())
}
