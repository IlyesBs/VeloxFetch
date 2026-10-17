//! Who is talking to the native host?
//!
//! Browsers tell a native messaging host who launched it through its command line:
//! * Chrome / Edge / Chromium: the first argument is the caller's origin, `chrome-extension://<id>/`
//!   (on Windows a `--parent-window=<hwnd>` argument may follow).
//! * Firefox: the first argument is the host manifest path and the second the extension id.
//!
//! The browser already enforces `allowed_origins` / `allowed_extensions` from the host manifest;
//! we check again here, against a list compiled into the binary, so a wrongly written manifest (or
//! the host being run by hand) cannot open the door.

/// Chromium extension id, fixed by the public key in the extension's manifest.
pub const CHROME_EXTENSION_ID: &str = "gbcanjaaelioalfdggkhbbnbinmfgdbc";
/// The extension's id in the Chrome Web Store (the store gives an item its own id). Chrome, Edge and Brave get the
/// extension from the store: the installers point the browsers at it (their own "New extension added" prompt) and
/// Install extension opens its store page. While it is `None`, browsers get it through Developer mode instead.
/// The installers carry the same id (`packaging/postinst.sh`, `installer-hooks.nsh`; a test keeps them in step).
pub const CHROME_STORE_ID: Option<&str> = Some("keobnkoppcchnbpmmfodidkcedglbhnn");

/// Every Chromium extension id that may use the bridge: the one loaded unpacked, and the store one.
pub fn chromium_ids() -> Vec<String> {
    std::iter::once(CHROME_EXTENSION_ID).chain(CHROME_STORE_ID).map(str::to_string).collect()
}

/// Firefox add-on id (`browser_specific_settings.gecko.id`).
pub const FIREFOX_EXTENSION_ID: &str = "velox-fetch@velox.app";
/// Name the host is registered under with the browsers.
pub const HOST_NAME: &str = "app.velox.fetch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Chromium { extension_id: String },
    Firefox { extension_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallerError {
    #[error("this program is started by a web browser, not by hand")]
    NoArguments,
    #[error("unrecognised caller")]
    Unrecognised,
    #[error("caller is not an allowed extension")]
    NotAllowed,
}

/// Which extensions may use the bridge.
#[derive(Debug, Clone)]
pub struct Allowlist {
    pub chromium: Vec<String>,
    pub firefox: Vec<String>,
}

impl Default for Allowlist {
    fn default() -> Self {
        Self { chromium: chromium_ids(), firefox: vec![FIREFOX_EXTENSION_ID.to_string()] }
    }
}

/// A Chromium extension id is exactly 32 characters from `a` to `p`.
fn is_chromium_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn is_firefox_id(s: &str) -> bool {
    // `name@domain`, or a `{uuid}`; conservative character set, bounded length.
    !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'-' | b'_' | b'{' | b'}'))
}

impl Caller {
    /// Interpret the host's command-line arguments (without the program name).
    pub fn from_args(args: &[String]) -> Result<Self, CallerError> {
        let first = args.first().ok_or(CallerError::NoArguments)?;
        if let Some(rest) = first.strip_prefix("chrome-extension://") {
            let id = rest.strip_suffix('/').unwrap_or(rest);
            return if is_chromium_id(id) { Ok(Self::Chromium { extension_id: id.to_string() }) } else { Err(CallerError::Unrecognised) };
        }
        // Firefox: <manifest path> <extension id>
        if first.ends_with(".json") {
            if let Some(id) = args.get(1).filter(|a| !a.starts_with("--")) {
                if is_firefox_id(id) {
                    return Ok(Self::Firefox { extension_id: id.clone() });
                }
            }
        }
        Err(CallerError::Unrecognised)
    }

    /// Stable text form, used on the host <-> app link and in logs.
    pub fn origin(&self) -> String {
        match self {
            Self::Chromium { extension_id } => format!("chrome-extension://{extension_id}/"),
            Self::Firefox { extension_id } => format!("firefox-extension:{extension_id}"),
        }
    }

