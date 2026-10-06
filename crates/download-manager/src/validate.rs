//! Input validation. Everything from the UI is untrusted: validate strictly, fail closed.

use std::path::{Path, PathBuf};

use downloader_core::Checksum;
use url::Url;

use crate::api::ManagerError;

const MAX_URL_LEN: usize = 8192;
const MAX_SUBDIR_DEPTH: usize = 8;
const MIN_SPEED_LIMIT: u64 = 1024; // 1 KiB/s
const MAX_SPEED_LIMIT: u64 = 1 << 40;

/// Only http(s) URLs with a host, of sane length and without control characters.
pub fn url(s: &str) -> Result<Url, ManagerError> {
    if s.is_empty() || s.len() > MAX_URL_LEN {
        return Err(ManagerError::invalid("URL is empty or too long"));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(ManagerError::invalid("URL contains control characters"));
    }
    let u = Url::parse(s.trim()).map_err(|e| ManagerError::invalid(format!("bad URL: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(ManagerError::invalid(format!("unsupported URL scheme `{}` (only http/https)", u.scheme())));
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(ManagerError::invalid("URL has no host"));
    }
    Ok(u)
}

/// A link whose user name / password (if any) are taken out: the link is what gets stored, shown
/// and logged, so it must never carry them. The credentials come back as an `Authorization`
/// header value, which is sealed before it can reach the disk.
pub fn split_credentials(mut u: Url) -> (Url, Option<String>) {
    if u.username().is_empty() && u.password().is_none() {
        return (u, None);
    }
    let decode = |s: &str| http_client::percent_decode(s).unwrap_or_else(|| s.to_string());
    let header = http_client::basic_authorization(&decode(u.username()), &decode(u.password().unwrap_or("")));
    let _ = u.set_username("");
    let _ = u.set_password(None);
    (u, Some(header))
}

/// Like [`url`], but a link with a user name or password is refused (streams do not support them).
pub fn url_without_credentials(s: &str) -> Result<Url, ManagerError> {
    let u = url(s)?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ManagerError::invalid("links with a user name or password are not supported here"));
    }
    Ok(u)
}

pub(crate) fn check_component(c: &str, what: &str) -> Result<(), ManagerError> {
    path_safety::check_component(c).map_err(|why| ManagerError::invalid(format!("invalid {what}: `{}` {why}", c.escape_debug())))
}

/// A bare file name: no separators at all (the engine sanitizes further).
pub fn filename(s: &str) -> Result<String, ManagerError> {
    if s.contains(['/', '\\']) {
        return Err(ManagerError::invalid("file name must not contain path separators"));
    }
    check_component(s, "file name")?;
    Ok(s.to_string())
}

/// Resolve a relative sub-folder under `root`. Absolute paths, drive letters, `..`, and
/// reserved/illegal names are rejected; symlinks cannot lead outside `root`.
pub fn subdir(root: &Path, sub: Option<&str>) -> Result<PathBuf, ManagerError> {
    let sub = sub.map(str::trim).unwrap_or("");
    let mut dir = root.to_path_buf();
    if !sub.is_empty() {
        if sub.starts_with(['/', '\\']) || sub.contains(':') {
            return Err(ManagerError::invalid("sub-folder must be relative to the download folder"));
        }
        let parts: Vec<&str> = sub.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || parts.len() > MAX_SUBDIR_DEPTH {
            return Err(ManagerError::invalid("sub-folder is empty or nested too deeply"));
        }
        for p in parts {
            check_component(p, "sub-folder name")?;
            dir.push(p);
        }
    }
    ensure_within(root, &dir)?;
    Ok(dir)
}

/// `dir` (which may not exist yet) must really live under `root` once symlinks are resolved.
pub fn ensure_within(root: &Path, dir: &Path) -> Result<(), ManagerError> {
    path_safety::ensure_within(root, dir).map_err(|e| match e {
        path_safety::PathError::Escapes => ManagerError::invalid("path escapes the download folder"),
        path_safety::PathError::Root(e) => ManagerError::Internal(format!("download folder: {e}")),
    })
}

pub fn category(s: &str) -> Result<String, ManagerError> {
    let ok = !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(s.to_string())
    } else {
        Err(ManagerError::invalid("category must be 1-32 characters of a-z, 0-9, `_` or `-`"))
    }
}

pub fn speed_limit(v: Option<u64>) -> Result<Option<u64>, ManagerError> {
    match v {
        Some(n) if !(MIN_SPEED_LIMIT..=MAX_SPEED_LIMIT).contains(&n) => {
            Err(ManagerError::invalid(format!("speed limit must be between {MIN_SPEED_LIMIT} and {MAX_SPEED_LIMIT} bytes/s")))
        }
        other => Ok(other),
    }
}

