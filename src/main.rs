mod uia_probe;

use std::mem::size_of;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
    VK_ESCAPE, VK_F8,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, ES_PASSWORD, GUITHREADINFO, GWL_STYLE, GetClassNameW,
    GetForegroundWindow, GetGUIThreadInfo, GetWindowLongW, GetWindowThreadProcessId,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_REMOVE, PeekMessageW, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Destination {
    window: isize,
    field: isize,
    process: u32,
    class: String,
    password: bool,
}

impl Destination {
    fn is_native_edit(&self) -> bool {
        self.class.eq_ignore_ascii_case("Edit") || self.class.eq_ignore_ascii_case("RichEditD2DPT")
    }

    fn safe_to_insert(&self, current: Option<&Self>) -> bool {
        !self.password && self.is_native_edit() && current == Some(self)
    }
}

#[derive(Default)]
struct State {
    held: bool,
    destination: Option<Destination>,
    pending: Option<Instant>,
    waiting_for_uia: bool,
    focus_changed: bool,
    insertion_attempted: bool,
    job_id: u64,
}

impl State {
    fn observe_focus(&mut self, current: Option<&Destination>) {
        if (self.held || self.pending.is_some() || self.waiting_for_uia)
            && self.destination.as_ref() != current
        {
            self.focus_changed = true;
        }
    }

