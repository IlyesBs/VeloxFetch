//! Turn the engine's technical error text into a sentence a user can act on.
//!
//! The engine stores errors as text (`HTTP status 404`, `I/O error: ... (os error 112)`); the UI shows
//! [`explain`]'s sentence, and keeps the original for the details view.

use serde::Serialize;

/// Something only the user can fix, so the UI can offer the right button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Needs {
    /// The server wants a user name and password.
    SignIn,
    /// The proxy wants a user name and password (Settings > Network).
    ProxySignIn,
    /// The disk is full.
    Space,
    /// The folder cannot be written to (permissions, a locked file, a missing drive).
    Folder,
}

/// 1536 -> "1.5 KB" (binary units, like file managers show).
fn size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// The two numbers of "not enough free disk space: N bytes needed, M available".
pub(crate) fn space_numbers(raw: &str) -> Option<(u64, u64)> {
    let rest = raw.split("not enough free disk space: ").nth(1)?;
    let mut nums = rest.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse::<u64>().ok());
    Some((nums.next()??, nums.next()??))
}

fn http_status(raw: &str) -> Option<u16> {
    let rest = raw.split("HTTP status ").nth(1)?;
    rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

/// A plain sentence for `raw`, plus what the user can do about it. Unknown errors are passed through.
pub fn explain(raw: &str) -> (String, Option<Needs>) {
    let (text, needs, _) = classify(raw);
    (text, needs)
}

/// A short, stable name for the error (`http_404`, `disk_full`, `timeout`...): for logs, reports and support.
pub fn error_code(raw: &str) -> String {
    classify(raw).2
}

fn classify(raw: &str) -> (String, Option<Needs>, String) {
    let lower = raw.to_ascii_lowercase();
    let say = |s: &str, n: Option<Needs>, code: &str| (s.to_string(), n, code.to_string());
    if let Some(code) = http_status(raw) {
        let c = format!("http_{code}");
        return match code {
            401 => say("The server asks for a user name and password.", Some(Needs::SignIn), &c),
            407 => say("The proxy asks for a user name and password: set them in Settings > Network.", Some(Needs::ProxySignIn), &c),
            403 => say(
                "The server refused access. The link may have expired: start the download again from the page, or sign in.",
                Some(Needs::SignIn),
                &c,
            ),
            404 | 410 => say("The file is not on the server (the link is broken or has expired).", None, &c),
            416 => say("The server could not continue where the download stopped. Retry starts it again.", None, &c),
            429 => say("The server is limiting downloads right now. Retry in a moment.", None, &c),
            408 | 504 => say("The server took too long to answer. Retry in a moment.", None, &c),
            500..=599 => (format!("The server had a problem (error {code}). Retry later."), None, c),
            _ => (format!("The server answered with error {code}."), None, c),
        };
    }
    if let Some((needed, free)) = space_numbers(raw) {
        return (
            format!("Not enough free space on the disk: this file needs {}, only {} is free. Free some space (or choose another folder), then Retry.", size(needed), size(free)),
            Some(Needs::Space),
            "disk_full".into(),
        );
    }
    if lower.contains("no space left")
        || lower.contains("not enough space on the disk")
        || lower.contains("os error 112")
        || lower.contains("storage full")
        || lower.contains("disk quota")
    {
        return say(
            "The disk is full. Free some space, then press Retry: nothing already downloaded is lost.",
            Some(Needs::Space),
            "disk_full",
        );
    }
    if lower.contains("access is denied") || lower.contains("permission denied") || lower.contains("being used by another process") {
        return say(
            "VeloxFetch could not write the file: the folder is protected or the file is open in another program. Close it or choose another folder, then Retry.",
            Some(Needs::Folder),
            "access_denied",
        );
    }
    if lower.contains("not allowed") && lower.contains("folder") {
        return say(
            "That folder is not one VeloxFetch may write to. Choose the folder again, then Retry.",
            Some(Needs::Folder),
            "folder_not_allowed",
        );
    }
    if lower.contains("web page instead of the file") {
        return say(
            "The site sent a web page instead of the file (usually a sign-in or \"file not available\" page), so nothing was saved. Download it from your browser with \"Send cookies\" turned on in the VeloxFetch extension, or check that the file is shared publicly.",
            None,
            "web_page",
        );
    }
    if lower.contains("checksum mismatch") {
        return say("The file is not the one expected: its checksum does not match, so it was not kept.", None, "checksum_mismatch");
    }
    if lower.contains("destination already exists") {
        return say("A file with this name already exists in the folder. Rename or move it, then Retry.", None, "file_exists");
    }
    if lower.contains("size mismatch") || lower.contains("ended early") {
        return say("The server sent less than it announced. Retry continues the download.", None, "incomplete");
    }
    if lower.contains("redirect") && lower.contains("refused") {
        return say("The link redirected to an address on this computer or network, which is not allowed.", None, "redirect_refused");
    }
    if lower.contains("redirect") && (lower.contains("too many") || lower.contains("loop") || lower.contains("error following")) {
        return say("The link keeps redirecting (too many redirects or a loop), so the file could not be reached.", None, "redirect_loop");
    }
    if lower.contains("file changed on the server") {
        return say(
            "The file changed on the server during the download, so its new parts were not mixed with the saved ones. Retry checks the server again and starts over safely if needed.",
            None,
            "source_changed",
        );
    }
    if lower.contains("cannot finish:") {
        return say(
            "The saved parts do not cover the whole file, so it was not finished. Retry downloads what is missing.",
            None,
            "incomplete",
        );
    }
    if lower.contains("did not honor range") {
        return say(
            "The server stopped sending parts of the file, so the saved part cannot be continued as it is. Retry checks the server again and starts over safely if needed.",
            None,
            "range_not_honored",
        );
    }
    if lower.contains("state store error") {
        return say(
            "VeloxFetch could not save the download's progress on this computer. Retry; if it happens again, restart VeloxFetch.",
            None,
            "persistence",
        );
    }
    if lower.contains("unsupported url scheme") || lower.contains("invalid url") {
        return say("This link cannot be downloaded as it is (it is not a valid web address for a file).", None, "invalid_request");
    }
    if lower.contains("download cancelled") {
        return say("The download was cancelled.", None, "cancelled");
    }
    if lower.contains("dns") || lower.contains("resolve") || lower.contains("no such host") {
        return say("The server's address could not be found. Check the link and your internet connection.", None, "dns");
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return say("The server stopped answering. Retry continues the download.", None, "timeout");
    }
    if lower.contains("error sending request") || lower.contains("connection") || lower.contains("connect") {
        return say("Could not reach the server. Check your internet connection (or proxy), then Retry.", None, "connection");
    }
    if lower.contains("invalid proxy") {
        return say("The proxy address is not valid. Check Settings > Network.", Some(Needs::ProxySignIn), "proxy_invalid");
    }
    (raw.to_string(), None, "other".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_errors_become_plain_sentences_with_the_right_action() {
        assert_eq!(explain("HTTP status 401").1, Some(Needs::SignIn));
        assert_eq!(explain("request failed: HTTP status 407").1, Some(Needs::ProxySignIn));
        assert!(explain("HTTP status 404").0.contains("not on the server"));
        assert!(explain("HTTP status 503").0.contains("error 503"));
        assert_eq!(explain("I/O error: There is not enough space on the disk. (os error 112)").1, Some(Needs::Space));
        assert_eq!(explain("I/O error: No space left on device (os error 28)").1, Some(Needs::Space));
        let (text, needs) = explain("I/O error: not enough free disk space: 1125899906842624 bytes needed, 1073741824 available");
        assert_eq!(needs, Some(Needs::Space));
        assert!(text.contains("needs 1.0 PB, only 1.0 GB is free"), "{text}");
        assert_eq!(explain("I/O error: Access is denied. (os error 5)").1, Some(Needs::Folder));
        assert!(explain("checksum mismatch: expected a, got b").0.contains("checksum"));
        assert!(explain("request failed: error sending request for url (https://x.test/)").0.contains("reach the server"));
    }

    #[test]
    fn every_error_has_a_short_stable_code() {
        assert_eq!(error_code("HTTP status 404"), "http_404");
        assert_eq!(error_code("request failed: HTTP status 503"), "http_503");
        assert_eq!(error_code("I/O error: No space left on device (os error 28)"), "disk_full");
        assert_eq!(error_code("I/O error: not enough free disk space: 5 bytes needed, 1 available"), "disk_full");
        assert_eq!(error_code("I/O error: Access is denied. (os error 5)"), "access_denied");
        assert_eq!(error_code("request failed: error sending request for url (https://x.test/): operation timed out"), "timeout");
        assert_eq!(error_code("request failed: error sending request for url (https://x.test/)"), "connection");
        assert_eq!(error_code("checksum mismatch: expected a, got b"), "checksum_mismatch");
        assert_eq!(
            error_code("the file changed on the server during the download: Content-Range total Some(5), expected 4"),
            "source_changed"
        );
        assert_eq!(error_code("internal error: cannot finish: segment 3 is not complete"), "incomplete");
        assert_eq!(error_code("the server sent a web page instead of the file `a.zip`"), "web_page");
        assert_eq!(error_code("something odd"), "other");
        assert_eq!(error_code("request failed: error following redirect for url (https://x.test/): too many redirects"), "redirect_loop");
        assert_eq!(error_code("server did not honor range request: expected 206, got 200 for bytes=5-9"), "range_not_honored");
        assert_eq!(error_code("state store error: database is locked"), "persistence");
        assert_eq!(error_code("unsupported URL scheme `ftp` (only http/https allowed)"), "invalid_request");
        assert_eq!(error_code("download cancelled (partial data discarded)"), "cancelled");
        assert!(explain("server did not honor range request: x").0.contains("starts over safely"));
        // Codes are safe to put anywhere: lowercase letters, digits and underscores only.
        for raw in ["HTTP status 404", "x", "", "HTTP status 99999999999"] {
            assert!(error_code(raw).bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{raw}");
        }
    }

    #[test]
    fn unknown_errors_are_passed_through_unchanged() {
        assert_eq!(explain("something odd"), ("something odd".to_string(), None));
        assert_eq!(explain(""), (String::new(), None));
        // Garbage never panics.
        for s in ["HTTP status ", "HTTP status 99999999999", "HTTP status x", "\u{0}\u{ffff}"] {
            let _ = explain(s);
        }
    }
}