const MAX_REFERRER_LEN: usize = 2048;
const MAX_COOKIE_LEN: usize = 16 * 1024;
const MAX_UA_LEN: usize = 512;

/// Printable ASCII only (space through `~`): no CR/LF, NUL or other control characters that could
/// split or smuggle a header, and nothing outside what real Cookie / User-Agent values contain.
fn header_text(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Turn what a browser handed over into the extra request headers the engine will send.
/// Anything unexpected is rejected, never silently repaired.
pub fn browser_headers(ctx: &crate::api::BrowserContext) -> Result<Vec<(String, String)>, ManagerError> {
    let mut out = Vec::new();
    if let Some(r) = ctx.referrer.as_deref().filter(|r| !r.is_empty()) {
        if r.len() > MAX_REFERRER_LEN {
            return Err(ManagerError::invalid("referrer is too long"));
        }
        let mut u = url(r).map_err(|_| ManagerError::invalid("referrer must be an http(s) URL"))?;
        // What browsers send: no fragment, no credentials.
        u.set_fragment(None);
        let _ = u.set_username("");
        let _ = u.set_password(None);
        out.push(("referer".to_string(), u.to_string()));
    }
    if let Some(c) = ctx.cookies.as_deref().filter(|c| !c.is_empty()) {
        if ctx.incognito {
            return Err(ManagerError::invalid("cookies from a private window are never accepted"));
        }
        if c.len() > MAX_COOKIE_LEN || !header_text(c) {
            return Err(ManagerError::invalid("cookies are too long or contain forbidden characters"));
        }
        out.push(("cookie".to_string(), c.to_string()));
    }
    if let Some(ua) = ctx.user_agent.as_deref().filter(|u| !u.is_empty()) {
        if ua.len() > MAX_UA_LEN || !header_text(ua) {
            return Err(ManagerError::invalid("user agent is too long or contains forbidden characters"));
        }
        out.push(("user-agent".to_string(), ua.to_string()));
    }
    Ok(out)
}

const MAX_USERNAME_LEN: usize = 256;
const MAX_PASSWORD_LEN: usize = 1024;
const MAX_PROXY_URL_LEN: usize = 512;

/// The `Authorization` header for HTTP Basic authentication with these credentials.
/// A user name cannot contain `:` (the protocol splits on it); neither may contain control characters.
pub fn credentials(username: &str, password: &str) -> Result<String, ManagerError> {
    if username.is_empty() || username.len() > MAX_USERNAME_LEN || username.contains(':') || username.chars().any(char::is_control) {
        return Err(ManagerError::invalid("the user name is empty, too long, or contains `:` or control characters"));
    }
    if password.len() > MAX_PASSWORD_LEN || password.chars().any(char::is_control) {
        return Err(ManagerError::invalid("the password is too long or contains control characters"));
    }
    Ok(http_client::basic_authorization(username, password))
}

/// The extra headers a user typed in the Add dialog (referrer, cookies, user agent, user name and password),
/// checked with the same rules as those a browser hands over.
pub fn user_headers(req: &crate::api::NewDownload) -> Result<Vec<(String, String)>, ManagerError> {
    let ctx = crate::api::BrowserContext {
        referrer: req.referrer.clone(),
        cookies: req.cookies.clone(),
        user_agent: req.user_agent.clone(),
        incognito: false,
    };
    let mut out = browser_headers(&ctx)?;
    match (req.username.as_deref().map(str::trim).filter(|u| !u.is_empty()), req.password.as_deref()) {
        (Some(user), pass) => out.push(("authorization".to_string(), credentials(user, pass.unwrap_or(""))?)),
        (None, Some(p)) if !p.is_empty() => return Err(ManagerError::invalid("a password needs a user name")),
        _ => {}
    }
    Ok(out)
}

/// A proxy address: `http`, `https`, `socks5` or `socks5h`, a host, an optional port, and nothing else
/// (credentials go in their own fields so they can be sealed). Returned in normal form.
pub fn proxy_url(s: &str) -> Result<String, ManagerError> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_PROXY_URL_LEN || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ManagerError::invalid("enter the proxy as http://host:port (or socks5://host:port)"));
    }
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
    let u = Url::parse(&with_scheme).map_err(|_| ManagerError::invalid("the proxy address is not valid: use http://host:port"))?;
    if !matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err(ManagerError::invalid("the proxy must start with http://, https://, socks5:// or socks5h://"));
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(ManagerError::invalid("the proxy address has no host"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ManagerError::invalid("put the proxy user name and password in their own fields, not in the address"));
    }
    if !(u.path().is_empty() || u.path() == "/") || u.query().is_some() || u.fragment().is_some() {
        return Err(ManagerError::invalid("the proxy address must be only a host and a port"));
    }
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{}://{}{port}", u.scheme(), u.host_str().unwrap_or_default()))
}

