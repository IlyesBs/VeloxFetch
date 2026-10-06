//! Operating-system integration: power actions (shutdown / sleep / hibernate), files, browsers, clipboard, and
//! [`system::SystemIntegration`], the one door the download manager goes through.
//!
//! Safety rules, enforced here and by the caller (`download-manager`):
//! * Nothing in this crate decides *when* to act. It only knows *how*. The manager runs an action
//!   only after the user explicitly armed it, and after a visible, cancellable countdown.
//! * Commands are fixed argument lists run without a shell; no user-supplied text ever reaches them.
//! * On Windows the system tools are called by absolute path under `%SystemRoot%\System32`, so a
//!   hostile `PATH` entry cannot substitute them.

pub mod browser_launch;
pub mod clipboard;
pub mod files;
pub mod native_messaging;
pub mod sandboxed_browsers;
pub mod store_extension;
pub mod system;

pub use system::{DryRunSystem, KeepAwake, NativeSystem, Notification, ProtocolHandler, SystemCall, SystemError, SystemIntegration};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    Shutdown,
    Sleep,
    Hibernate,
}

impl PowerAction {
    pub const ALL: [PowerAction; 3] = [Self::Shutdown, Self::Sleep, Self::Hibernate];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PowerError {
    #[error("{0:?} is not supported on this operating system")]
    Unsupported(PowerAction),
    #[error("could not run the system command: {0}")]
    Spawn(String),
    #[error("the system command failed with status {0}")]
    Failed(String),
}

/// A program and its fixed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

fn spec(program: &str, args: &[&str]) -> CommandSpec {
    CommandSpec { program: program.to_string(), args: args.iter().map(|a| a.to_string()).collect() }
}

/// The command that performs `action` on `platform`; `None` if the platform cannot do it.
/// `system_root` is `%SystemRoot%` (only used on Windows).
pub fn command_for(platform: Platform, action: PowerAction, system_root: &str) -> Option<CommandSpec> {
    use PowerAction::*;
    match platform {
        Platform::Windows => {
            let sys32 = format!("{}\\System32\\", system_root.trim_end_matches(['\\', '/']));
            Some(match action {
                Shutdown => spec(&format!("{sys32}shutdown.exe"), &["/s", "/t", "0"]),
                Hibernate => spec(&format!("{sys32}shutdown.exe"), &["/h"]),
                // Note: Windows hibernates instead of sleeping when hibernation is enabled and
                // "hybrid sleep" is off; that is an OS setting and not something we override.
                Sleep => spec(&format!("{sys32}rundll32.exe"), &["powrprof.dll,SetSuspendState", "0,1,0"]),
            })
        }
        Platform::Linux => Some(match action {
            Shutdown => spec("systemctl", &["poweroff"]),
            Sleep => spec("systemctl", &["suspend"]),
            Hibernate => spec("systemctl", &["hibernate"]),
        }),
        Platform::MacOs => match action {
            Shutdown => Some(spec("osascript", &["-e", "tell application \"System Events\" to shut down"])),
            Sleep => Some(spec("pmset", &["sleepnow"])),
            Hibernate => None, // macOS manages hibernation itself
        },
        Platform::Other => None,
    }
}

/// Which actions this machine's OS can perform.
pub fn supported_actions(platform: Platform) -> Vec<PowerAction> {
    PowerAction::ALL.into_iter().filter(|a| command_for(platform, *a, "C:\\Windows").is_some()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_uses_absolute_system32_paths() {
        let c = command_for(Platform::Windows, PowerAction::Shutdown, "C:\\Windows").unwrap();
        assert_eq!(c.program, "C:\\Windows\\System32\\shutdown.exe");
        assert_eq!(c.args, ["/s", "/t", "0"]);
        let h = command_for(Platform::Windows, PowerAction::Hibernate, "D:\\WIN\\").unwrap();
        assert_eq!(h.program, "D:\\WIN\\System32\\shutdown.exe", "trailing slash is normalised");
        assert_eq!(h.args, ["/h"]);
        let s = command_for(Platform::Windows, PowerAction::Sleep, "C:\\Windows").unwrap();
        assert!(s.program.ends_with("System32\\rundll32.exe"));
        assert_eq!(s.args, ["powrprof.dll,SetSuspendState", "0,1,0"]);
    }

    #[test]
    fn linux_and_macos_commands() {
        assert_eq!(command_for(Platform::Linux, PowerAction::Shutdown, "").unwrap().args, ["poweroff"]);
        assert_eq!(command_for(Platform::Linux, PowerAction::Sleep, "").unwrap().args, ["suspend"]);
        assert_eq!(command_for(Platform::Linux, PowerAction::Hibernate, "").unwrap().args, ["hibernate"]);
        assert_eq!(command_for(Platform::MacOs, PowerAction::Sleep, "").unwrap().program, "pmset");
        assert!(command_for(Platform::MacOs, PowerAction::Hibernate, "").is_none());
        assert!(command_for(Platform::Other, PowerAction::Shutdown, "").is_none());
    }

    #[test]
    fn supported_lists_match_the_platform() {
        assert_eq!(supported_actions(Platform::Windows).len(), 3);
        assert_eq!(supported_actions(Platform::MacOs), [PowerAction::Shutdown, PowerAction::Sleep]);
        assert!(supported_actions(Platform::Other).is_empty());
    }
}
