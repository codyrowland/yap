use std::mem::size_of;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, UIA_EditControlTypeId,
};
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

    fn native_edit(&self) -> bool {
        self.class.eq_ignore_ascii_case("Edit") || self.class.eq_ignore_ascii_case("RichEditD2DPT")
    }
}

pub fn destination_verified(
    original: Option<&Destination>,
    current: Option<&Destination>,
    verified: bool,
    modifiers_released: bool,
) -> bool {
    verified
        && modifiers_released
        && original.is_some_and(|field| !field.password && Some(field) == current)
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

pub fn foreground_window_handle() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

pub fn app_is_foreground() -> bool {
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

        let mut process = 0;
        let thread = GetWindowThreadProcessId(window, &mut process);
        if process == std::process::id() {
            return None;
        }

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

fn same_element(automation: &IUIAutomation, original: &IUIAutomationElement) -> bool {
    unsafe {
        automation
            .GetFocusedElement()
            .ok()
            .and_then(|current| automation.CompareElements(original, &current).ok())
            .is_some_and(|same| same.as_bool())
    }
}

enum Command {
    Capture(u64, Option<Destination>),
    Check(u64),
    Cancel,
}

pub struct Outcome {
    pub id: u64,
    pub verified: bool,
}

pub struct Verifier {
    commands: Sender<Command>,
    results: Receiver<Outcome>,
}

impl Verifier {
    pub fn start() -> Self {
        let (commands, requests) = mpsc::channel();
        let (responses, results) = mpsc::channel();
        std::thread::spawn(move || unsafe {
            if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                return;
            }

            let automation: windows::core::Result<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
            if let Ok(automation) = automation {
                let mut original: Option<Destination> = None;
                let mut element: Option<IUIAutomationElement> = None;
                let mut active_id = 0;
                let mut changed = false;
                loop {
                    match requests.recv_timeout(Duration::from_millis(50)) {
                        Ok(Command::Capture(id, target)) => {
                            active_id = id;
                            changed = target.is_none() || target != focused_destination();
                            original = target;
                            element = None;
                            if let Some(target) = original.as_ref()
                                && !target.native_edit()
                                && !changed
                                && let Ok(focused) = automation.GetFocusedElement()
                                && focused.CurrentControlType() == Ok(UIA_EditControlTypeId)
                                && focused
                                    .CurrentIsPassword()
                                    .is_ok_and(|flag| !flag.as_bool())
                                && focused
                                    .CurrentHasKeyboardFocus()
                                    .is_ok_and(|flag| flag.as_bool())
                            {
                                element = Some(focused);
                            }
                        }
                        Ok(Command::Check(id)) => {
                            let verified = id == active_id
                                && !changed
                                && original.as_ref().is_some_and(|target| {
                                    !target.password
                                        && Some(target) == focused_destination().as_ref()
                                        && (target.native_edit()
                                            || element.as_ref().is_some_and(|first| {
                                                same_element(&automation, first)
                                            }))
                                });
                            let _ = responses.send(Outcome { id, verified });
                            original = None;
                            element = None;
                        }
                        Ok(Command::Cancel) => {
                            original = None;
                            element = None;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Some(target) = original.as_ref()
                                && (Some(target) != focused_destination().as_ref()
                                    || (!target.native_edit()
                                        && !element
                                            .as_ref()
                                            .is_some_and(|first| same_element(&automation, first))))
                            {
                                changed = true;
                                original = None;
                                element = None;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }

            CoUninitialize();
        });
        Self { commands, results }
    }

    pub fn capture(&self, id: u64, target: Option<Destination>) {
        let _ = self.commands.send(Command::Capture(id, target));
    }

    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel);
    }

    pub fn check(&self, id: u64) -> bool {
        if self.commands.send(Command::Check(id)).is_err() {
            return false;
        }

        self.results
            .recv_timeout(Duration::from_secs(2))
            .is_ok_and(|outcome| outcome.id == id && outcome.verified)
    }
}

pub fn modifiers_released() -> bool {
    unsafe {
        [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .all(|key| GetAsyncKeyState(*key as i32) & i16::MIN == 0)
    }
}

pub fn insert_unicode(text: &str) -> bool {
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
    fn insertion_requires_the_same_nonsecure_field_and_released_modifiers() {
        let original = Destination {
            window: 1,
            field: 2,
            process: 3,
            class: "Edit".into(),
            password: false,
        };
        assert!(destination_verified(
            Some(&original),
            Some(&original),
            true,
            true
        ));
        assert!(!destination_verified(Some(&original), None, true, true));
        assert!(!destination_verified(
            Some(&original),
            Some(&Destination {
                field: 4,
                ..original.clone()
            }),
            true,
            true
        ));
        assert!(!destination_verified(
            Some(&original),
            Some(&original),
            false,
            true
        ));
        assert!(!destination_verified(
            Some(&original),
            Some(&original),
            true,
            false
        ));
        let secure = Destination {
            password: true,
            ..original.clone()
        };
        assert!(!destination_verified(
            Some(&secure),
            Some(&secure),
            true,
            true
        ));
        let browser = Destination {
            class: "Chrome_WidgetWin_1".into(),
            ..original
        };
        assert!(destination_verified(
            Some(&browser),
            Some(&browser),
            true,
            true
        ));
        assert!(!destination_verified(
            Some(&browser),
            Some(&browser),
            false,
            true
        ));
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