/// A user agent chosen in the settings.
pub fn user_agent(s: &str) -> Result<String, ManagerError> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_UA_LEN || !header_text(s) {
        return Err(ManagerError::invalid("the user agent is empty, too long, or contains forbidden characters"));
    }
    Ok(s.to_string())
}

pub fn checksum(s: &str) -> Result<Checksum, ManagerError> {
    Checksum::parse(s).map_err(|e| ManagerError::invalid(format!("bad checksum: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-val-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ctx(r: Option<&str>, c: Option<&str>, u: Option<&str>, incognito: bool) -> crate::api::BrowserContext {
        crate::api::BrowserContext {
            referrer: r.map(str::to_string),
            cookies: c.map(str::to_string),
            user_agent: u.map(str::to_string),
            incognito,
        }
    }

    #[test]
    fn credentials_and_user_headers_are_checked() {
        assert_eq!(credentials("user", "p@ss").unwrap(), http_client::basic_authorization("user", "p@ss"));
        assert!(credentials("", "x").is_err());
        assert!(credentials("a:b", "x").is_err(), "a colon would change the user name the server sees");
        assert!(credentials("a\nb", "x").is_err());
        assert!(credentials("a", "p\r\nX-Evil: 1").is_err(), "no header injection through a password");
        assert!(credentials(&"a".repeat(MAX_USERNAME_LEN + 1), "").is_err());
        assert!(credentials("a", &"p".repeat(MAX_PASSWORD_LEN + 1)).is_err());
        assert!(credentials("\u{e9}l\u{e8}ve", "\u{fc}ber").is_ok(), "non-ASCII names are fine");

        let req = |u: Option<&str>, p: Option<&str>| crate::api::NewDownload {
            url: "https://x.test/f".into(),
            username: u.map(Into::into),
            password: p.map(Into::into),
            referrer: Some("https://x.test/page#top".into()),
            cookies: Some("sid=1".into()),
            user_agent: Some("Agent/1".into()),
            ..Default::default()
        };
        let h = user_headers(&req(Some("me"), Some("pw"))).unwrap();
        let names: Vec<_> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["referer", "cookie", "user-agent", "authorization"]);
        assert_eq!(h[0].1, "https://x.test/page", "fragment dropped like a browser does");
        assert!(user_headers(&req(None, Some("pw"))).is_err(), "a password without a user name is a mistake");
        assert_eq!(user_headers(&req(Some("  "), None)).unwrap().len(), 3, "a blank user name means no sign-in");
        let mut bad = req(None, None);
        bad.cookies = Some("a=1\r\nHost: evil".into());
        assert!(user_headers(&bad).is_err());
    }

    #[test]
    fn proxy_addresses_are_normalised_and_anything_else_is_refused() {
        assert_eq!(proxy_url("http://proxy.lan:3128").unwrap(), "http://proxy.lan:3128");
        assert_eq!(proxy_url("proxy.lan:8080").unwrap(), "http://proxy.lan:8080", "http is assumed");
        assert_eq!(proxy_url(" socks5h://127.0.0.1:9050/ ").unwrap(), "socks5h://127.0.0.1:9050");
        assert_eq!(proxy_url("https://[::1]:443").unwrap(), "https://[::1]", "default port is dropped by the URL parser");
        for bad in [
            "",
            "ftp://p:21",
            "file:///etc/passwd",
            "http://user:pw@p:1",
            "http://p:1/path",
            "http://p:1/?q",
            "http://p:1/#f",
            "http://",
            "http://p :1",
            "javascript:alert(1)",
            "http://p:1\r\nX: y",
        ] {
            assert!(proxy_url(bad).is_err(), "{bad:?}");
        }
        assert!(proxy_url(&format!("http://{}.test:1", "a".repeat(MAX_PROXY_URL_LEN))).is_err());
    }

    #[test]
    fn user_agents_are_single_printable_lines() {
        assert_eq!(user_agent(" Mozilla/5.0 (X11) ").unwrap(), "Mozilla/5.0 (X11)");
        for bad in ["", "   ", "a\r\nb", "caf\u{e9}", &"a".repeat(MAX_UA_LEN + 1)] {
            assert!(user_agent(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn browser_headers_are_validated_strictly() {
        let ok = browser_headers(&ctx(Some("https://site.test/p?q=1#frag"), Some("sid=1; a=b"), Some("Mozilla/5.0 (X11)"), false)).unwrap();
        assert_eq!(ok[0], ("referer".into(), "https://site.test/p?q=1".into()), "fragment is dropped");
        assert_eq!(ok[1].0, "cookie");
        assert_eq!(ok[2].0, "user-agent");
        assert!(browser_headers(&ctx(None, None, None, false)).unwrap().is_empty());
        assert!(browser_headers(&ctx(Some(""), Some(""), Some(""), false)).unwrap().is_empty(), "empty means absent");
        let cred = browser_headers(&ctx(Some("https://user:pw@site.test/p"), None, None, false)).unwrap();
        assert_eq!(cred[0].1, "https://site.test/p", "credentials never travel in a referrer");

        // Rejected outright.
        for bad_ref in ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,x", "not a url", "ftp://x.test/"] {
            assert!(browser_headers(&ctx(Some(bad_ref), None, None, false)).is_err(), "{bad_ref}");
        }
        for bad in ["a=b\r\nX-Injected: 1", "a=b\nSet-Cookie: x", "a=b\0", "a=\u{e9}", "a=b\tc", "\u{7f}"] {
            assert!(browser_headers(&ctx(None, Some(bad), None, false)).is_err(), "cookie {bad:?}");
            assert!(browser_headers(&ctx(None, None, Some(bad), false)).is_err(), "ua {bad:?}");
        }
        assert!(browser_headers(&ctx(None, Some(&"a".repeat(MAX_COOKIE_LEN + 1)), None, false)).is_err());
        assert!(browser_headers(&ctx(None, None, Some(&"a".repeat(MAX_UA_LEN + 1)), false)).is_err());
        assert!(browser_headers(&ctx(Some(&format!("https://x.test/{}", "a".repeat(MAX_REFERRER_LEN))), None, None, false)).is_err());
        assert!(browser_headers(&ctx(None, Some(&"a".repeat(MAX_COOKIE_LEN)), None, false)).is_ok(), "limit is inclusive");
    }

    #[test]
    fn cookies_from_private_windows_are_refused() {
        assert!(browser_headers(&ctx(Some("https://x.test/"), Some("sid=1"), None, true)).is_err());
        // A private window without cookies is fine.
        assert!(browser_headers(&ctx(Some("https://x.test/"), None, None, true)).is_ok());
    }

    #[test]
    fn urls() {
        assert!(url("https://example.com/a.zip").is_ok());
        assert!(url("http://127.0.0.1:8080/x").is_ok());
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://x/y",
            "data:text/plain,hi",
            "",
            "https://",
            "https://exa\nmple.com",
            "not a url",
        ] {
            assert!(url(bad).is_err(), "{bad} must be rejected");
        }
        assert!(url(&format!("https://a.b/{}", "x".repeat(9000))).is_err());
    }

    #[test]
    fn credentials_are_taken_out_of_links() {
        let (u, auth) = split_credentials(url("https://user:p%40ss@files.test/a.zip?x=1").unwrap());
        assert_eq!(u.as_str(), "https://files.test/a.zip?x=1");
        assert_eq!(auth.unwrap(), http_client::basic_authorization("user", "p@ss"), "percent-encoding is undone");
        let (u, auth) = split_credentials(url("https://only@files.test/").unwrap());
        assert_eq!((u.as_str(), auth.unwrap()), ("https://files.test/", http_client::basic_authorization("only", "")));
        let (u, auth) = split_credentials(url("https://files.test/a").unwrap());
        assert_eq!((u.as_str(), auth), ("https://files.test/a", None));
        assert!(url_without_credentials("https://u:p@h.test/x.m3u8").is_err());
        assert!(url_without_credentials("https://h.test/x.m3u8").is_ok());
    }

    #[test]
    fn subdirs_stay_inside_the_root() {
        let r = root();
        assert_eq!(subdir(&r, None).unwrap(), r);
        assert_eq!(subdir(&r, Some("")).unwrap(), r);
        assert_eq!(subdir(&r, Some("movies/2026")).unwrap(), r.join("movies").join("2026"));
        for bad in [
            "..",
            "../x",
            "a/../../x",
            "/etc",
            "\\windows",
            "C:\\Windows",
            "C:",
            "a/./b",
            "CON",
            "nul.txt",
            "a<b",
            "trail.",
            "x\0y",
            "1/2/3/4/5/6/7/8/9",
        ] {
            assert!(subdir(&r, Some(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn filenames_are_bare() {
        assert_eq!(filename("movie.mkv").unwrap(), "movie.mkv");
        for bad in ["a/b", "..\\x", "..", "", "nul", "a|b", "x.", "ctrl\u{7}"] {
            assert!(filename(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn misc_fields() {
        assert!(category("video_hd-1").is_ok());
        for bad in ["", "has space", "a/b", "é", &"x".repeat(33)] {
            assert!(category(bad).is_err(), "{bad:?}");
        }
        assert_eq!(speed_limit(None).unwrap(), None);
        assert_eq!(speed_limit(Some(1 << 20)).unwrap(), Some(1 << 20));
        assert!(speed_limit(Some(0)).is_err() && speed_limit(Some(10)).is_err() && speed_limit(Some(u64::MAX)).is_err());
        assert!(checksum(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(checksum("md5:abc").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn symlink_or_junction_cannot_escape() {
        let r = root();
        let outside = std::env::temp_dir().join(format!("velox-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let link = r.join("link");
        let _ = std::fs::remove_dir_all(&link);
        // A directory junction needs no privileges on Windows.
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", link.to_str().unwrap(), outside.to_str().unwrap()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            assert!(subdir(&r, Some("link/inner")).is_err(), "junction escaping the root must be rejected");
            let _ = std::fs::remove_dir(&link);
        }
    }
}

#[cfg(test)]
mod robustness {
    use super::*;

    /// Small deterministic generator: the same inputs on every run and every OS, so a failure reproduces.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[allow(dead_code)]
    /// Damage `seed` the ways a hostile server or a bit-flip would: cut, repeat, splice, replace.
    fn mutate(rng: &mut Rng, seed: &str, pool: &[&str]) -> String {
        let mut chars: Vec<char> = seed.chars().collect();
        for _ in 0..1 + rng.below(6) {
            if chars.is_empty() {
                break;
            }
            let at = rng.below(chars.len());
            match rng.below(5) {
                0 => chars.truncate(at),
                1 => {
                    let end = (at + 1 + rng.below(40)).min(chars.len());
                    let piece: Vec<char> = chars[at..end].to_vec();
                    for _ in 0..1 + rng.below(30) {
                        for (k, c) in piece.iter().enumerate() {
                            chars.insert(at + k, *c);
                        }
                    }
                }
                2 => {
                    let end = (at + 1 + rng.below(20)).min(chars.len());
                    chars.drain(at..end);
                }
                3 => {
                    let t: Vec<char> = pool[rng.below(pool.len())].chars().collect();
                    for (k, c) in t.iter().enumerate() {
                        chars.insert(at + k, *c);
                    }
                }
                _ => chars[at] = char::from_u32(rng.next() as u32 % 0x3000).unwrap_or('?'),
            }
        }
        chars.into_iter().collect()
    }

    #[test]
    fn nothing_the_ui_can_type_crashes_validation_or_escapes_the_download_folder() {
        let root = std::env::temp_dir().join(format!("velox-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let pool = [
            "..", "/", "\\", ":", "C:", "\\\\?\\", "//", "%2e%2e", "\u{202e}", "\u{0}", "CON", "a", "b", " ", ".", "~", "$", "*", "?",
            "\u{e9}", "http://", "@", "[", "]",
        ];
        let mut rng = Rng(99);
        for _ in 0..10_000 {
            let mut s = String::new();
            for _ in 0..rng.below(14) {
                s.push_str(pool[rng.below(pool.len())]);
            }
            if let Ok(dir) = subdir(&root, Some(&s)) {
                let canon = root.canonicalize().unwrap();
                let shown = dir.components().any(|c| matches!(c, std::path::Component::ParentDir));
                assert!(!shown, "{s:?} produced {dir:?}");
                assert!(ensure_within(&root, &dir).is_ok(), "{s:?}");
                assert!(dir.starts_with(&root) || dir.starts_with(&canon), "{s:?} -> {dir:?}");
            }
            let _ = filename(&s);
            let _ = url(&s);
            let _ = url_without_credentials(&s);
            let _ = category(&s);
            let _ = checksum(&s);
            if let Ok(u) = url(&s) {
                let (clean, _) = split_credentials(u);
                assert!(clean.username().is_empty() && clean.password().is_none());
            }
            let ctx = crate::api::BrowserContext {
                referrer: Some(s.clone()),
                cookies: Some(s.clone()),
                user_agent: Some(s.clone()),
                incognito: false,
            };
            if let Ok(h) = browser_headers(&ctx) {
                assert!(h.iter().all(|(_, v)| !v.contains(['\r', '\n', '\0'])), "header injection from {s:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
