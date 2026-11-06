#[path = "../platform/windows/field_watch.rs"]
mod field_watch;
#[path = "../platform/windows/input.rs"]
mod input;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, ES_MULTILINE, ES_PASSWORD, FindWindowW, GetClassNameW,
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, MSG, PM_REMOVE, PeekMessageW,
    SendMessageW, SetForegroundWindow, TranslateMessage, WM_GETTEXT, WM_GETTEXTLENGTH, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pump() {
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn make_window() -> Result<[HWND; 4], String> {
    unsafe {
        let window = CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide("Yap controlled input test").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            500,
            260,
            null_mut(),
            null_mut(),
            null_mut(),
            null(),
        );
        let edit = wide("EDIT");
        let first = CreateWindowExW(
            0,
            edit.as_ptr(),
            wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | ES_MULTILINE as u32,
            10,
            10,
            450,
            90,
            window,
            null_mut(),
            null_mut(),
            null(),
        );
        let second = CreateWindowExW(
            0,
            edit.as_ptr(),
            wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | ES_MULTILINE as u32,
            10,
            110,
            450,
            45,
            window,
            null_mut(),
            null_mut(),
            null(),
        );
        let secure = CreateWindowExW(
            0,
            edit.as_ptr(),
            wide("").as_ptr(),
            WS_CHILD | WS_VISIBLE | ES_PASSWORD as u32,
            10,
            165,
            450,
            30,
            window,
            null_mut(),
            null_mut(),
            null(),
        );
        if [window, first, second, secure]
            .iter()
            .any(|handle| handle.is_null())
        {
            return Err(format!(
                "Could not create test fields: {}",
                std::io::Error::last_os_error()
            ));
        }

        SetFocus(first);
        Ok([window, first, second, secure])
    }
}

