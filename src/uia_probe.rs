use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextEditPattern,
    IUIAutomationValuePattern, UIA_EditControlTypeId, UIA_TextEditPatternId, UIA_ValuePatternId,
};

pub enum Command {
    Capture(u64),
    Check(u64),
    Cancel,
}

pub struct Outcome {
    pub job_id: u64,
    pub eligible: bool,
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

pub fn start(inspect: bool) -> (Sender<Command>, Receiver<Outcome>) {
    let (sender, commands) = mpsc::channel();
    let (outcomes, receiver) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            println!("UIA: COM initialization failed");
            return;
        }

        let automation: windows::core::Result<IUIAutomation> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        match automation {
            Ok(automation) => {
                let mut original: Option<IUIAutomationElement> = None;
                let mut active_id = 0;
                let mut changed = false;
                loop {
                    match commands.recv_timeout(Duration::from_millis(50)) {
                        Ok(Command::Capture(job_id)) => {
                            active_id = job_id;
                            original = None;
                            changed = false;
                            match automation.GetFocusedElement() {
                                Ok(element) => {
                                    let control = element.CurrentControlType();
                                    let edit = control == Ok(UIA_EditControlTypeId);
                                    let password =
                                        element.CurrentIsPassword().map(|value| value.as_bool());
                                    let focused = element
                                        .CurrentHasKeyboardFocus()
                                        .map(|value| value.as_bool());
                                    if inspect {
                                        let class =
                                            element.CurrentClassName().map(|s| s.to_string());
                                        let framework =
                                            element.CurrentFrameworkId().map(|s| s.to_string());
                                        let role = element.CurrentAriaRole().map(|s| s.to_string());
                                        let value = element
                                            .GetCurrentPatternAs::<IUIAutomationValuePattern>(
                                                UIA_ValuePatternId,
                                            );
                                        let writable = value
                                            .ok()
                                            .and_then(|pattern| pattern.CurrentIsReadOnly().ok())
                                            .map(|readonly| !readonly.as_bool());
                                        let text_edit = element
                                            .GetCurrentPatternAs::<IUIAutomationTextEditPattern>(
                                                UIA_TextEditPatternId,
                                            )
                                            .is_ok();
                                        println!(
                                            "UIA: class={class:?} control={control:?} edit={edit} password={password:?} focused={focused:?} framework={framework:?} role={role:?} writable={writable:?} text_edit={text_edit}"
                                        );
                                    }

                                    if edit && password == Ok(false) && focused == Ok(true) {
                                        original = Some(element);
                                    }
                                }
                                Err(error) => {
                                    if inspect {
                                        println!("UIA: focused element unavailable: {error}");
                                    }
                                }
                            }
                        }
                        Ok(Command::Check(job_id)) => {
                            let same = if active_id == job_id && !changed {
                                original
                                    .as_ref()
                                    .is_some_and(|element| same_element(&automation, element))
                            } else {
                                false
                            };
                            if inspect {
                                println!("UIA: same eligible edit element={same}");
                            }

                            let _ = outcomes.send(Outcome {
                                job_id,
                                eligible: same,
                            });
                            original = None;
                        }
                        Ok(Command::Cancel) => original = None,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Some(element) = original.as_ref()
                                && !same_element(&automation, element)
                            {
                                changed = true;
                                original = None;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
            Err(error) => println!("UIA: initialization failed: {error}"),
        }

        CoUninitialize();
    });
    (sender, receiver)
}
