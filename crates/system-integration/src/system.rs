//! One door to the operating system for everything the app asks of it: power actions, showing and opening
//! files, keeping the computer awake while downloading, notifications, and the `veloxfetch://` link
//! protocol. The download manager only ever holds a [`SystemIntegration`]: no OS code leaks into it, and
//! tests swap in [`DryRunSystem`], which records what would have happened and touches nothing.
//!
//! Rules (same as the rest of this crate):
//! * fixed programs and argument lists, never a shell; text that comes from a download (a file name in a
//!   notification) is always a separate argument, or escaped for the one format that embeds it (Windows
//!   toast XML), so it can never become a command or markup;
//! * everything is per user: no administrator rights, no system-wide change;
//! * keeping the computer awake is a handle that releases itself when dropped, and on Linux / macOS the helper
//!   process also ends with the app if the app crashes: the computer is never left unable to sleep.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::{command_for, files, supported_actions, CommandSpec, Platform, PowerAction, PowerError};

/// The application's name as the system shows it (notification sender, inhibitor owner).
pub const APP_NAME: &str = "VeloxFetch";
/// The link protocol the app answers (`veloxfetch://...`).
pub const PROTOCOL_SCHEME: &str = "veloxfetch";

#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("not available on this system: {0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A notification. Built with [`Notification::new`], which bounds and cleans the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

impl Notification {
    pub const MAX_TITLE: usize = 120;
    pub const MAX_BODY: usize = 400;

    /// Control characters become spaces (a file name cannot add lines or escape sequences), and both parts are
    /// cut to a sane length.
    pub fn new(title: &str, body: &str) -> Self {
        fn clean(s: &str, max: usize) -> String {
            let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            let s = s.trim();
            if s.chars().count() <= max {
                s.to_string()
            } else {
                let mut cut: String = s.chars().take(max.saturating_sub(1)).collect();
                cut.push('…');
                cut
            }
        }
        Self { title: clean(title, Self::MAX_TITLE), body: clean(body, Self::MAX_BODY) }
    }
}

/// The app as the handler of a link protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolHandler {
    pub scheme: String,
    /// The program started with the link as its only argument. Must be absolute.
    pub exe: PathBuf,
}

/// A scheme the app may claim: a letter, then letters, digits, `+ - .`; never one that belongs to the web or
/// the system.
pub fn valid_scheme(s: &str) -> bool {
    const RESERVED: &[&str] = &["http", "https", "file", "ftp", "mailto", "data", "javascript", "about", "blob", "ms-settings"];
    let mut chars = s.chars();
    s.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
        && !RESERVED.contains(&s)
}

/// While held, the computer does not go to sleep on its own. Dropping it lets it sleep again.
#[must_use = "the computer may sleep again as soon as this is dropped"]
pub struct KeepAwake {
    release: Option<Box<dyn FnOnce() + Send>>,
}

impl KeepAwake {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self { release: Some(Box::new(release)) }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(f) = self.release.take() {
            f();
        }
    }
}

impl std::fmt::Debug for KeepAwake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeepAwake")
    }
}

/// Everything the app asks of the operating system. See the module documentation.
pub trait SystemIntegration: Send + Sync {
    /// Power actions this system can perform.
    fn power_actions(&self) -> Vec<PowerAction>;
    /// Shut down, sleep or hibernate now.
    fn power(&self, action: PowerAction) -> Result<(), PowerError>;
    /// Show `path` selected in the file manager (a folder is opened).
    fn reveal(&self, path: &Path) -> io::Result<()>;
    /// Open a file with its usual application (programs, scripts and web pages are refused).
    fn open(&self, path: &Path) -> io::Result<()>;
    /// Keep the computer from sleeping on its own until the handle is dropped.
    fn keep_awake(&self, why: &str) -> Result<KeepAwake, SystemError>;
    /// Show a system notification.
    fn notify(&self, n: &Notification) -> Result<(), SystemError>;
    /// Make the app the handler of `veloxfetch://` links (per user).
    fn register_protocol(&self, h: &ProtocolHandler) -> Result<(), SystemError>;
    fn unregister_protocol(&self, scheme: &str) -> Result<(), SystemError>;
    /// Registered exactly as [`register_protocol`](Self::register_protocol) would do it.
    fn protocol_registered(&self, h: &ProtocolHandler) -> bool;
}

