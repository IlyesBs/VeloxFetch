//! Remove secrets from text before it is logged or put in a report.
//!
//! What is hidden, wherever it appears in a line:
//! * the value after a secret name (`Authorization: ...`, `cookie=...`, `"password": "..."`, `("cookie", "...")`,
//!   `proxy_password=...`, `X-Amz-Signature=...`): see [`is_secret_name`];
//! * `Bearer ...` / `Basic ...` credentials;
//! * in every link: the user name and password (`https://user:pass@host`), every value of the query string and
//!   the fragment (signed download links carry their tokens there, under names nobody can list);
//! * optionally the user's home folder (`C:\Users\name` -> `~`), so a shared report does not carry the account name.
//!
//! The path of a link is kept (it names the file, which is what makes a log useful). A service that puts a
//! token in the path itself is not recognised: that is the one thing this cannot know.

use std::borrow::Cow;

/// What replaces a hidden value.
pub const MASK: &str = "[redacted]";

/// A name containing one of these holds a secret (checked case-insensitively, anywhere in the name).
const SECRET_PARTS: &[&str] = &[
    "authorization",
    "cookie",
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "apikey",
    "api_key",
    "api-key",
    "private_key",
    "private-key",
    "privatekey",
    "signature",
    "sessionid",
    "session_id",
    "sessid",
    "bearer",
];

/// Short names that are secrets on their own but too common to look for inside other words.
const SECRET_NAMES: &[&str] = &["pwd", "pass", "auth", "sig", "key", "sid", "session", "jwt", "hmac", "otp"];

/// Header-style names whose value runs to the end of the line (`Cookie: a=1; b=2`, `Authorization: Basic x`).
const WHOLE_LINE: &[&str] = &["authorization", "proxy-authorization", "proxy_authorization", "cookie", "set-cookie", "set_cookie"];

/// Does a field / header / query / JSON key called `name` hold a secret?
pub fn is_secret_name(name: &str) -> bool {
    let lower = name.trim_matches(|c: char| c == '"' || c == '\'').to_ascii_lowercase();
    SECRET_PARTS.iter().any(|p| lower.contains(p)) || SECRET_NAMES.contains(&lower.as_str())
}

/// [`Redactor::redact`] without hiding a home folder.
pub fn redact(text: &str) -> Cow<'_, str> {
    Redactor::default().redact(text)
}

/// Hides secrets (and, when given one, the home folder) in text.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    home: Option<String>,
}

impl Redactor {
    /// Also replace `home` (the user's home folder) with `~`.
    pub fn with_home(home: Option<&std::path::Path>) -> Self {
        let home = home.map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string()).filter(|h| h.len() > 3);
        Self { home }
    }

    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if text.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = links(text);
        if let Some(s) = pairs(&out) {
            out = Cow::Owned(s);
        }
        if let Some(s) = schemes(&out) {
            out = Cow::Owned(s);
        }
        if let Some(home) = &self.home {
            if let Some(s) = replace_home(&out, home) {
                out = Cow::Owned(s);
            }
        }
        out
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Where a link that starts at `start` ends: at white space or a quote, before an unbalanced closing bracket,
/// and without trailing sentence punctuation.
fn link_end(s: &[u8], start: usize) -> usize {
    let mut depth = [0i32; 3];
    let mut i = start;
    while i < s.len() {
        let b = s[i];
        if b.is_ascii_whitespace() || b.is_ascii_control() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b'`') {
            break;
        }
        let slot = match b {
            b'(' | b')' => 0,
            b'[' | b']' => 1,
            b'{' | b'}' => 2,
            _ => 3,
        };
        if slot < 3 {
            if matches!(b, b'(' | b'[' | b'{') {
                depth[slot] += 1;
            } else if depth[slot] == 0 {
                break;
            } else {
                depth[slot] -= 1;
            }
        }
        i += 1;
    }
    while i > start && matches!(s[i - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
        i -= 1;
    }
    i
}

