//! How the app behaves on the desktop: close to the tray, start at login, notifications, and the language of the
//! texts Rust shows itself. Stored with the other settings; starting at login goes through the system integration
//! (a per-user entry: the Windows `Run` key, an XDG autostart file, a macOS launch agent).

use std::sync::atomic::Ordering;

use super::{blocking, DownloadManager};
use downloader_core::DownloadError;

use crate::api::{DesktopPrefsDto, LoginStart, ManagerError, SetDesktopPrefs};

const CLOSE_TO_TRAY_KEY: &str = "desktop.close_to_tray";
const START_AT_LOGIN_KEY: &str = "desktop.start_at_login";
const NOTIFY_KEY: &str = "desktop.notify_finished";
const LANGUAGE_KEY: &str = "desktop.language";
const LOGIN_START_KEY: &str = "desktop.login_start";

/// `en`, `fr`, `pt-br`...: what a language file may be called. Anything else is refused.
pub fn valid_language(code: &str) -> bool {
    let mut parts = code.split('-');
    let first = parts.next().unwrap_or_default();
    (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        && code.len() <= 16
}

impl DownloadManager {
    fn flag(&self, key: &str, default: bool) -> bool {
        match self.inner.store.get_setting(key).ok().flatten().as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        }
    }

    pub fn desktop_prefs(&self) -> DesktopPrefsDto {
        let launcher = self.inner.cfg.app_launcher.as_deref();
        DesktopPrefsDto {
            close_to_tray: self.flag(CLOSE_TO_TRAY_KEY, true),
            start_at_login: launcher.is_some_and(|exe| self.inner.cfg.system.0.starts_at_login(exe)),
            notify_finished: self.flag(NOTIFY_KEY, true),
            language: self
                .inner
                .store
                .get_setting(LANGUAGE_KEY)
                .ok()
                .flatten()
                .filter(|l| valid_language(l))
                .unwrap_or_else(|| "en".into()),
            tray_available: self.inner.tray_available.load(Ordering::Relaxed),
            start_at_login_available: launcher.is_some(),
            login_start: self.login_start(),
        }
    }

    /// How to show the app when it was started at login (tray by default).
    pub fn login_start(&self) -> LoginStart {
        self.inner.store.get_setting(LOGIN_START_KEY).ok().flatten().map_or_else(LoginStart::default, |t| LoginStart::from_setting(&t))
    }

    pub async fn set_desktop_prefs(&self, req: SetDesktopPrefs) -> Result<DesktopPrefsDto, ManagerError> {
        if let Some(lang) = &req.language {
            if !valid_language(lang) {
                return Err(ManagerError::InvalidInput("not a language code".into()));
            }
        }
        if let Some(on) = req.start_at_login {
            let exe = self
                .inner
                .cfg
                .app_launcher
                .clone()
                .ok_or_else(|| ManagerError::InvalidInput("starting at login is not available".into()))?;
            let system = self.inner.cfg.system.clone();
            blocking(move || system.0.set_start_at_login(on.then_some(exe.as_path())).map_err(|e| DownloadError::Internal(e.to_string())))
                .await
                .map_err(|e| ManagerError::Internal(format!("could not change starting at login: {e}")))?;
        }
        let bool_text = |b: bool| if b { "true" } else { "false" }.to_string();
        let writes: Vec<(&'static str, String)> = [
            req.close_to_tray.map(|b| (CLOSE_TO_TRAY_KEY, bool_text(b))),
            req.start_at_login.map(|b| (START_AT_LOGIN_KEY, bool_text(b))),
            req.notify_finished.map(|b| (NOTIFY_KEY, bool_text(b))),
            req.language.map(|l| (LANGUAGE_KEY, l)),
            req.login_start.map(|w| (LOGIN_START_KEY, w.as_str().to_string())),
        ]
        .into_iter()
        .flatten()
        .collect();
        let store = self.inner.store.clone();
        blocking(move || writes.iter().try_for_each(|(k, v)| store.set_setting(k, v))).await?;
        Ok(self.desktop_prefs())
    }

    /// The operating system, for the app's own desktop features (notifications).
    pub fn system(&self) -> crate::api::SystemHandle {
        self.inner.cfg.system.clone()
    }

    /// Told by the app once it knows whether this desktop shows tray icons.
    pub fn set_tray_available(&self, available: bool) {
        self.inner.tray_available.store(available, Ordering::Relaxed);
    }

    /// At start: when the user asked to start at login, keep the entry pointing at this copy of the app (it was
    /// moved, or an AppImage was replaced by a newer file).
    pub fn refresh_start_at_login(&self) {
        let Some(exe) = self.inner.cfg.app_launcher.as_deref() else { return };
        let system = &self.inner.cfg.system.0;
        if self.flag(START_AT_LOGIN_KEY, false) && !system.starts_at_login(exe) {
            if let Err(e) = system.set_start_at_login(Some(exe)) {
                tracing::warn!(error = %e, "could not keep starting at login up to date");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid_language;
    use crate::api::LoginStart;

    #[test]
    fn login_start_reads_back_what_it_wrote_and_falls_back_to_the_tray() {
        for w in [LoginStart::Tray, LoginStart::Minimized, LoginStart::Window] {
            assert_eq!(LoginStart::from_setting(w.as_str()), w);
        }
        for bad in ["", "Minimized", "hidden", "window\n"] {
            assert_eq!(LoginStart::from_setting(bad), LoginStart::Tray, "{bad:?}");
        }
        assert_eq!(LoginStart::default(), LoginStart::Tray, "the behaviour before the choice existed");
    }

    #[test]
    fn only_language_codes_are_accepted() {
        for ok in ["en", "fr", "ar", "pt-br", "zh-hans", "fil"] {
            assert!(valid_language(ok), "{ok}");
        }
        for bad in ["", "e", "EN", "english", "en_US", "../x", "en-", "-en", "fr\n", "a-b", &format!("en-{}", "x".repeat(20))] {
            assert!(!valid_language(bad), "{bad:?}");
        }
    }
}