// ------------------------------------------------------------------------------------- commands (pure)

/// The program that shows a notification on Linux / macOS (`None`: Windows uses its own API).
pub fn notify_command(platform: Platform, n: &Notification) -> Option<CommandSpec> {
    let own = |program: &str, args: Vec<String>| Some(CommandSpec { program: program.into(), args });
    match platform {
        // `--` ends the options: a title starting with `-` stays text.
        Platform::Linux => own("notify-send", vec!["--app-name".into(), APP_NAME.into(), "--".into(), n.title.clone(), n.body.clone()]),
        // The text travels as script arguments (`argv`), never inside the script.
        Platform::MacOs => own(
            "/usr/bin/osascript",
            vec![
                "-e".into(),
                "on run argv".into(),
                "-e".into(),
                "display notification (item 2 of argv) with title (item 1 of argv)".into(),
                "-e".into(),
                "end run".into(),
                n.title.clone(),
                n.body.clone(),
            ],
        ),
        Platform::Windows | Platform::Other => None,
    }
}

/// The helper that keeps the computer awake on Linux / macOS (`None`: Windows uses its own API). It holds the
/// computer awake while it runs; it ends when the app releases it, and with the app if the app dies (Linux:
/// its input closes; macOS: it watches the app's process id).
pub fn keep_awake_command(platform: Platform, why: &str, pid: u32) -> Option<CommandSpec> {
    let own = |program: &str, args: Vec<String>| Some(CommandSpec { program: program.into(), args });
    match platform {
        // "idle": the automatic sleep after inactivity. A sleep the user asks for is never blocked.
        Platform::Linux => own(
            "systemd-inhibit",
            vec!["--what=idle".into(), format!("--who={APP_NAME}"), format!("--why={why}"), "--mode=block".into(), "cat".into()],
        ),
        Platform::MacOs => own("/usr/bin/caffeinate", vec!["-i".into(), "-w".into(), pid.to_string()]),
        Platform::Windows | Platform::Other => None,
    }
}

/// Text for XML (Windows toast).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The Windows toast document for `n`.
pub fn toast_xml(n: &Notification) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(&n.title),
        xml_escape(&n.body)
    )
}

/// Windows: the command line Explorer runs for a link (`"C:\...\VeloxFetch.exe" "%1"`).
pub fn windows_protocol_command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// Linux: where the handler's desktop entry lives.
pub fn desktop_file(data_home: &Path, scheme: &str) -> PathBuf {
    data_home.join("applications").join(format!("{scheme}-url-handler.desktop"))
}

/// Linux: the desktop entry that hands `scheme:` links to `exe` (quoted as the desktop entry specification
/// wants: inside double quotes, `"`, `` ` ``, `$` and `\` escaped, `%` doubled).
pub fn desktop_entry(h: &ProtocolHandler) -> String {
    let mut exe = String::new();
    for c in h.exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                exe.push('\\');
                exe.push(c);
            }
            '%' => exe.push_str("%%"),
            c => exe.push(c),
        }
    }
    format!(
        "[Desktop Entry]\nType=Application\nName={APP_NAME}\nExec=\"{exe}\" %u\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/{};\n",
        h.scheme
    )
}

fn check_handler(h: &ProtocolHandler) -> Result<(), SystemError> {
    if !valid_scheme(&h.scheme) {
        return Err(SystemError::Failed("not a link protocol the app may register".into()));
    }
    if !h.exe.is_absolute() || h.exe.to_string_lossy().contains(['"', '\n', '\r']) {
        return Err(SystemError::Failed("the program path is not usable for a link protocol".into()));
    }
    Ok(())
}

