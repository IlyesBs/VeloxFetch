//! The message schema shared by the extension, the native host and the app.
//!
//! Everything here is validated as if it came from a hostile web page (a compromised content
//! script, a malicious extension talking to the host, another local process):
//!
//! * a **closed set of commands** (`ping`, `download`); anything else is rejected;
//! * strict field sets (`deny_unknown_fields`), typed values, hard length limits;
//! * URLs are *data*, never executable input: only `http`/`https`, no credentials, no control
//!   characters. Nothing received is ever passed to a shell or interpreted as a command;
//! * header-bound values (cookies, user agent) are printable ASCII only, so they cannot split or
//!   smuggle headers.
//!
//! Validation *rejects*; it never "repairs". Both the host and the app run it (defense in depth).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

pub const MAX_URL_LEN: usize = 8192;
pub const MAX_FILENAME_LEN: usize = 255;
pub const MAX_REFERRER_LEN: usize = 2048;
pub const MAX_COOKIE_LEN: usize = 16 * 1024;
pub const MAX_UA_LEN: usize = 512;

/// Where in the browser the handoff came from (for display and auditing only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    ContextMenu,
    Intercept,
    Popup,
    LinkList,
    /// The "Download video" bar the extension shows over a video it detected.
    VideoBar,
}

/// What the extension saw behind a link it hands over as a video (absent = an ordinary file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaHint {
    /// An HLS (`.m3u8`) or DASH (`.mpd`) stream description the page loaded.
    Stream,
    /// A video page of a site the app reads through yt-dlp (YouTube...).
    Page,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadRequest {
    pub url: String,
    pub filename: Option<String>,
    /// The page the link was on.
    pub referrer: Option<String>,
    /// A `Cookie` header value for `url` (the extension sends it only when the user asked for the
    /// handoff and never from a private window).
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    #[serde(default)]
    pub incognito: bool,
    pub source: Source,
    /// Set when the link is a video stream or a video page rather than a file.
    #[serde(default)]
    pub media: Option<MediaHint>,
}

// Never prints the cookies, and shows links without their query / fragment (signed links carry a token there).
impl std::fmt::Debug for DownloadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bare = |u: &str| u.split(['?', '#']).next().unwrap_or("").to_string();
        f.debug_struct("DownloadRequest")
            .field("url", &bare(&self.url))
            .field("filename", &self.filename)
            .field("referrer", &self.referrer.as_deref().map(bare))
            .field("cookies", &self.cookies.as_ref().map(|_| "***"))
            .field("incognito", &self.incognito)
            .field("source", &self.source)
            .field("media", &self.media)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Ping,
    Download(DownloadRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("the message is not valid JSON")]
    BadJson,
    #[error("the message must be a JSON object")]
    BadShape,
    #[error("unknown command")]
    UnknownCommand,
    #[error("unexpected or malformed fields: {0}")]
    BadFields(String),
    #[error("{0}")]
    Invalid(String),
}

impl ProtocolError {
    /// Stable machine-readable code (what the extension switches on).
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadJson => "bad_json",
            Self::BadShape => "bad_shape",
            Self::UnknownCommand => "unknown_command",
            Self::BadFields(_) => "bad_fields",
            Self::Invalid(_) => "invalid",
        }
    }
}

fn invalid(msg: &str) -> ProtocolError {
    ProtocolError::Invalid(msg.to_string())
}

/// Parse and fully validate one browser message.
pub fn parse_message(bytes: &[u8]) -> Result<Message, ProtocolError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| ProtocolError::BadJson)?;
    parse_value(v)
}

pub fn parse_value(v: Value) -> Result<Message, ProtocolError> {
    let Value::Object(mut obj) = v else { return Err(ProtocolError::BadShape) };
    let command = match obj.remove("command") {
        Some(Value::String(s)) => s,
        _ => return Err(ProtocolError::UnknownCommand),
    };
    match command.as_str() {
        "ping" => {
            if let Some(k) = obj.keys().next() {
                return Err(ProtocolError::BadFields(format!("`{}` is not allowed on ping", k.chars().take(32).collect::<String>())));
            }
            Ok(Message::Ping)
        }
        "download" => {
            let req: DownloadRequest = serde_json::from_value(Value::Object(Map::from_iter(obj)))
                .map_err(|e| ProtocolError::BadFields(e.to_string().chars().take(160).collect()))?;
            validate_download(&req)?;
            Ok(Message::Download(req))
        }
        _ => Err(ProtocolError::UnknownCommand),
    }
}

