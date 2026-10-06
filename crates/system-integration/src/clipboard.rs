//! Reading the clipboard's *text*, the same way on Windows, macOS and Linux.
//!
//! Only text is ever requested (never images or files), and the caller decides what to do with it.
//! Nothing here stores or logs what it reads.

use std::path::PathBuf;

/// Why the clipboard could not be read (as opposed to holding something that is not text).
#[derive(Debug, thiserror::Error)]
#[error("cannot read the clipboard: {0}")]
pub struct ClipboardError(pub String);

/// Where copied text comes from. A trait so tests can supply text without touching the real clipboard.
pub trait ClipboardSource: Send {
    /// The current text, `Ok(None)` when the clipboard is empty or holds something else.
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;
}

/// The operating system's clipboard.
#[derive(Default)]
pub struct SystemClipboard {
    inner: Option<arboard::Clipboard>,
}

impl ClipboardSource for SystemClipboard {
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        if self.inner.is_none() {
            self.inner = Some(arboard::Clipboard::new().map_err(|e| ClipboardError(e.to_string()))?);
        }
        let cb = self.inner.as_mut().expect("just created");
        match cb.get_text() {
            Ok(t) => Ok(Some(t)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => {
                // Start from a fresh handle next time (the clipboard owner may have gone away).
                self.inner = None;
                Err(ClipboardError(e.to_string()))
            }
        }
    }
}

/// Text from a file, for tests that must not touch the user's real clipboard. A missing file is an
/// empty clipboard.
pub struct FileClipboard(pub PathBuf);

impl ClipboardSource for FileClipboard {
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        match std::fs::read_to_string(&self.0) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ClipboardError(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_source_reads_text_and_treats_a_missing_file_as_empty() {
        let p = std::env::temp_dir().join(format!("velox-clip-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut c = FileClipboard(p.clone());
        assert_eq!(c.read_text().unwrap(), None);
        std::fs::write(&p, "https://a.test/x.zip").unwrap();
        assert_eq!(c.read_text().unwrap().as_deref(), Some("https://a.test/x.zip"));
        std::fs::remove_file(&p).unwrap();
    }

    /// Reads (never writes) the real clipboard: proves the OS binding works on this machine.
    /// `cargo test -p system-integration -- --ignored real_clipboard_read`
    #[test]
    #[ignore]
    fn real_clipboard_read_works() {
        let mut sys = SystemClipboard::default();
        sys.read_text().expect("the clipboard can be read (text or nothing)");
    }

    /// Talks to the real clipboard of whoever runs it, so it is opt-in:
    /// `cargo test -p system-integration -- --ignored real_clipboard` (it puts the previous text back).
    #[test]
    #[ignore]
    fn real_clipboard_round_trip() {
        let mut real = arboard::Clipboard::new().expect("a clipboard");
        let before = real.get_text().ok();
        real.set_text("https://example.test/velox-clipboard-check.zip").unwrap();
        let mut sys = SystemClipboard::default();
        assert_eq!(sys.read_text().unwrap().as_deref(), Some("https://example.test/velox-clipboard-check.zip"));
        match before {
            Some(t) => real.set_text(t).unwrap(),
            None => real.clear().unwrap(),
        }
    }
}