fn run(c: &CommandSpec, stdin: Stdio) -> io::Result<std::process::Child> {
    Command::new(&c.program).args(&c.args).stdin(stdin).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

fn run_to_end(c: &CommandSpec) -> Result<(), SystemError> {
    let status = run(c, Stdio::null())?.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(SystemError::Failed(format!("{} failed ({status})", c.program)))
    }
}

/// Linux: have the desktop see a handler added or removed right away (the desktop portal, which starts the app for
/// a sandboxed browser, otherwise may not know it yet). Best effort: the tool is not on every system.
fn refresh_desktop_database(desktop_file: &Path) {
    if let Some(dir) = desktop_file.parent() {
        let c = CommandSpec { program: "update-desktop-database".into(), args: vec![dir.to_string_lossy().into_owned()] };
        let _ = run_to_end(&c);
    }
}

// ------------------------------------------------------------------------------------- the real system

/// The operating system this program runs on.
#[derive(Debug, Clone)]
pub struct NativeSystem {
    platform: Platform,
    /// Linux: `$XDG_DATA_HOME` (or `~/.local/share`), where the protocol's desktop entry goes.
    data_home: Option<PathBuf>,
    /// Windows: the app's id, as its Start menu shortcut carries it (toasts are shown under it).
    app_id: String,
}

impl Default for NativeSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeSystem {
    pub fn new() -> Self {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        Self { platform: Platform::current(), data_home, app_id: "app.velox.fetch".into() }
    }
}

impl SystemIntegration for NativeSystem {
    fn power_actions(&self) -> Vec<PowerAction> {
        supported_actions(self.platform)
    }

    fn power(&self, action: PowerAction) -> Result<(), PowerError> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let c = command_for(self.platform, action, &root).ok_or(PowerError::Unsupported(action))?;
        let status = Command::new(&c.program)
            .args(&c.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| PowerError::Spawn(e.kind().to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(PowerError::Failed(status.to_string()))
        }
    }

    fn reveal(&self, path: &Path) -> io::Result<()> {
        files::reveal(self.platform, path)
    }

    fn open(&self, path: &Path) -> io::Result<()> {
        files::open(self.platform, path)
    }

