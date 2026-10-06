//! Filename selection and sanitization. Server-supplied names are hostile input:
//! they must never be able to escape the destination directory or hit reserved names.

use http_client::{filename_from_content_disposition, percent_decode, ResourceInfo};
use url::Url;

/// Reduce an untrusted name to a single safe path component (see the `path-safety` crate).
pub fn sanitize_filename(raw: &str) -> String {
    path_safety::sanitize_filename(raw)
}

/// Pick a filename: Content-Disposition, else last URL path segment, else `download`.
pub fn choose_filename(info: &ResourceInfo, url: &Url) -> String {
    if let Some(n) = info.content_disposition.as_deref().and_then(filename_from_content_disposition) {
        return sanitize_filename(&n);
    }
    let from_url = Url::parse(&info.final_url)
        .ok()
        .as_ref()
        .unwrap_or(url)
        .path_segments()
        .and_then(|mut s| s.next_back().map(str::to_string))
        .filter(|s| !s.is_empty())
        .map(|s| percent_decode(&s).unwrap_or(s));
    sanitize_filename(&from_url.unwrap_or_default())
}

/// Did the server answer with a web page where a file was expected? True when the answer is HTML,
/// the server did not name it as an attachment, and the name we expect is not a web page (`a.zip`,
/// from the browser, the user or the link). A login or "file not available" page saved as `a.zip`
/// is the wrong file, so the download fails instead.
pub fn web_page_instead_of_file(info: &ResourceInfo, name: &str) -> bool {
    let html = info.content_type.as_deref().is_some_and(|t| {
        let t = t.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        t == "text/html" || t == "application/xhtml+xml"
    });
    if !html || info.content_disposition.as_deref().is_some_and(|d| filename_from_content_disposition(d).is_some()) {
        return false;
    }
    let Some((_, ext)) = name.rsplit_once('.') else { return false };
    let ext = ext.to_ascii_lowercase();
    !ext.is_empty()
        && !matches!(ext.as_str(), "html" | "htm" | "xhtml" | "shtml" | "mht" | "mhtml" | "php" | "asp" | "aspx" | "jsp" | "cgi")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(content_type: Option<&str>, disposition: Option<&str>) -> ResourceInfo {
        ResourceInfo {
            content_type: content_type.map(str::to_string),
            content_disposition: disposition.map(str::to_string),
            final_url: "https://x.test/a".into(),
            location: None,
            content_length: None,
            accept_ranges_header: false,
            range_verified: false,
            etag: None,
            last_modified: None,
            server: None,
            status: 200,
        }
    }

    #[test]
    fn a_web_page_is_not_the_expected_file() {
        // A sign-in page where `report.pdf` was expected.
        assert!(web_page_instead_of_file(&info(Some("text/html; charset=utf-8"), None), "report.pdf"));
        assert!(web_page_instead_of_file(&info(Some("application/xhtml+xml"), None), "a.ZIP"));
        // The server names it as an attachment: trust it, even with a wrong type.
        assert!(!web_page_instead_of_file(&info(Some("text/html"), Some("attachment; filename=\"a.zip\"")), "a.zip"));
        // A real web page, or nothing to compare with.
        assert!(!web_page_instead_of_file(&info(Some("text/html"), None), "index.html"));
        assert!(!web_page_instead_of_file(&info(Some("text/html"), None), "download"));
        assert!(!web_page_instead_of_file(&info(Some("application/zip"), None), "a.zip"));
        assert!(!web_page_instead_of_file(&info(None, None), "a.zip"));
    }

    #[test]
    fn traversal_is_neutralized() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\win\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename(""), "download");
    }

    #[test]
    fn reserved_and_illegal() {
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("a<b>c:d.txt"), "a_b_c_d.txt");
        assert_eq!(sanitize_filename("we\u{0}ird\n.bin"), "weird.bin");
    }
}