    pub fn from_origin(s: &str) -> Result<Self, CallerError> {
        if s.starts_with("chrome-extension://") {
            return Self::from_args(&[s.to_string()]);
        }
        match s.strip_prefix("firefox-extension:") {
            Some(id) if is_firefox_id(id) => Ok(Self::Firefox { extension_id: id.to_string() }),
            _ => Err(CallerError::Unrecognised),
        }
    }

    pub fn check(&self, allow: &Allowlist) -> Result<(), CallerError> {
        let ok = match self {
            Self::Chromium { extension_id } => allow.chromium.iter().any(|a| a == extension_id),
            Self::Firefox { extension_id } => allow.firefox.iter().any(|a| a == extension_id),
        };
        if ok {
            Ok(())
        } else {
            Err(CallerError::NotAllowed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn chrome_origin_is_recognised_and_checked_against_the_allowlist() {
        let ok = Caller::from_args(&a(&["chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/", "--parent-window=123"])).unwrap();
        assert_eq!(ok.origin(), "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/");
        assert!(ok.check(&Allowlist::default()).is_ok());

        let other = Caller::from_args(&a(&["chrome-extension://abcdefghijklmnopabcdefghijklmnop/"])).unwrap();
        assert_eq!(other.check(&Allowlist::default()), Err(CallerError::NotAllowed), "well-formed but not ours");
    }

    #[test]
    fn the_store_extension_is_trusted_next_to_the_developer_one() {
        let store = Caller::from_args(&a(&["chrome-extension://keobnkoppcchnbpmmfodidkcedglbhnn/"])).unwrap();
        assert!(store.check(&Allowlist::default()).is_ok());
        assert_eq!(chromium_ids(), a(&[CHROME_EXTENSION_ID, "keobnkoppcchnbpmmfodidkcedglbhnn"]));
    }

    #[test]
    fn malformed_callers_are_refused() {
        assert_eq!(Caller::from_args(&[]), Err(CallerError::NoArguments));
        for bad in [
            "chrome-extension://short/",
            "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbcx/",
            "chrome-extension://GBCANJAAELIOALFDGGKHBBNBINMFGDBC/",
            "chrome-extension://gbcanjaaelioalfdggkhbbnbinmfgdbc/evil",
            "chrome-extension://",
            "https://gbcanjaaelioalfdggkhbbnbinmfgdbc/",
            "--parent-window=1",
            "gbcanjaaelioalfdggkhbbnbinmfgdbc",
            "",
        ] {
            assert!(Caller::from_args(&a(&[bad])).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn firefox_arguments() {
        let ok = Caller::from_args(&a(&["C:\\x\\firefox.json", "velox-fetch@velox.app"])).unwrap();
        assert!(ok.check(&Allowlist::default()).is_ok());
        assert_eq!(Caller::from_origin(&ok.origin()).unwrap(), ok);
        let other = Caller::from_args(&a(&["/x/firefox.json", "evil@evil.test"])).unwrap();
        assert_eq!(other.check(&Allowlist::default()), Err(CallerError::NotAllowed));
        assert!(Caller::from_args(&a(&["/x/firefox.json"])).is_err(), "id is required");
        assert!(Caller::from_args(&a(&["/x/firefox.json", "bad id;rm -rf"])).is_err());
        assert!(Caller::from_args(&a(&["not-a-manifest", "velox-fetch@velox.app"])).is_err());
    }

    #[test]
    fn origin_round_trips() {
        for c in
            [Caller::Chromium { extension_id: CHROME_EXTENSION_ID.into() }, Caller::Firefox { extension_id: FIREFOX_EXTENSION_ID.into() }]
        {
            assert_eq!(Caller::from_origin(&c.origin()).unwrap(), c);
        }
        assert!(Caller::from_origin("nonsense").is_err());
    }
}