    fn keep_awake(&self, why: &str) -> Result<KeepAwake, SystemError> {
        if self.platform == Platform::Windows {
            return windows::keep_awake();
        }
        let c = keep_awake_command(self.platform, why, std::process::id()).ok_or(SystemError::Unsupported("keeping the computer awake"))?;
        let mut child = run(&c, Stdio::piped()).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => SystemError::Unsupported("keeping the computer awake (no systemd-inhibit / caffeinate)"),
            _ => SystemError::Io(e),
        })?;
        // Gone at once: the inhibitor was refused (no session bus, no logind...).
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Ok(Some(status)) = child.try_wait() {
            return Err(SystemError::Failed(format!("{} ended at once ({status})", c.program)));
        }
        let stdin = child.stdin.take();
        Ok(KeepAwake::new(move || {
            drop(stdin); // Linux: `cat` sees the end of its input and the inhibitor ends
            let _ = child.kill();
            let _ = child.wait();
        }))
    }

    fn notify(&self, n: &Notification) -> Result<(), SystemError> {
        if self.platform == Platform::Windows {
            return windows::toast(&self.app_id, n);
        }
        let c = notify_command(self.platform, n).ok_or(SystemError::Unsupported("notifications"))?;
        run_to_end(&c).map_err(|e| match e {
            SystemError::Io(e) if e.kind() == io::ErrorKind::NotFound => SystemError::Unsupported("notifications (no notify-send)"),
            other => other,
        })
    }

    fn register_protocol(&self, h: &ProtocolHandler) -> Result<(), SystemError> {
        check_handler(h)?;
        match self.platform {
            Platform::Windows => windows::register_protocol(h),
            Platform::Linux => {
                let home = self.data_home.as_deref().ok_or(SystemError::Unsupported("link protocols (no home folder)"))?;
                let file = desktop_file(home, &h.scheme);
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&file, desktop_entry(h))?;
                refresh_desktop_database(&file);
                let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let mime = format!("x-scheme-handler/{}", h.scheme);
                run_to_end(&CommandSpec { program: "xdg-mime".into(), args: vec!["default".into(), name, mime] })
            }
            // Declared by the app bundle itself (Info.plist), read by Launch Services when the app is installed.
            Platform::MacOs => Err(SystemError::Unsupported("link protocols are declared by the app bundle on macOS")),
            Platform::Other => Err(SystemError::Unsupported("link protocols")),
        }
    }

    fn unregister_protocol(&self, scheme: &str) -> Result<(), SystemError> {
        if !valid_scheme(scheme) {
            return Err(SystemError::Failed("not a link protocol the app may register".into()));
        }
        match self.platform {
            Platform::Windows => windows::unregister_protocol(scheme),
            Platform::Linux => {
                let Some(home) = self.data_home.as_deref() else { return Ok(()) };
                let file = desktop_file(home, scheme);
                match std::fs::remove_file(&file) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
                    _ => {
                        refresh_desktop_database(&file);
                        Ok(())
                    }
                }
            }
            _ => Ok(()),
        }
    }

    fn protocol_registered(&self, h: &ProtocolHandler) -> bool {
        match self.platform {
            Platform::Windows => windows::protocol_registered(h),
            Platform::Linux => self
                .data_home
                .as_deref()
                .and_then(|home| std::fs::read_to_string(desktop_file(home, &h.scheme)).ok())
                .is_some_and(|text| text == desktop_entry(h)),
            _ => false,
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::{toast_xml, windows_protocol_command, KeepAwake, Notification, ProtocolHandler, SystemError};

    /// Windows keeps a computer awake for as long as a thread says so: a small thread holds the request until
    /// the handle is dropped (or the app ends, which ends the thread).
    pub fn keep_awake() -> Result<KeepAwake, SystemError> {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let (ready, is_ready) = std::sync::mpsc::channel::<bool>();
        std::thread::Builder::new().name("keep-awake".into()).spawn(move || {
            // SAFETY: plain Win32 calls with constant flags, on this thread only.
            let ok = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } != 0;
            let _ = ready.send(ok);
            let _ = stopped.recv(); // until the handle is dropped
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        })?;
        if !is_ready.recv().unwrap_or(false) {
            return Err(SystemError::Failed("Windows refused to keep the computer awake".into()));
        }
        Ok(KeepAwake::new(move || drop(stop)))
    }

    pub fn toast(app_id: &str, n: &Notification) -> Result<(), SystemError> {
        use windows::core::HSTRING;
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
        let fail = |e: windows::core::Error| SystemError::Failed(format!("notification: {}", e.message()));
        let doc = XmlDocument::new().map_err(fail)?;
        doc.LoadXml(&HSTRING::from(toast_xml(n))).map_err(fail)?;
        let toast = ToastNotification::CreateToastNotification(&doc).map_err(fail)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id)).map_err(fail)?.Show(&toast).map_err(fail)
    }

    fn classes(scheme: &str) -> String {
        format!(r"Software\Classes\{scheme}")
    }

    pub fn register_protocol(h: &ProtocolHandler) -> Result<(), SystemError> {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (root, _) = hkcu.create_subkey(classes(&h.scheme))?;
        root.set_value("", &format!("URL:{} link", super::APP_NAME))?;
        root.set_value("URL Protocol", &"")?;
        let (icon, _) = root.create_subkey("DefaultIcon")?;
        icon.set_value("", &format!("\"{}\",0", h.exe.display()))?;
        let (cmd, _) = root.create_subkey(r"shell\open\command")?;
        cmd.set_value("", &windows_protocol_command(&h.exe))?;
        Ok(())
    }

    pub fn unregister_protocol(scheme: &str) -> Result<(), SystemError> {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(classes(scheme)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn protocol_registered(h: &ProtocolHandler) -> bool {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        use winreg::RegKey;
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(format!(r"{}\shell\open\command", classes(&h.scheme)), KEY_READ)
            .and_then(|k| k.get_value::<String, _>(""))
            .is_ok_and(|v| v == windows_protocol_command(&h.exe))
    }
}

#[cfg(not(windows))]
mod windows {
    use super::{KeepAwake, Notification, ProtocolHandler, SystemError};

    pub fn keep_awake() -> Result<KeepAwake, SystemError> {
        Err(SystemError::Unsupported("not Windows"))
    }
    pub fn toast(_: &str, _: &Notification) -> Result<(), SystemError> {
        Err(SystemError::Unsupported("not Windows"))
    }
    pub fn register_protocol(_: &ProtocolHandler) -> Result<(), SystemError> {
        Err(SystemError::Unsupported("not Windows"))
    }
    pub fn unregister_protocol(_: &str) -> Result<(), SystemError> {
        Ok(())
    }
    pub fn protocol_registered(_: &ProtocolHandler) -> bool {
        false
    }
}

// ------------------------------------------------------------------------------------- the pretend system

/// What a [`DryRunSystem`] was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemCall {
    Power(PowerAction),
    Reveal(PathBuf),
    Open(PathBuf),
    Notify(Notification),
    KeepAwake(String),
    /// A [`KeepAwake`] handle was dropped.
    ReleaseAwake,
    RegisterProtocol(String),
    UnregisterProtocol(String),
}

