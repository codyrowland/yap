use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

const WATCHDOG: &str = "read line; kill -s KILL -- -\"$1\"";

pub struct HelperProcess {
    child: Child,
    watchdog: Child,
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        let _ = self.watchdog.kill();
        let _ = self.watchdog.wait();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl HelperProcess {
    pub fn spawn(mut command: Command) -> Result<Self, String> {
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let watchdog = Command::new("/bin/sh")
            .args(["-c", WATCHDOG, "sh", &child.id().to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match watchdog {
            Ok(watchdog) => Ok(Self { child, watchdog }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error.to_string())
            }
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

pub fn missing_runtime(_exit: &ExitStatus) -> Option<&'static str> {
    None
}
