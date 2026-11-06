use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows exit code for "a required DLL was not found".
const DLL_NOT_FOUND: i32 = 0xC000_0135_u32 as i32;

pub struct HelperProcess {
    child: Child,
    job: HANDLE,
}

// The job handle is only used to close it; it is safe to move between threads.
unsafe impl Send for HelperProcess {}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        unsafe { CloseHandle(self.job) };
    }
}

impl HelperProcess {
    pub fn spawn(mut command: Command) -> Result<Self, String> {
        let job = kill_on_close_job()?;
        command.creation_flags(CREATE_NO_WINDOW);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe { CloseHandle(job) };
                return Err(error.to_string());
            }
        };
        unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
        Ok(Self { child, job })
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

/// A job object that kills its processes when Yap's handle to it closes.
fn kill_on_close_job() -> Result<HANDLE, String> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err("Could not create a job object".to_owned());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            CloseHandle(job);
            return Err("Could not configure process limits".to_owned());
        }
        Ok(job)
    }
}

pub fn missing_runtime(exit: &ExitStatus) -> Option<&'static str> {
    (exit.code() == Some(DLL_NOT_FOUND))
        .then_some("the Microsoft Visual C++ Redistributable (x64) from Microsoft")
}
