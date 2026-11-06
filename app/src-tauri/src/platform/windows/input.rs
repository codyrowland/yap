use std::mem::size_of;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ES_PASSWORD, GUITHREADINFO, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowLongW, GetWindowThreadProcessId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    window: isize,
    field: isize,
    process: u32,
    class: String,
    password: bool,
}

impl Destination {
    pub fn app_name(&self) -> String {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.process);
            if handle.is_null() {
                return "Unknown app".to_owned();
            }

            let mut path = [0u16; 1024];
            let mut len = path.len() as u32;
            let found = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut len) != 0;
            CloseHandle(handle);
            if !found {
                return "Unknown app".to_owned();
            }

            std::path::Path::new(&String::from_utf16_lossy(&path[..len as usize]))
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unknown app".to_owned())
        }
    }

    pub fn safe_to_insert(&self, current: Option<&Destination>) -> bool {
        !self.password && Some(self) == current
    }

    pub(super) fn is_password(&self) -> bool {
        self.password
    }

    pub(super) fn native_edit(&self) -> bool {
        self.class.eq_ignore_ascii_case("Edit") || self.class.eq_ignore_ascii_case("RichEditD2DPT")
    }
}

pub fn accessibility_ready() -> bool {
    std::thread::spawn(|| unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return false;
        }

        let ready =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .is_ok();
        CoUninitialize();
        ready
    })
    .join()
    .unwrap_or(false)
}

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

pub fn app_in_front() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return false;
        }

        let mut process = 0;
        GetWindowThreadProcessId(window, &mut process);
        process == std::process::id()
    }
}

pub fn focused_destination() -> Option<Destination> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }

        // Yap's own windows are valid targets too (onboarding's "Try it" box,
        // History search). Their sensitive fields are still refused: API-key
        // and token inputs are password fields, and the talk-key field is a
        // button, not an edit control (see `Verifier`).
        let mut process = 0;
        let thread = GetWindowThreadProcessId(window, &mut process);

        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..std::mem::zeroed()
        };
        if thread == 0 || GetGUIThreadInfo(thread, &mut info) == 0 || info.hwndFocus.is_null() {
            return None;
        }

        let mut class = [0u16; 256];
        let count = GetClassNameW(info.hwndFocus, class.as_mut_ptr(), class.len() as i32);
        if count == 0 {
            return None;
        }

        let name = String::from_utf16_lossy(&class[..count as usize]);
        let native_edit =
            name.eq_ignore_ascii_case("Edit") || name.eq_ignore_ascii_case("RichEditD2DPT");
        Some(Destination {
            window: window as isize,
            field: info.hwndFocus as isize,
            process,
            password: native_edit && GetWindowLongW(info.hwndFocus, GWL_STYLE) & ES_PASSWORD != 0,
            class: name,
        })
    }
}

pub fn modifiers_released() -> bool {
    unsafe {
        [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .all(|key| GetAsyncKeyState(*key as i32) & i16::MIN == 0)
    }
}

pub fn type_text(text: &str) -> bool {
    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }

    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) == inputs.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_requires_the_same_nonsecure_field() {
        let original = Destination {
            window: 1,
            field: 2,
            process: 3,
            class: "Edit".into(),
            password: false,
        };
        assert!(original.safe_to_insert(Some(&original)));
        assert!(!original.safe_to_insert(None));
        assert!(!original.safe_to_insert(Some(&Destination {
            field: 4,
            ..original.clone()
        })));
        let secure = Destination {
            password: true,
            ..original.clone()
        };
        assert!(!secure.safe_to_insert(Some(&secure)));
        let browser = Destination {
            class: "Chrome_WidgetWin_1".into(),
            ..original
        };
        assert!(browser.safe_to_insert(Some(&browser)));
    }

    #[test]
    fn native_edit_policy_excludes_unknown_controls() {
        let target = Destination {
            window: 1,
            field: 2,
            process: 3,
            class: "RichEditD2DPT".into(),
            password: false,
        };
        assert!(target.native_edit());
        assert!(
            !Destination {
                class: "Chrome_WidgetWin_1".into(),
                ..target
            }
            .native_edit()
        );
    }
}