/// Every `scheme://...` in `text` with its user name / password, query values and fragment hidden.
fn links(text: &str) -> Cow<'_, str> {
    let s = text.as_bytes();
    let mut out = String::new();
    let (mut copied, mut i) = (0usize, 0usize);
    let mut changed = false;
    while let Some(pos) = text[i..].find("://") {
        let sep = i + pos;
        let mut start = sep;
        while start > 0 && (s[start - 1].is_ascii_alphanumeric() || matches!(s[start - 1], b'+' | b'.' | b'-')) {
            start -= 1;
        }
        if sep - start < 2 || !s[start].is_ascii_alphabetic() {
            i = sep + 3;
            continue;
        }
        let end = link_end(s, sep + 3);
        let link = &text[start..end];
        let clean = clean_link(link, sep + 3 - start);
        if clean != link {
            out.push_str(&text[copied..start]);
            out.push_str(&clean);
            copied = end;
            changed = true;
        }
        i = end.max(sep + 3);
    }
    if !changed {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// `link` with `authority_at` = where the host part starts (just after `://`).
fn clean_link(link: &str, authority_at: usize) -> String {
    let rest = &link[authority_at..];
    let auth_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_len);
    let mut out = String::with_capacity(link.len());
    out.push_str(&link[..authority_at]);
    match authority.rfind('@') {
        Some(at) => {
            out.push_str(MASK);
            out.push_str(&authority[at..]);
        }
        None => out.push_str(authority),
    }
    let (before_fragment, fragment) = match tail.find('#') {
        Some(h) => (&tail[..h], Some(&tail[h + 1..])),
        None => (tail, None),
    };
    match before_fragment.find('?') {
        Some(q) => {
            out.push_str(&before_fragment[..=q]);
            let query = &before_fragment[q + 1..];
            let hidden: Vec<String> = query
                .split('&')
                .map(|item| match item.split_once('=') {
                    Some((k, v)) if !v.is_empty() && v != MASK => format!("{k}={MASK}"),
                    Some(_) => item.to_string(),
                    None if item.is_empty() || item == MASK => item.to_string(),
                    None => MASK.to_string(),
                })
                .collect();
            out.push_str(&hidden.join("&"));
        }
        None => out.push_str(before_fragment),
    }
    if let Some(f) = fragment {
        out.push('#');
        if !f.is_empty() {
            out.push_str(MASK);
        }
    }
    out
}