/// Records what would have happened and touches nothing on the machine. Used by tests and by debug builds
/// (end-to-end tests), so the whole flow (arm -> countdown -> "shut down", keep awake, notify...) can run
/// safely. Applies the same checks as the real one.
pub struct DryRunSystem {
    calls: std::sync::Arc<Mutex<Vec<SystemCall>>>,
    power_supported: Vec<PowerAction>,
    power_fails: bool,
    protocols: Mutex<HashSet<(String, PathBuf)>>,
}

impl Default for DryRunSystem {
    fn default() -> Self {
        Self { calls: Default::default(), power_supported: PowerAction::ALL.to_vec(), power_fails: false, protocols: Default::default() }
    }
}

impl DryRunSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Only these power actions are "supported", and running one fails when `fails`.
    pub fn with_power(mut self, supported: Vec<PowerAction>, fails: bool) -> Self {
        self.power_supported = supported;
        self.power_fails = fails;
        self
    }

    pub fn calls(&self) -> Vec<SystemCall> {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn power_calls(&self) -> Vec<PowerAction> {
        self.calls().into_iter().filter_map(|c| if let SystemCall::Power(a) = c { Some(a) } else { None }).collect()
    }

    /// The files shown (`"reveal"`) and opened (`"open"`), in order.
    pub fn file_calls(&self) -> Vec<(&'static str, PathBuf)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                SystemCall::Reveal(p) => Some(("reveal", p)),
                SystemCall::Open(p) => Some(("open", p)),
                _ => None,
            })
            .collect()
    }

    /// Keep-awake handles taken and not dropped yet.
    pub fn awake_holds(&self) -> usize {
        self.calls().iter().fold(0i64, |n, c| match c {
            SystemCall::KeepAwake(_) => n + 1,
            SystemCall::ReleaseAwake => n - 1,
            _ => n,
        }) as usize
    }

    fn record(&self, c: SystemCall) {
        eprintln!("[dry-run] {c:?}");
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).push(c);
    }
}

impl SystemIntegration for DryRunSystem {
    fn power_actions(&self) -> Vec<PowerAction> {
        self.power_supported.clone()
    }

    fn power(&self, action: PowerAction) -> Result<(), PowerError> {
        self.record(SystemCall::Power(action));
        if !self.power_supported.contains(&action) {
            return Err(PowerError::Unsupported(action));
        }
        if self.power_fails {
            return Err(PowerError::Failed("simulated".into()));
        }
        Ok(())
    }

