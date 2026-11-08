//! "Open at login": a LaunchAgent in the user's Library that starts Yap in
//! the menu bar (`--background`) when they log in.

use std::path::PathBuf;

use crate::platform::BACKGROUND_ARG;

const LABEL: &str = "com.yap.desktop";

fn agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("Could not find the home folder")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn agent(executable: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{BACKGROUND_ARG}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        escape(executable)
    )
}

pub fn set(enabled: bool) -> Result<(), String> {
    let path = agent_path()?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Could not update the macOS login items".to_owned()),
        };
    }

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = path.parent().ok_or("Invalid LaunchAgents folder")?;
    std::fs::create_dir_all(parent)
        .and_then(|()| std::fs::write(&path, agent(&executable.to_string_lossy())))
        .map_err(|_| "Could not update the macOS login items".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_starts_yap_in_the_background() {
        let plist = agent("/Applications/Yap & Co.app/Contents/MacOS/yap-app");
        assert!(
            plist
                .contains("<string>/Applications/Yap &amp; Co.app/Contents/MacOS/yap-app</string>")
        );
        assert!(plist.contains("<string>--background</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }
}
