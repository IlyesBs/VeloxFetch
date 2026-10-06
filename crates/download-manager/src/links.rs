//! Links that point at a page about a file rather than at the file itself.
//!
//! Google Drive's share links (`drive.google.com/file/d/<id>/view`, `open?id=`, `uc?id=`) answer
//! with a web page, or with a "can't scan for viruses" page for big files. Downloaded as they are,
//! they give an HTML file named `view` or `uc`. The direct address of the same file is
//! `drive.usercontent.google.com/download?id=<id>&export=download&confirm=t`.

use url::Url;

/// The direct download address for a known share link, or `None` to keep the link as it is.
pub fn direct(url: &Url) -> Option<Url> {
    google_drive(url)
}

/// Is this a Google Drive download address (after [`direct`])?
pub fn is_google_drive_file(url: &Url) -> bool {
    url.host_str() == Some("drive.usercontent.google.com")
}

fn google_drive(url: &Url) -> Option<Url> {
    if !matches!(url.host_str(), Some("drive.google.com" | "docs.google.com")) {
        return None;
    }
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    // `/u/0/uc` is `/uc` for the first signed-in account.
    let path = match segments.as_slice() {
        ["u", n, rest @ ..] if n.bytes().all(|b| b.is_ascii_digit()) => rest,
        all => all,
    };
    let id = match path {
        ["file", "d", id, ..] => Some(id.to_string()),
        ["open"] | ["uc"] => query("id"),
        _ => None,
    }
    .filter(|id| is_drive_id(id))?;
    let mut out = Url::parse("https://drive.usercontent.google.com/download").ok()?;
    {
        let mut q = out.query_pairs_mut();
        q.append_pair("id", &id).append_pair("export", "download").append_pair("confirm", "t");
        // Older shared files need their resource key too.
        if let Some(key) = query("resourcekey").filter(|k| is_drive_id(k)) {
            q.append_pair("resourcekey", &key);
        }
    }
    Some(out)
}

fn is_drive_id(s: &str) -> bool {
    (10..=200).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz012345";

    fn direct_of(s: &str) -> Option<String> {
        direct(&Url::parse(s).unwrap()).map(|u| u.to_string())
    }

    #[test]
    fn drive_share_links_become_direct_downloads() {
        let want = format!("https://drive.usercontent.google.com/download?id={ID}&export=download&confirm=t");
        for link in [
            format!("https://drive.google.com/file/d/{ID}/view?usp=sharing"),
            format!("https://drive.google.com/file/d/{ID}/edit"),
            format!("https://drive.google.com/file/d/{ID}"),
            format!("https://drive.google.com/open?id={ID}"),
            format!("https://drive.google.com/uc?id={ID}&export=download"),
            format!("https://drive.google.com/u/0/uc?id={ID}&export=download"),
            format!("https://drive.google.com/u/1/file/d/{ID}/view"),
            format!("https://docs.google.com/uc?export=download&id={ID}"),
        ] {
            assert_eq!(direct_of(&link).as_deref(), Some(want.as_str()), "{link}");
        }
        assert_eq!(
            direct_of(&format!("https://drive.google.com/file/d/{ID}/view?resourcekey=0-AbCdEfGhIjKl")).unwrap(),
            format!("{want}&resourcekey=0-AbCdEfGhIjKl")
        );
    }

    #[test]
    fn other_links_are_kept() {
        for link in [
            "https://drive.google.com/drive/folders/1AbCdEfGhIjKlMnOp".to_string(),
            "https://drive.google.com/file/d/short/view".to_string(),
            "https://drive.google.com/file/d/bad%2Fid%2Fvalue12/view".to_string(),
            format!("https://drive.usercontent.google.com/download?id={ID}&export=download&uuid=x&at=y"),
            format!("https://docs.google.com/document/d/{ID}/edit"),
            format!("https://evil.test/file/d/{ID}/view"),
            format!("https://drive.google.com.evil.test/file/d/{ID}/view"),
            "https://example.com/uc?id=1AbCdEfGhIjKlMnOp".to_string(),
        ] {
            assert_eq!(direct_of(&link), None, "{link}");
        }
    }

    #[test]
    fn drive_file_addresses_are_recognised() {
        assert!(is_google_drive_file(&Url::parse("https://drive.usercontent.google.com/download?id=x").unwrap()));
        assert!(!is_google_drive_file(&Url::parse("https://drive.google.com/file/d/x/view").unwrap()));
    }
}