    fn reveal(&self, path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not an absolute path"));
        }
        self.record(SystemCall::Reveal(path.to_path_buf()));
        Ok(())
    }

    fn open(&self, path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not an absolute path"));
        }
        if files::is_risky_to_open(path) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "programs, scripts and web pages are not opened from VeloxFetch"));
        }
        self.record(SystemCall::Open(path.to_path_buf()));
        Ok(())
    }

    fn keep_awake(&self, why: &str) -> Result<KeepAwake, SystemError> {
        self.record(SystemCall::KeepAwake(why.to_string()));
        let calls = self.calls.clone();
        Ok(KeepAwake::new(move || calls.lock().unwrap_or_else(|p| p.into_inner()).push(SystemCall::ReleaseAwake)))
    }

    fn notify(&self, n: &Notification) -> Result<(), SystemError> {
        self.record(SystemCall::Notify(n.clone()));
        Ok(())
    }

    fn register_protocol(&self, h: &ProtocolHandler) -> Result<(), SystemError> {
        check_handler(h)?;
        self.record(SystemCall::RegisterProtocol(h.scheme.clone()));
        self.protocols.lock().unwrap_or_else(|p| p.into_inner()).insert((h.scheme.clone(), h.exe.clone()));
        Ok(())
    }

    fn unregister_protocol(&self, scheme: &str) -> Result<(), SystemError> {
        self.record(SystemCall::UnregisterProtocol(scheme.to_string()));
        self.protocols.lock().unwrap_or_else(|p| p.into_inner()).retain(|(s, _)| s != scheme);
        Ok(())
    }

    fn protocol_registered(&self, h: &ProtocolHandler) -> bool {
        self.protocols.lock().unwrap_or_else(|p| p.into_inner()).contains(&(h.scheme.clone(), h.exe.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_cleaned_and_bounded() {
        let n = Notification::new("  Done\n", "file\u{1b}[31m.zip\r\nrm -rf /");
        assert_eq!(n.title, "Done");
        assert!(!n.body.chars().any(char::is_control), "{:?}", n.body);
        let long = Notification::new(&"t".repeat(500), &"b".repeat(5000));
        assert_eq!(long.title.chars().count(), Notification::MAX_TITLE);
        assert_eq!(long.body.chars().count(), Notification::MAX_BODY);
        assert!(long.body.ends_with('…'));
    }

    #[test]
    fn notification_text_is_never_part_of_a_command_or_markup() {
        let n = Notification::new("-rf \"; do shell script \"x\"", "<b>&</b>");
        let linux = notify_command(Platform::Linux, &n).unwrap();
        assert_eq!(linux.program, "notify-send");
        let dashdash = linux.args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&linux.args[dashdash + 1..], [n.title.clone(), n.body.clone()], "text only after `--`");
        let mac = notify_command(Platform::MacOs, &n).unwrap();
        assert!(mac.args.iter().filter(|a| a.contains("display notification")).all(|a| !a.contains("shell script")));
        assert_eq!(&mac.args[mac.args.len() - 2..], [n.title.clone(), n.body.clone()], "passed as argv");
        assert!(notify_command(Platform::Windows, &n).is_none(), "Windows uses its own API");
        let xml = toast_xml(&n);
        assert!(xml.contains("&lt;b&gt;&amp;&lt;/b&gt;") && xml.contains("&quot;"), "{xml}");
        assert_eq!(xml.matches("<text>").count(), 2);
    }

    #[test]
    fn keep_awake_helpers_never_block_a_sleep_the_user_asks_for() {
        let linux = keep_awake_command(Platform::Linux, "Downloading", 42).unwrap();
        assert_eq!(linux.program, "systemd-inhibit");
        assert!(linux.args.contains(&"--what=idle".to_string()));
        assert!(!linux.args.iter().any(|a| a.contains("sleep") || a.contains("shutdown")));
        assert_eq!(linux.args.last().unwrap(), "cat", "ends when the app's pipe closes");
        let mac = keep_awake_command(Platform::MacOs, "Downloading", 42).unwrap();
        assert_eq!(mac.args, ["-i", "-w", "42"], "idle sleep only, ends with the app");
        assert!(keep_awake_command(Platform::Windows, "x", 1).is_none());
    }

    #[test]
    fn link_protocols_are_checked_and_written_safely() {
        assert!(valid_scheme("veloxfetch") && valid_scheme("velox-fetch+1.x"));
        for bad in ["", "http", "https", "file", "javascript", "Velox", "1abc", "a b", "a:b", &"a".repeat(33)] {
            assert!(!valid_scheme(bad), "{bad:?}");
        }
        let h = ProtocolHandler { scheme: "veloxfetch".into(), exe: PathBuf::from("/opt/Velox $Fetch/100%/velox-desktop") };
        let entry = desktop_entry(&h);
        assert!(entry.contains("Exec=\"/opt/Velox \\$Fetch/100%%/velox-desktop\" %u\n"), "{entry}");
        assert!(entry.contains("MimeType=x-scheme-handler/veloxfetch;\n"));
        assert_eq!(
            desktop_file(Path::new("/h/.local/share"), "veloxfetch"),
            Path::new("/h/.local/share/applications/veloxfetch-url-handler.desktop")
        );
        assert_eq!(windows_protocol_command(Path::new(r"C:\Apps\VeloxFetch.exe")), r#""C:\Apps\VeloxFetch.exe" "%1""#);

        let dry = DryRunSystem::new();
        assert!(dry.register_protocol(&ProtocolHandler { scheme: "https".into(), exe: h.exe.clone() }).is_err());
        assert!(dry.register_protocol(&ProtocolHandler { scheme: "veloxfetch".into(), exe: "relative".into() }).is_err());
        assert!(!dry.protocol_registered(&h));
        dry.register_protocol(&h).unwrap();
        assert!(dry.protocol_registered(&h));
        dry.unregister_protocol("veloxfetch").unwrap();
        assert!(!dry.protocol_registered(&h));
    }

    #[test]
    fn the_linux_protocol_is_registered_in_the_users_own_folder() {
        let dir = std::env::temp_dir().join(format!("velox-proto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sys = NativeSystem { platform: Platform::Linux, data_home: Some(dir.clone()), app_id: String::new() };
        let h = ProtocolHandler { scheme: PROTOCOL_SCHEME.into(), exe: PathBuf::from("/opt/VeloxFetch/velox-desktop") };
        assert!(!sys.protocol_registered(&h));
        // Without xdg-mime the default cannot be set, but the entry is written either way.
        let _ = sys.register_protocol(&h);
        assert!(sys.protocol_registered(&h));
        assert!(
            !sys.protocol_registered(&ProtocolHandler { exe: "/elsewhere/velox".into(), ..h.clone() }),
            "a moved app counts as not registered"
        );
        sys.unregister_protocol(PROTOCOL_SCHEME).unwrap();
        sys.unregister_protocol(PROTOCOL_SCHEME).unwrap();
        assert!(!sys.protocol_registered(&h));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_dry_run_keeps_count_of_keep_awake_handles() {
        let dry = DryRunSystem::new();
        let a = dry.keep_awake("one").unwrap();
        let b = dry.keep_awake("two").unwrap();
        assert_eq!(dry.awake_holds(), 2);
        drop(a);
        assert_eq!(dry.awake_holds(), 1);
        drop(b);
        assert_eq!(dry.awake_holds(), 0);
        let only_sleep = DryRunSystem::new().with_power(vec![PowerAction::Sleep], true);
        assert!(matches!(only_sleep.power(PowerAction::Shutdown), Err(PowerError::Unsupported(_))));
        assert!(matches!(only_sleep.power(PowerAction::Sleep), Err(PowerError::Failed(_))));
        assert_eq!(only_sleep.power_calls(), [PowerAction::Shutdown, PowerAction::Sleep]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "holds a real inhibitor on this machine; run by hand: cargo test -- --ignored keep_awake_really"]
    fn keep_awake_really_holds_and_releases_an_inhibitor() {
        let guard = NativeSystem::new().keep_awake("VeloxFetch test").unwrap();
        let list = Command::new("systemd-inhibit").arg("--list").output().unwrap();
        assert!(String::from_utf8_lossy(&list.stdout).contains(APP_NAME));
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let list = Command::new("systemd-inhibit").arg("--list").output().unwrap();
        assert!(!String::from_utf8_lossy(&list.stdout).contains("VeloxFetch test"));
    }
}