/// `name: value`, `name=value`, `"name": "value"` and `("name", "value")` with a secret name: the value is hidden.
fn pairs(text: &str) -> Option<String> {
    let s = text.as_bytes();
    let mut out = String::new();
    let (mut copied, mut i) = (0usize, 0usize);
    while i < s.len() {
        if !is_ident(s[i]) || (i > 0 && is_ident(s[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < s.len() && is_ident(s[i]) {
            i += 1;
        }
        let name = &text[start..i];
        if !is_secret_name(name) {
            continue;
        }
        let quoted = start > 0 && matches!(s[start - 1], b'"' | b'\'');
        let mut j = i;
        if quoted && j < s.len() && s[j] == s[start - 1] {
            j += 1;
        }
        while j < s.len() && matches!(s[j], b' ' | b'\t') {
            j += 1;
        }
        let sep_ok = j < s.len() && (matches!(s[j], b':' | b'=') || (quoted && s[j] == b','));
        if !sep_ok {
            continue;
        }
        j += 1;
        if j < s.len() && s[j] == b'>' {
            j += 1; // `=>`
        }
        while j < s.len() && matches!(s[j], b' ' | b'\t') {
            j += 1;
        }
        let (vstart, vend) = if j < s.len() && matches!(s[j], b'"' | b'\'') {
            let q = s[j];
            let mut k = j + 1;
            while k < s.len() && s[k] != q {
                k += if s[k] == b'\\' { 2 } else { 1 };
            }
            (j + 1, k.min(s.len()))
        } else if WHOLE_LINE.contains(&name.to_ascii_lowercase().as_str()) {
            let mut k = j;
            while k < s.len() && !matches!(s[k], b'\n' | b'\r' | b'"' | b'\'') {
                k += 1;
            }
            (j, k)
        } else {
            let mut k = j;
            while k < s.len() && !s[k].is_ascii_whitespace() && !matches!(s[k], b',' | b';' | b'&' | b')' | b']' | b'}' | b'"' | b'\'') {
                k += 1;
            }
            (j, k)
        };
        let value = &text[vstart..vend];
        if !value.trim().is_empty() && !text[vstart..].starts_with(MASK) {
            out.push_str(&text[copied..vstart]);
            out.push_str(MASK);
            copied = vend;
        }
        i = vend.max(i);
    }
    (copied > 0).then(|| {
        out.push_str(&text[copied..]);
        out
    })
}

/// `Bearer <token>` always; `Basic` / `Digest` / `Negotiate` / `NTLM <token>` when the token looks encoded
/// (so "Basic settings" stays readable).
fn schemes(text: &str) -> Option<String> {
    const SCHEMES: &[(&str, bool)] = &[("bearer", true), ("basic", false), ("digest", false), ("negotiate", false), ("ntlm", false)];
    let s = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let mut out = String::new();
    let (mut copied, mut i) = (0usize, 0usize);
    while i < s.len() {
        let hit = SCHEMES.iter().find(|(name, _)| {
            lower[i..].starts_with(name.as_bytes()) && (i == 0 || !is_ident(s[i - 1])) && s.get(i + name.len()) == Some(&b' ')
        });
        let Some((name, always)) = hit else {
            i += 1;
            continue;
        };
        let vstart = i + name.len() + 1;
        let mut vend = vstart;
        while vend < s.len() && (s[vend].is_ascii_alphanumeric() || matches!(s[vend], b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')) {
            vend += 1;
        }
        let token = &text[vstart..vend];
        let encoded = token.len() >= 8
            && (token.bytes().any(|b| b.is_ascii_digit() || matches!(b, b'=' | b'+' | b'/'))
                || (token[1..].bytes().any(|b| b.is_ascii_uppercase()) && token.bytes().any(|b| b.is_ascii_lowercase())));
        if !token.is_empty() && token != "[redacted]" && (*always || encoded) {
            out.push_str(&text[copied..vstart]);
            out.push_str(MASK);
            copied = vend;
        }
        i = vend.max(i + 1);
    }
    (copied > 0).then(|| {
        out.push_str(&text[copied..]);
        out
    })
}

/// Every occurrence of `home` (ASCII case-insensitively on Windows and macOS) replaced with `~`.
fn replace_home(text: &str, home: &str) -> Option<String> {
    let fold = cfg!(any(windows, target_os = "macos"));
    let (hay, needle) = if fold { (text.to_ascii_lowercase(), home.to_ascii_lowercase()) } else { (text.to_string(), home.to_string()) };
    let variants = [needle.clone(), needle.replace('\\', "/")];
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for v in variants.iter().filter(|v| !v.is_empty()) {
        let mut from = 0;
        while let Some(p) = hay[from..].find(v.as_str()) {
            let at = from + p;
            let end = at + v.len();
            // Only a whole folder name: `C:\Users\bob` must not match inside `C:\Users\bobby`.
            if hay.as_bytes().get(end).is_none_or(|b| matches!(b, b'\\' | b'/') || !is_ident(*b)) {
                hits.push((at, end));
            }
            from = end;
        }
    }
    if hits.is_empty() {
        return None;
    }
    hits.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, end) in hits {
        if at < copied {
            continue;
        }
        out.push_str(&text[copied..at]);
        out.push('~');
        copied = end;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact(s).into_owned()
    }

    #[test]
    fn header_values_are_hidden() {
        assert_eq!(r("Authorization: Basic dXNlcjpwYXNz"), "Authorization: [redacted]");
        assert_eq!(r("cookie: sid=abc; theme=dark\nnext line"), "cookie: [redacted]\nnext line");
        assert_eq!(r("proxy-authorization: Basic Zm9vOmJhcg=="), "proxy-authorization: [redacted]");
        assert_eq!(
            r(r#"headers=[("authorization", "Basic abc"), ("referer", "https://a.test/")]"#),
            r#"headers=[("authorization", "[redacted]"), ("referer", "https://a.test/")]"#
        );
        assert_eq!(r(r#"{"cookie": "a=b", "user-agent": "X"}"#), r#"{"cookie": "[redacted]", "user-agent": "X"}"#);
    }

    #[test]
    fn passwords_tokens_and_keys_are_hidden() {
        assert_eq!(r("password=hunter2 user=bob"), "password=[redacted] user=bob");
        assert_eq!(r(r#"{"proxyPassword":"s3cr3t","mode":"manual"}"#), r#"{"proxyPassword":"[redacted]","mode":"manual"}"#);
        assert_eq!(r("network.proxy_password = abc"), "network.proxy_password = [redacted]");
        assert_eq!(r("access_token: eyJhbGciOi.x.y"), "access_token: [redacted]");
        assert_eq!(r("api-key=K1; other=1"), "api-key=[redacted]; other=1");
        assert_eq!(r("Bearer abc.def.ghi"), "Bearer [redacted]");
        assert_eq!(r("sent Basic dXNlcjpwYXNz to the server"), "sent Basic [redacted] to the server");
        assert_eq!(r("Password => 'x y'"), "Password => '[redacted]'");
    }

    #[test]
    fn links_lose_their_credentials_query_values_and_fragment() {
        assert_eq!(
            r("error sending request for url (https://bob:pw@files.test/a/b.zip?token=XYZ&n=1#frag)"),
            "error sending request for url (https://[redacted]@files.test/a/b.zip?token=[redacted]&n=[redacted]#[redacted])"
        );
        assert_eq!(r("see http://x.test/p?SIGNED."), "see http://x.test/p?[redacted].");
        assert_eq!(r("socks5h://u:p@proxy.test:1080"), "socks5h://[redacted]@proxy.test:1080");
        assert_eq!(r("https://cdn.test/v/file.mp4"), "https://cdn.test/v/file.mp4");
        assert_eq!(
            r("https://s3.test/f?X-Amz-Signature=abc&X-Amz-Credential=AK%2F1"),
            "https://s3.test/f?X-Amz-Signature=[redacted]&X-Amz-Credential=[redacted]"
        );
        // Balanced brackets stay in the link, an unbalanced one ends it.
        assert_eq!(r("(https://x.test/a_(b)?k=v)"), "(https://x.test/a_(b)?k=[redacted])");
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for s in [
            "the browser's cookie, referrer and user agent were kept",
            "blocked IPC call from a window that is not authorized for it",
            "browser bridge: cannot write the token file; browser integration is off",
            "Basic settings changed",
            "segment failed, retrying segment=3 attempt=1 error=timed out",
            "download ended id=4 status=failed error_code=http_404 retries=2 http_status=404 connections=8",
            "",
            "日本語のファイル.zip ✓",
        ] {
            assert_eq!(r(s), s);
        }
    }

    #[test]
    fn redacting_twice_changes_nothing_and_garbage_never_panics() {
        for s in ["Authorization: Bearer abcdef", "https://u:p@h.test/?a=b#c", "password=x", "cookie=\"a"] {
            let once = r(s);
            assert_eq!(r(&once), once, "{s}");
        }
        for s in [
            "://",
            "a://",
            "x=://?",
            "\"password\"",
            "password",
            "password:",
            "cookie: \"",
            "https://@",
            "é://ü?ß=1",
            "Bearer ",
            "\u{0}password=\u{0}",
        ] {
            let _ = r(s);
        }
        let long = "password=".repeat(20_000) + &"https://a.test/?q=1 ".repeat(20_000);
        assert!(!r(&long).contains("q=1"));
    }

    #[test]
    fn secret_names() {
        for n in [
            "Authorization",
            "Cookie",
            "set-cookie",
            "proxy_password",
            "X-Amz-Security-Token",
            "apiKey",
            "PASSWD",
            "sid",
            "key",
            "\"password\"",
        ] {
            assert!(is_secret_name(n), "{n}");
        }
        for n in ["error", "attempt", "segment", "reason", "origin", "file", "status", "connections", "keyboard", "folders", "passenger"] {
            assert!(!is_secret_name(n), "{n}");
        }
    }

    #[test]
    fn the_home_folder_becomes_a_tilde() {
        let red = Redactor::with_home(Some(std::path::Path::new(if cfg!(windows) { r"C:\Users\Bob" } else { "/home/bob" })));
        if cfg!(windows) {
            assert_eq!(red.redact(r"saved to c:\users\bob\Downloads\a.zip"), r"saved to ~\Downloads\a.zip");
            assert_eq!(red.redact(r"C:/Users/Bob/x"), "~/x");
            assert_eq!(red.redact(r"C:\Users\Bobby\x"), r"C:\Users\Bobby\x");
        } else {
            assert_eq!(red.redact("saved to /home/bob/Downloads/a.zip"), "saved to ~/Downloads/a.zip");
            assert_eq!(red.redact("/home/bobby/x"), "/home/bobby/x");
        }
    }
}