fn window_process() -> Result<(), String> {
    let handles = make_window()?;
    println!(
        "{} {} {} {}",
        handles[0] as usize, handles[1] as usize, handles[2] as usize, handles[3] as usize
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    loop {
        pump();
        match receiver.try_recv() {
            Ok(command) => {
                let index: usize = command.parse().map_err(|_| "Invalid field index")?;
                let field = *handles.get(index).ok_or("Invalid field index")?;
                unsafe {
                    SetFocus(field);
                }
                println!("ready");
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        thread::sleep(Duration::from_millis(5));
    }
}

struct TestWindow {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    handles: [HWND; 4],
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl TestWindow {
    fn start() -> Result<Self, String> {
        let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("--window-child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take().ok_or("No test window input")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("No test window output")?);
        let mut line = String::new();
        output
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let numbers = line
            .split_whitespace()
            .map(str::parse::<usize>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let handles: [HWND; 4] = numbers
            .into_iter()
            .map(|value| value as HWND)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "Invalid test window handles")?;
        Ok(Self {
            child,
            input,
            output,
            handles,
        })
    }

    fn focus(&mut self, index: usize) -> Result<(), String> {
        let window = self.handles[0];
        let field = self.handles[index];
        unsafe {
            SetForegroundWindow(window);
        }
        writeln!(self.input, "{index}").map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        let mut response = String::new();
        self.output
            .read_line(&mut response)
            .map_err(|error| error.to_string())?;
        if response.trim() != "ready" {
            return Err("Test window did not acknowledge focus request".to_owned());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if unsafe { GetForegroundWindow() } == window
                && input::focused_destination().is_some()
                && focused_field() == field
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let foreground = unsafe { GetForegroundWindow() };
        let mut process = 0;
        let thread = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                foreground,
                &mut process,
            )
        };
        Err(format!(
            "Test field did not become focused (foreground={foreground:?}, process={process}, thread={thread}, window={window:?}, field={field:?}); run on an unlocked interactive desktop",
        ))
    }

    fn wait_text(&self, index: usize, expected: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.text(index) == expected {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!(
            "Text mismatch: expected {expected:?}, got {:?}",
            self.text(index)
        ))
    }

    fn text(&self, index: usize) -> String {
        let field = self.handles[index];
        unsafe {
            let mut buffer = vec![0u16; SendMessageW(field, WM_GETTEXTLENGTH, 0, 0) as usize + 1];
            let count = SendMessageW(
                field,
                WM_GETTEXT,
                buffer.len(),
                buffer.as_mut_ptr() as isize,
            );
            String::from_utf16_lossy(&buffer[..count as usize])
        }
    }
}

struct ChromeTest {
    child: Child,
    directory: PathBuf,
    window: HWND,
}

impl Drop for ChromeTest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl ChromeTest {
    fn start() -> Result<Self, String> {
        let directory =
            std::env::temp_dir().join(format!("yap-chrome-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let page = directory.join("test.html");
        std::fs::write(&page, "<!doctype html><title>Yap Chrome test</title><textarea aria-label='Yap test field' autofocus></textarea><script>const field=document.querySelector('textarea'); field.onfocus=()=>document.title='Yap Chrome test focused'; field.onblur=()=>document.title='Yap Chrome test'; field.oninput=()=>document.title='Yap Chrome test MODIFIED'; field.focus();</script>")
            .map_err(|error| error.to_string())?;
        let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
        let executable = "C:/Program Files/Google/Chrome/Application/chrome.exe";
        let child = Command::new(executable)
            .arg(format!(
                "--user-data-dir={}",
                directory.join("profile").display()
            ))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg(format!("--app={url}"))
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut test = Self {
            child,
            directory,
            window: null_mut(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let window = unsafe { FindWindowW(null(), wide("Yap Chrome test focused").as_ptr()) };
            let window = if window.is_null() {
                unsafe { FindWindowW(null(), wide("Yap Chrome test").as_ptr()) }
            } else {
                window
            };
            if !window.is_null() {
                let mut process = 0;
                let mut class = [0u16; 128];
                let count =
                    unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
                unsafe { GetWindowThreadProcessId(window, &mut process) };
                if process == test.child.id()
                    && String::from_utf16_lossy(&class[..count as usize]) == "Chrome_WidgetWin_1"
                {
                    test.window = window;
                    return Ok(test);
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err("Isolated Chrome test field did not become available".to_owned())
    }

    fn focus(&self) -> Result<(), String> {
        unsafe { SetForegroundWindow(self.window) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if unsafe { GetForegroundWindow() } == self.window {
                if self.title() == "Yap Chrome test focused" {
                    return Ok(());
                }
                if self.title() == "Yap Chrome test" {
                    send_key(0x09, false)?;
                    send_key(0x09, true)?;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err("Isolated Chrome field did not receive focus".to_owned())
    }

    fn title(&self) -> String {
        let mut text = [0u16; 128];
        let count = unsafe { GetWindowTextW(self.window, text.as_mut_ptr(), text.len() as i32) };
        String::from_utf16_lossy(&text[..count as usize])
    }
}

struct TestApp {
    child: Child,
    input: ChildStdin,
    results: Receiver<String>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl TestApp {
    fn start() -> Result<Self, String> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let status = Command::new("cargo")
            .args(["build", "--offline", "--manifest-path"])
            .arg(manifest)
            .args(["--features", "input-test", "--bin", "yap-test-app"])
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err("Could not build Yap for interactive test".to_owned());
        }

        let exe =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/yap-test-app.exe");
        let mut child = Command::new(exe)
            .arg("--input-test")
            .env("YAP_INPUT_TEST", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take().ok_or("No test app input")?;
        let output = child.stdout.take().ok_or("No test app output")?;
        let (sender, results) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(result) = line.strip_prefix("YAP_TEST ") {
                    let _ = sender.send(result.to_owned());
                }
            }
        });
        let app = Self {
            child,
            input,
            results,
        };
        app.expect("ready")?;
        app.expect("hotkey_ready")?;
        Ok(app)
    }

    fn expect(&self, expected: &str) -> Result<(), String> {
        let result = self
            .results
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| format!("Yap test response timed out: {error}"))?;
        if result != expected {
            return Err(format!("Expected {expected}, got {result}"));
        }
        Ok(())
    }

    fn request(&mut self, command: &str) -> Result<String, String> {
        writeln!(self.input, "{command}").map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        self.results
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| format!("Yap test response timed out: {error}"))
    }

    fn command(&mut self, command: &str, expected: &str) -> Result<(), String> {
        let result = self.request(command)?;
        if result != expected {
            return Err(format!("Expected {expected}, got {result}"));
        }
        Ok(())
    }

    fn wait_phase(&mut self, phase: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.request("phase")? == phase {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!("Yap did not reach {phase}"))
    }
}

fn key(window: HWND, field: HWND, code: u16, release: bool) -> Result<(), String> {
    if unsafe { GetForegroundWindow() } != window || focused_field() != field {
        return Err("Test window lost focus before shortcut input".to_owned());
    }
    send_key(code, release)
}

fn send_key(code: u16, release: bool) -> Result<(), String> {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: code,
                wScan: 0,
                dwFlags: if release { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    if unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) } != 1 {
        return Err("Shortcut input submission failed".to_owned());
    }
    Ok(())
}

struct HeldShift;

impl HeldShift {
    fn press(window: HWND, field: HWND) -> Result<Self, String> {
        key(window, field, 0x10, false)?;
        Ok(Self)
    }
}

impl Drop for HeldShift {
    fn drop(&mut self) {
        let _ = send_key(0x10, true);
    }
}

fn focused_field() -> HWND {
    unsafe {
        let mut info = windows_sys::Win32::UI::WindowsAndMessaging::GUITHREADINFO {
            cbSize: std::mem::size_of::<windows_sys::Win32::UI::WindowsAndMessaging::GUITHREADINFO>(
            ) as u32,
            ..std::mem::zeroed()
        };
        let thread = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            GetForegroundWindow(),
            null_mut(),
        );
        if windows_sys::Win32::UI::WindowsAndMessaging::GetGUIThreadInfo(thread, &mut info) == 0 {
            return null_mut();
        }
        info.hwndFocus
    }
}

fn verified(original: &input::Destination, watched: bool) -> bool {
    watched
        && input::modifiers_released()
        && original.safe_to_insert(input::focused_destination().as_ref())
}

fn run() -> Result<(), String> {
    let mut test = TestWindow::start()?;
    let verifier = field_watch::FieldWatch::start();
    test.focus(1)?;
    let original = input::focused_destination().ok_or("No original field")?;
    verifier.capture(1, Some(original.clone()));
    thread::sleep(Duration::from_millis(150));
    if !verified(&original, verifier.check(1).is_ok()) {
        return Err("Native field was not verified".to_owned());
    }

    let sample = "Yap ✓\nSecond line";
    if !input::type_text(sample) {
        return Err("Unicode input submission failed".to_owned());
    }
    test.wait_text(1, "Yap ✓\r\nSecond line")?;
    println!("PASS native Unicode/multiline insertion");

    verifier.capture(2, Some(original.clone()));
    thread::sleep(Duration::from_millis(150));
    test.focus(2)?;
    thread::sleep(Duration::from_millis(150));
    let safe = verified(&original, verifier.check(2).is_ok());
    if safe || !test.text(2).is_empty() {
        return Err("Focus change did not fail closed".to_owned());
    }
    println!("PASS changed field rejected without insertion");

    test.focus(3)?;
    let password = input::focused_destination().ok_or("No secure field")?;
    verifier.capture(3, Some(password.clone()));
    thread::sleep(Duration::from_millis(150));
    if verified(&password, verifier.check(3).is_ok()) {
        return Err("Password field was accepted".to_owned());
    }
    println!("PASS password field rejected");

    let mut app = TestApp::start()?;
    test.focus(2)?;
    app.command("start", "started")?;
    app.command("start", "error: Job already active")?;
    app.command("release", "released")?;
    app.command("transcript", "transcribed")?;
    app.command("submit", "inserted")?;
    app.command("submit", "error: Job is not ready")?;
    test.wait_text(2, "Yap test ✓")?;
    println!("PASS Yap test job inserted into native field");

    app.command("start", "started")?;
    app.command("release", "released")?;
    app.command("transcript", "transcribed")?;
    test.focus(1)?;
    app.command("submit", "not_inserted")?;
    if test.text(1) != "Yap ✓\r\nSecond line" || test.text(2) != "Yap test ✓" {
        return Err("Yap inserted after the destination changed".to_owned());
    }
    println!("PASS Yap test job rejected after focus change");

    test.focus(2)?;
    app.command("start", "started")?;
    thread::sleep(Duration::from_millis(150));
    test.focus(1)?;
    thread::sleep(Duration::from_millis(150));
    test.focus(2)?;
    app.command("release", "released")?;
    app.command("transcript", "transcribed")?;
    app.command("submit", "not_inserted")?;
    if test.text(2) != "Yap test ✓" {
        return Err("Yap inserted after focus left and returned".to_owned());
    }
    println!("PASS focus leave-and-return rejected without insertion");

    test.focus(2)?;
    for phase in ["recording", "transcribing", "ready"] {
        app.command("start", "started")?;
        if phase != "recording" {
            app.command("release", "released")?;
        }
        if phase == "ready" {
            app.command("transcript", "transcribed")?;
        }
        app.command("cancel", "cancelled")?;
        app.command("late", "ignored")?;
        app.command("submit", "error: Job is not ready")?;
        if test.text(2) != "Yap test ✓" {
            return Err(format!("Yap inserted after cancellation in {phase}"));
        }
    }
    println!("PASS cancellation during recording, transcription, and pre-submission");

    test.focus(2)?;
    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    test.wait_text(2, "Yap test ✓Yap test ✓")?;
    println!("PASS injected shortcut through debug app capture and transcript path");

    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x1b, false)?;
    key(test.handles[0], test.handles[2], 0x1b, true)?;
    key(test.handles[0], test.handles[2], 0x77, true)?;
    thread::sleep(Duration::from_millis(400));
    if test.text(2) != "Yap test ✓Yap test ✓" {
        return Err("Shortcut cancellation during recording inserted text".to_owned());
    }

    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    thread::sleep(Duration::from_millis(75));
    key(test.handles[0], test.handles[2], 0x1b, false)?;
    key(test.handles[0], test.handles[2], 0x1b, true)?;
    thread::sleep(Duration::from_millis(400));
    if test.text(2) != "Yap test ✓Yap test ✓" {
        return Err("Shortcut cancellation after release inserted text".to_owned());
    }
    println!("PASS shortcut Escape cancellation before and after release");

    for fixture in ["audio_silence", "audio_bounded"] {
        app.command(fixture, fixture)?;
        key(test.handles[0], test.handles[2], 0x77, false)?;
        app.wait_phase("phase_Recording")?;
        key(test.handles[0], test.handles[2], 0x77, true)?;
        app.wait_phase("phase_Idle")?;
        if test.text(2) != "Yap test ✓Yap test ✓" {
            return Err(format!("{fixture} inserted text"));
        }
    }
    println!("PASS silence and 30-second audio bound rejected without insertion");

    for fixture in ["audio_capture_error", "audio_inference_error"] {
        app.command(fixture, fixture)?;
        key(test.handles[0], test.handles[2], 0x77, false)?;
        app.wait_phase("phase_Recording")?;
        key(test.handles[0], test.handles[2], 0x77, true)?;
        app.wait_phase("phase_Idle")?;
        if test.text(2) != "Yap test ✓Yap test ✓" {
            return Err(format!("{fixture} inserted text"));
        }
    }
    app.command("audio_speech", "audio_speech")?;
    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    test.wait_text(2, "Yap test ✓Yap test ✓Yap test ✓")?;
    println!("PASS capture/inference errors do not insert and subsequent job succeeds");

    let shift = HeldShift::press(test.handles[0], test.handles[2])?;
    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    app.wait_phase("phase_Ready")?;
    if test.text(2) != "Yap test ✓Yap test ✓Yap test ✓" {
        return Err("Yap inserted while Shift was held".to_owned());
    }
    drop(shift);
    test.wait_text(2, "Yap test ✓Yap test ✓Yap test ✓Yap test ✓")?;
    println!("PASS modifier release permits insertion after transcription");

    let shift = HeldShift::press(test.handles[0], test.handles[2])?;
    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    app.wait_phase("phase_Ready")?;
    app.wait_phase("phase_Idle")?;
    drop(shift);
    thread::sleep(Duration::from_millis(150));
    if test.text(2) != "Yap test ✓Yap test ✓Yap test ✓Yap test ✓" {
        return Err("Yap inserted after modifier wait timed out".to_owned());
    }
    println!("PASS held modifier times out without late insertion");

    let shift = HeldShift::press(test.handles[0], test.handles[2])?;
    key(test.handles[0], test.handles[2], 0x77, false)?;
    thread::sleep(Duration::from_millis(150));
    key(test.handles[0], test.handles[2], 0x77, true)?;
    app.wait_phase("phase_Ready")?;
    test.focus(1)?;
    thread::sleep(Duration::from_millis(150));
    test.focus(2)?;
    drop(shift);
    app.wait_phase("phase_Idle")?;
    if test.text(2) != "Yap test ✓Yap test ✓Yap test ✓Yap test ✓" {
        return Err("Yap inserted after focus changed while waiting for modifiers".to_owned());
    }
    println!("PASS focus leave-and-return during modifier wait rejected");
    verifier.cancel();

    let chrome = ChromeTest::start()?;
    chrome.focus()?;
    app.command("start", "started")?;
    app.command("release", "released")?;
    app.command("transcript", "transcribed")?;
    app.command("submit", "inserted")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while chrome.title() != "Yap Chrome test MODIFIED" && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if chrome.title() != "Yap Chrome test MODIFIED" {
        return Err("Chrome test field did not receive transcription".to_owned());
    }
    println!("PASS isolated Chrome textarea receives transcription");
    Ok(())
}

fn main() {
    let result = if std::env::args().any(|arg| arg == "--window-child") {
        window_process()
    } else {
        run()
    };
    if let Err(error) = result {
        eprintln!("FAIL {error}");
        std::process::exit(1);
    }
}