    fn finish_uia(
        &mut self,
        outcome: &uia_probe::Outcome,
        current: Option<&Destination>,
    ) -> Option<bool> {
        self.observe_focus(current);
        if !self.waiting_for_uia || self.job_id != outcome.job_id {
            return None;
        }

        let original = self.destination.take();
        self.waiting_for_uia = false;
        Some(
            outcome.eligible
                && !self.focus_changed
                && original.as_ref() == current
                && original.as_ref().is_some_and(|field| !field.password),
        )
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn destination() -> Option<Destination> {
    focused_in(unsafe { GetForegroundWindow() })
}

fn focused_in(window: windows_sys::Win32::Foundation::HWND) -> Option<Destination> {
    unsafe {
        if window.is_null() {
            return None;
        }

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
        let password = (name.eq_ignore_ascii_case("Edit")
            || name.eq_ignore_ascii_case("RichEditD2DPT"))
            && (GetWindowLongW(info.hwndFocus, GWL_STYLE) & ES_PASSWORD != 0);
        Some(Destination {
            window: window as isize,
            field: info.hwndFocus as isize,
            process,
            class: name,
            password,
        })
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
        if key.flags & LLKHF_INJECTED == 0 {
            let down = message as u32 == WM_KEYDOWN || message as u32 == WM_SYSKEYDOWN;
            let up = message as u32 == WM_KEYUP || message as u32 == WM_SYSKEYUP;
            let mut state = STATE.get().unwrap().lock().unwrap();
            if key.vkCode == VK_ESCAPE as u32
                && down
                && (state.held || state.pending.is_some() || state.waiting_for_uia)
            {
                state.held = false;
                state.pending = None;
                state.waiting_for_uia = false;
                state.destination = None;
                state.focus_changed = false;
                println!("cancelled");
                return 1;
            } else if key.vkCode == VK_F8 as u32
                && down
                && !state.held
                && state.pending.is_none()
                && !state.waiting_for_uia
            {
                state.held = true;
                state.job_id += 1;
                state.focus_changed = false;
                state.insertion_attempted = false;
                state.destination = destination();
                println!("F8 down: {:?}", state.destination);
            } else if key.vkCode == VK_F8 as u32 && up && state.held {
                state.held = false;
                state.pending = Some(Instant::now() + Duration::from_secs(1));
                println!("F8 up: checking destination in one second");
            }

            if key.vkCode == VK_F8 as u32 && (down || up) {
                return 1;
            }
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

fn insert_probe_text() -> bool {
    let mut inputs = Vec::new();
    for unit in "Voice probe ✓".encode_utf16() {
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

fn main() {
    let insert = std::env::args().any(|arg| arg == "--insert");
    let inspect = std::env::args().any(|arg| arg == "--inspect-uia");
    let (ui_sender, ui_results) = uia_probe::start(inspect);
    let mut observed_job = 0;
    let mut ui_active = false;
    let mut ui_deadline: Option<Instant> = None;
    STATE
        .set(Mutex::new(State::default()))
        .unwrap_or_else(|_| panic!("state already initialized"));
    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0);
        if hook.is_null() {
            panic!(
                "could not install keyboard hook: {}",
                std::io::Error::last_os_error()
            );
        }

        println!(
            "Focus a textbox, hold/release F8, then switch focus during the one-second delay. Escape cancels. Ctrl+C quits."
        );
        println!(
            "Insertion {} (only into unchanged eligible text fields).",
            if insert { "enabled" } else { "disabled" }
        );
        loop {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            {
                let state = STATE.get().unwrap().lock().unwrap();
                if state.held && state.job_id != observed_job {
                    observed_job = state.job_id;
                    ui_active = ui_sender
                        .send(uia_probe::Command::Capture(state.job_id))
                        .is_ok();
                } else if ui_active
                    && !state.held
                    && state.pending.is_none()
                    && !state.waiting_for_uia
                {
                    let _ = ui_sender.send(uia_probe::Command::Cancel);
                    ui_active = false;
                }
            }

            let current = destination();
            let ready = {
                let mut state = STATE.get().unwrap().lock().unwrap();
                state.observe_focus(current.as_ref());
                if state
                    .pending
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    state.pending = None;
                    Some((state.job_id, state.destination.clone(), state.focus_changed))
                } else {
                    None
                }
            };
            if let Some((job_id, original, focus_changed)) = ready {
                if !focus_changed
                    && original
                        .as_ref()
                        .is_some_and(|field| field.safe_to_insert(current.as_ref()))
                {
                    {
                        let mut state = STATE.get().unwrap().lock().unwrap();
                        state.destination = None;
                        state.insertion_attempted = insert;
                    }
                    let _ = ui_sender.send(uia_probe::Command::Cancel);
                    ui_active = false;
                    println!("same native edit field: {:?}", current);
                    if insert {
                        println!("SendInput submitted all events: {}", insert_probe_text());
                    }
                } else if !focus_changed
                    && original.as_ref() == current.as_ref()
                    && original.as_ref().is_some_and(|field| !field.password)
                    && ui_active
                    && ui_sender.send(uia_probe::Command::Check(job_id)).is_ok()
                {
                    STATE.get().unwrap().lock().unwrap().waiting_for_uia = true;
                    ui_deadline = Some(Instant::now() + Duration::from_secs(2));
                } else {
                    let mut state = STATE.get().unwrap().lock().unwrap();
                    state.destination = None;
                    let _ = ui_sender.send(uia_probe::Command::Cancel);
                    ui_active = false;
                    println!(
                        "recovery required: original={original:?}, current={current:?}, focus_changed={focus_changed}"
                    );
                }
            }

            while let Ok(outcome) = ui_results.try_recv() {
                let current = destination();
                let eligible = {
                    let mut state = STATE.get().unwrap().lock().unwrap();
                    let result = state.finish_uia(&outcome, current.as_ref());
                    if result == Some(true) {
                        state.insertion_attempted = insert;
                    }

                    result
                };
                if let Some(eligible) = eligible {
                    ui_deadline = None;
                    ui_active = false;
                    if eligible {
                        println!("same UIA edit field: {:?}", current);
                        if insert {
                            println!("SendInput submitted all events: {}", insert_probe_text());
                        }
                    } else {
                        println!("recovery required: UIA destination could not be verified");
                    }
                }
            }

            if ui_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let mut state = STATE.get().unwrap().lock().unwrap();
                if state.waiting_for_uia {
                    state.waiting_for_uia = false;
                    state.destination = None;
                    println!("recovery required: UIA check timed out");
                }

                ui_deadline = None;
                ui_active = false;
                let _ = ui_sender.send(uia_probe::Command::Cancel);
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetWindowTextW, SW_SHOW, SetForegroundWindow, ShowWindow,
        WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn test_window() -> TestWindow {
        let window = unsafe {
            CreateWindowExW(
                0,
                wide("Static").as_ptr(),
                wide("Voice input test").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                400,
                240,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null(), "failed to create test window");
        TestWindow(window)
    }

    unsafe fn test_edit(parent: HWND, style: u32) -> HWND {
        unsafe {
            CreateWindowExW(
                0,
                wide("Edit").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                300,
                40,
                parent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        }
    }

    fn field(class: &str, password: bool) -> Destination {
        Destination {
            window: 1,
            field: 2,
            process: 3,
            class: class.into(),
            password,
        }
    }

    #[test]
    fn only_unchanged_native_non_password_edit_is_eligible() {
        let edit = field("Edit", false);
        assert!(edit.safe_to_insert(Some(&edit)));
        let notepad = field("RichEditD2DPT", false);
        assert!(notepad.safe_to_insert(Some(&notepad)));
        assert!(!field("RichEditD2DPT", true).safe_to_insert(Some(&field("RichEditD2DPT", true))));
        assert!(!edit.safe_to_insert(None));
        assert!(!edit.safe_to_insert(Some(&Destination {
            field: 4,
            ..edit.clone()
        })));
        assert!(!field("Edit", true).safe_to_insert(Some(&field("Edit", true))));
        assert!(
            !field("Chrome_RenderWidgetHostHWND", false)
                .safe_to_insert(Some(&field("Chrome_RenderWidgetHostHWND", false)))
        );
    }

    #[test]
    fn shortcut_is_consumed_and_escape_cancels() {
        assert!(STATE.set(Mutex::new(State::default())).is_ok());
        let mut key = KBDLLHOOKSTRUCT {
            vkCode: VK_F8 as u32,
            scanCode: 0,
            flags: 0,
            time: 0,
            dwExtraInfo: 0,
        };
        let data = &key as *const KBDLLHOOKSTRUCT as LPARAM;
        assert_eq!(unsafe { keyboard_hook(0, WM_KEYDOWN as WPARAM, data) }, 1);
        assert!(STATE.get().unwrap().lock().unwrap().held);
        assert_eq!(unsafe { keyboard_hook(0, WM_KEYUP as WPARAM, data) }, 1);
        assert!(STATE.get().unwrap().lock().unwrap().pending.is_some());
        key.vkCode = VK_ESCAPE as u32;
        let data = &key as *const KBDLLHOOKSTRUCT as LPARAM;
        assert_eq!(unsafe { keyboard_hook(0, WM_KEYDOWN as WPARAM, data) }, 1);
        assert!(STATE.get().unwrap().lock().unwrap().pending.is_none());
    }

    #[test]
    fn focus_change_during_pending_job_is_sticky() {
        let edit = field("Edit", false);
        let mut state = State {
            destination: Some(edit.clone()),
            pending: Some(Instant::now() + Duration::from_secs(1)),
            ..State::default()
        };
        state.observe_focus(Some(&edit));
        assert!(!state.focus_changed);
        state.observe_focus(None);
        state.observe_focus(Some(&edit));
        assert!(state.focus_changed);
    }

    #[test]
    fn uia_result_cannot_insert_after_cancel_or_focus_change() {
        let edit = field("Chrome_WidgetWin_1", false);
        let outcome = uia_probe::Outcome {
            job_id: 7,
            eligible: true,
        };
        let mut state = State {
            job_id: 7,
            destination: Some(edit.clone()),
            waiting_for_uia: true,
            ..State::default()
        };
        assert_eq!(
            state.finish_uia(
                &uia_probe::Outcome {
                    job_id: 6,
                    eligible: true
                },
                Some(&edit)
            ),
            None
        );
        assert_eq!(state.finish_uia(&outcome, None), Some(false));

        state.destination = Some(edit.clone());
        state.waiting_for_uia = true;
        state.focus_changed = false;
        assert_eq!(state.finish_uia(&outcome, Some(&edit)), Some(true));
        assert_eq!(state.finish_uia(&outcome, Some(&edit)), None);

        state.destination = None;
        state.waiting_for_uia = false;
        assert_eq!(state.finish_uia(&outcome, Some(&edit)), None);
    }

    #[test]
    fn native_edit_focus_change_and_password_detection() {
        unsafe {
            let window = test_window();
            let edit = test_edit(window.0, 0);
            let other = test_edit(window.0, 0);
            let password = test_edit(window.0, ES_PASSWORD as u32);
            assert!(!edit.is_null() && !other.is_null() && !password.is_null());
            SetFocus(edit);
            let original = focused_in(window.0).expect("native focused edit should be detectable");
            assert_eq!(original.field, edit as isize);
            assert!(original.safe_to_insert(focused_in(window.0).as_ref()));
            SetFocus(other);
            assert!(!original.safe_to_insert(focused_in(window.0).as_ref()));
            SetFocus(password);
            let secure = focused_in(window.0).expect("native password field should be detectable");
            assert!(secure.password);
            assert!(!secure.safe_to_insert(Some(&secure)));
        }
    }

    #[test]
    #[ignore = "requires activating a test window on an interactive Windows desktop"]
    fn unicode_insertion_into_native_edit() {
        unsafe {
            let window = test_window();
            let edit = test_edit(window.0, 0);
            assert!(!edit.is_null());
            ShowWindow(window.0, SW_SHOW);
            assert_ne!(
                SetForegroundWindow(window.0),
                0,
                "test window must be foreground"
            );
            SetFocus(edit);
            let original = destination().expect("native focused edit should be detectable");
            assert!(original.safe_to_insert(destination().as_ref()));
            assert!(insert_probe_text());
            let mut message: MSG = std::mem::zeroed();
            for _ in 0..100 {
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut text = [0u16; 64];
            let count = GetWindowTextW(edit, text.as_mut_ptr(), text.len() as i32);
            assert_eq!(
                String::from_utf16_lossy(&text[..count as usize]),
                "Voice probe ✓"
            );
        }
    }
}