/// Printable ASCII (space through `~`) only.
fn header_text(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn http_url(s: &str, what: &str, max: usize) -> Result<Url, ProtocolError> {
    if s.is_empty() || s.len() > max {
        return Err(ProtocolError::Invalid(format!("{what} is empty or too long")));
    }
    if s.chars().any(char::is_control) {
        return Err(ProtocolError::Invalid(format!("{what} contains control characters")));
    }
    let u = Url::parse(s).map_err(|_| ProtocolError::Invalid(format!("{what} is not a valid URL")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(ProtocolError::Invalid(format!("{what} must be an http or https URL")));
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(ProtocolError::Invalid(format!("{what} has no host")));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ProtocolError::Invalid(format!("{what} must not contain credentials")));
    }
    Ok(u)
}

pub fn validate_download(r: &DownloadRequest) -> Result<(), ProtocolError> {
    http_url(&r.url, "url", MAX_URL_LEN)?;
    if let Some(f) = &r.filename {
        let bad_char = |c: char| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if f.is_empty() || f.chars().count() > MAX_FILENAME_LEN || f.chars().any(bad_char) || f == "." || f == ".." {
            return Err(invalid("filename must be a plain file name"));
        }
    }
    if let Some(r) = r.referrer.as_deref() {
        http_url(r, "referrer", MAX_REFERRER_LEN)?;
    }
    if let Some(c) = &r.cookies {
        if c.len() > MAX_COOKIE_LEN || !header_text(c) {
            return Err(invalid("cookies are too long or contain forbidden characters"));
        }
        if r.incognito && !c.is_empty() {
            return Err(invalid("cookies from a private window are never accepted"));
        }
    }
    if let Some(u) = &r.user_agent {
        if u.len() > MAX_UA_LEN || !header_text(u) {
            return Err(invalid("user agent is too long or contains forbidden characters"));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ replies

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// What goes back to the browser (and over the host <-> app link).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Reply {
    pub fn ok(result: Value) -> Self {
        Self { ok: true, result: Some(result), error: None }
    }

    pub fn err(code: &str, message: &str) -> Self {
        // Messages are short and never carry paths, stack traces or the offending input.
        Self { ok: false, result: None, error: Some(ErrorBody { code: code.to_string(), message: message.chars().take(200).collect() }) }
    }

    pub fn from_protocol(e: &ProtocolError) -> Self {
        Self::err(e.code(), &e.to_string())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| br#"{"ok":false,"error":{"code":"internal","message":"internal error"}}"#.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(v: Value) -> Result<Message, ProtocolError> {
        parse_value(v)
    }

    fn dl(over: impl FnOnce(&mut Value)) -> Value {
        let mut v = json!({"command":"download","url":"https://example.com/a.zip","source":"contextMenu"});
        over(&mut v);
        v
    }

    #[test]
    fn accepts_the_two_commands() {
        assert_eq!(parse(json!({"command":"ping"})).unwrap(), Message::Ping);
        let Message::Download(d) = parse(dl(|v| {
            v["filename"] = json!("a.zip");
            v["referrer"] = json!("https://example.com/page");
            v["cookies"] = json!("sid=1; a=b");
            v["userAgent"] = json!("Mozilla/5.0");
        }))
        .unwrap() else {
            panic!("expected download")
        };
        assert_eq!((d.url.as_str(), d.source, d.incognito), ("https://example.com/a.zip", Source::ContextMenu, false));
        assert_eq!(d.media, None, "no hint = an ordinary file");
    }

    #[test]
    fn a_video_hint_is_a_closed_set() {
        for (hint, want) in [("stream", MediaHint::Stream), ("page", MediaHint::Page)] {
            let Message::Download(d) = parse(dl(|v| {
                v["media"] = json!(hint);
                v["source"] = json!("videoBar");
            }))
            .unwrap() else {
                panic!("expected download")
            };
            assert_eq!((d.media, d.source), (Some(want), Source::VideoBar));
        }
        for bad in [json!("exec"), json!("Stream"), json!(1), json!({"kind": "page"}), json!("")] {
            assert!(matches!(parse(dl(|v| v["media"] = bad.clone())), Err(ProtocolError::BadFields(_))), "{bad}");
        }
    }

    #[test]
    fn rejects_unknown_commands_and_shapes() {
        for cmd in ["exec", "run", "shell", "open", "eval", "readFile", "download ", "Download", "", "ping\0"] {
            assert_eq!(parse(json!({"command": cmd})), Err(ProtocolError::UnknownCommand), "{cmd:?}");
        }
        assert_eq!(parse(json!({})), Err(ProtocolError::UnknownCommand));
        assert_eq!(parse(json!({"command": 5})), Err(ProtocolError::UnknownCommand));
        assert_eq!(parse(json!({"command": null})), Err(ProtocolError::UnknownCommand));
        assert_eq!(parse(json!({"command": ["download"]})), Err(ProtocolError::UnknownCommand));
        for shape in [json!([]), json!("download"), json!(1), json!(null), json!(true)] {
            assert_eq!(parse(shape), Err(ProtocolError::BadShape));
        }
        assert_eq!(parse_message(b"{not json"), Err(ProtocolError::BadJson));
        assert_eq!(parse_message(b""), Err(ProtocolError::BadJson));
        assert_eq!(parse_message(&[0xff, 0xfe, 0x00]), Err(ProtocolError::BadJson));
    }

    #[test]
    fn extra_fields_are_rejected_on_every_command() {
        assert!(matches!(parse(json!({"command":"ping","x":1})), Err(ProtocolError::BadFields(_))));
        for extra in ["exec", "path", "cmd", "headers", "destination", "subdir", "priority", "start", "checksum"] {
            let r = parse(dl(|v| v[extra] = json!("x")));
            assert!(matches!(r, Err(ProtocolError::BadFields(_))), "{extra}: {r:?}");
        }
    }

    #[test]
    fn wrong_types_are_rejected() {
        for over in [
            |v: &mut Value| v["url"] = json!(5),
            |v: &mut Value| v["url"] = json!(null),
            |v: &mut Value| v["url"] = json!(["https://a.test/"]),
            |v: &mut Value| v["source"] = json!("hackers"),
            |v: &mut Value| v["source"] = json!(1),
            |v: &mut Value| v["incognito"] = json!("yes"),
            |v: &mut Value| v["cookies"] = json!({"a": 1}),
        ] {
            assert!(matches!(parse(dl(over)), Err(ProtocolError::BadFields(_))));
        }
        assert!(matches!(parse(json!({"command":"download","source":"popup"})), Err(ProtocolError::BadFields(_))), "url is required");
        assert!(
            matches!(parse(json!({"command":"download","url":"https://a.test/"})), Err(ProtocolError::BadFields(_))),
            "source is required"
        );
    }

    #[test]
    fn urls_are_data_only_http_and_https() {
        for bad in [
            "javascript:alert(1)",
            "file:///C:/Windows/win.ini",
            "data:text/html,<script>1</script>",
            "ftp://example.com/a",
            "chrome://settings",
            "about:blank",
            "blob:https://a.test/uuid",
            "vbscript:x",
            "\\\\server\\share\\a.exe",
            "C:\\Windows\\System32\\calc.exe",
            "calc.exe",
            "https://",
            "https://user:pass@example.com/a",
            "https://a.test/a\nb",
            "https://a.test/a\0b",
            "https://a.test/\u{7f}",
            "",
            "   ",
        ] {
            assert!(matches!(parse(dl(|v| v["url"] = json!(bad))), Err(ProtocolError::Invalid(_))), "{bad:?}");
        }
        assert!(parse(dl(|v| v["url"] = json!("http://127.0.0.1:8080/a?b=c#d"))).is_ok());
        let long = format!("https://a.test/{}", "a".repeat(MAX_URL_LEN));
        assert!(parse(dl(|v| v["url"] = json!(long))).is_err());
    }

    #[test]
    fn debug_output_never_contains_cookies_or_link_queries() {
        let m = parse(dl(|v| {
            v["cookies"] = json!("sid=TOPSECRET");
            v["url"] = json!("https://h.test/a.zip?token=SIGNED#x");
            v["referrer"] = json!("https://h.test/page?session=SECRET2");
        }))
        .unwrap();
        let shown = format!("{m:?}");
        for secret in ["TOPSECRET", "SIGNED", "SECRET2", "sid="] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        assert!(shown.contains("h.test/a.zip"));
    }

    #[test]
    fn filenames_are_plain_names() {
        for bad in
            ["", "..", ".", "a/b.zip", "a\\b.zip", "C:evil.exe", "a*.zip", "a?.zip", "a\"b", "a<b", "a|b", "a\nb", "a\0b", &"x".repeat(256)]
        {
            assert!(parse(dl(|v| v["filename"] = json!(bad))).is_err(), "{bad:?}");
        }
        for ok in ["a.zip", "My File (1).tar.gz", "r\u{e9}sum\u{e9}.pdf", &"x".repeat(255)] {
            assert!(parse(dl(|v| v["filename"] = json!(ok))).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn header_bound_values_cannot_inject_headers() {
        for bad in ["a=b\r\nX-Evil: 1", "a=b\nSet-Cookie: x", "a=b\0", "a=\u{e9}", "a=b\tc", "\u{7f}", "\u{1b}[31m"] {
            assert!(parse(dl(|v| v["cookies"] = json!(bad))).is_err(), "cookie {bad:?}");
            assert!(parse(dl(|v| v["userAgent"] = json!(bad))).is_err(), "ua {bad:?}");
        }
        assert!(parse(dl(|v| v["cookies"] = json!("a".repeat(MAX_COOKIE_LEN)))).is_ok());
        assert!(parse(dl(|v| v["cookies"] = json!("a".repeat(MAX_COOKIE_LEN + 1)))).is_err());
        assert!(parse(dl(|v| v["userAgent"] = json!("a".repeat(MAX_UA_LEN + 1)))).is_err());
    }

    #[test]
    fn referrer_must_be_an_http_url_and_private_windows_send_no_cookies() {
        for bad in ["javascript:1", "file:///x", "not a url", "https://u:p@a.test/"] {
            assert!(parse(dl(|v| v["referrer"] = json!(bad))).is_err(), "{bad:?}");
        }
        assert!(parse(dl(|v| {
            v["incognito"] = json!(true);
            v["cookies"] = json!("sid=1");
        }))
        .is_err());
        assert!(parse(dl(|v| v["incognito"] = json!(true))).is_ok(), "a private window may hand over a link, just no cookies");
    }

    #[test]
    fn deeply_nested_json_is_a_clean_error_not_a_crash() {
        let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(parse_message(deep.as_bytes()), Err(ProtocolError::BadJson));
        let deep_obj = format!("{{\"command\":\"ping\",\"x\":{}1{}}}", "{\"a\":".repeat(3000), "}".repeat(3000));
        assert!(parse_message(deep_obj.as_bytes()).is_err());
    }

    #[test]
    fn replies_never_echo_the_input_and_are_bounded() {
        let e = Reply::from_protocol(&ProtocolError::UnknownCommand);
        let s = String::from_utf8(e.to_bytes()).unwrap();
        assert!(s.contains("unknown_command") && !s.contains("null"));
        let long = Reply::err("x", &"m".repeat(5000));
        assert!(long.error.unwrap().message.len() <= 200);
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
    fn garbage_from_the_browser_side_never_crashes_the_parser() {
        let valid = r#"{"command":"download","url":"https://example.com/a.zip","filename":"a.zip","referrer":"https://example.com/p","cookies":"sid=1","source":"contextMenu"}"#;
        let pool = [
            "\"",
            "{",
            "}",
            "[",
            "]",
            ",",
            ":",
            "null",
            "1e999",
            "-0",
            "\u{0}",
            "\\u0000",
            "\\ud800",
            "true",
            "\"command\"",
            "\"ping\"",
            "99999999999999999999",
        ];
        let mut rng = Rng(3);
        let (mut parsed, mut refused) = (0, 0);
        for i in 0..8_000 {
            let bytes: Vec<u8> = if i % 4 == 0 {
                (0..rng.below(400)).map(|_| rng.next() as u8).collect()
            } else {
                mutate(&mut rng, valid, &pool).into_bytes()
            };
            match parse_message(&bytes) {
                Ok(Message::Download(d)) => {
                    parsed += 1;
                    // Whatever got through is still bounded and clean.
                    assert!(d.url.len() <= 8192 && !d.url.chars().any(char::is_control), "{d:?}");
                    assert!(d.cookies.as_deref().is_none_or(|c| c.len() <= 16 * 1024));
                }
                Ok(Message::Ping) => parsed += 1,
                Err(_) => refused += 1,
            }
        }
        assert!(parsed > 0 && refused > 0, "{parsed}/{refused}");
    }

    #[test]
    fn deeply_nested_json_is_refused_without_overflowing_the_stack() {
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(parse_message(deep.as_bytes()).is_err());
        let deep_obj = format!("{}1{}", "{\"a\":".repeat(100_000), "}".repeat(100_000));
        assert!(parse_message(deep_obj.as_bytes()).is_err());
    }
}
