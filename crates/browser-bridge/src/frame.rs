//! Length-prefixed frames: a 4-byte length (native byte order, as browsers use for native
//! messaging) followed by that many bytes of UTF-8 JSON.
//!
//! The length is checked against a limit **before** the body is read, so an oversized or hostile
//! frame can never make us allocate or read it. After a `TooLarge` error the stream is out of sync
//! and must be closed.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message accepted from a browser (Chrome itself allows 1 MiB; we need a few KiB).
pub const MAX_BROWSER_MESSAGE: usize = 64 * 1024;
/// Largest message on the host <-> app link.
pub const MAX_IPC_MESSAGE: usize = 128 * 1024;
/// Largest reply we send back to a browser.
pub const MAX_REPLY: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("message of {len} bytes exceeds the {max} byte limit")]
    TooLarge { len: usize, max: usize },
    #[error("connection closed in the middle of a message")]
    Truncated,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Read one frame. `Ok(None)` means the peer closed the stream cleanly between messages.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < header.len() {
        let n = r.read(&mut header[got..]).await?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(FrameError::Truncated) };
        }
        got += n;
    }
    let len = u32::from_ne_bytes(header) as usize;
    if len > max {
        return Err(FrameError::TooLarge { len, max });
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })?;
    Ok(Some(body))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8], max: usize) -> Result<(), FrameError> {
    if body.len() > max {
        return Err(FrameError::TooLarge { len: body.len(), max });
    }
    w.write_all(&(body.len() as u32).to_ne_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_ne_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[tokio::test]
    async fn round_trip_and_clean_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"a\":1}", 100).await.unwrap();
        write_frame(&mut buf, b"", 100).await.unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r, 100).await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read_frame(&mut r, 100).await.unwrap().unwrap(), b"");
        assert!(read_frame(&mut r, 100).await.unwrap().is_none(), "EOF between frames is a clean close");
    }

    #[tokio::test]
    async fn oversized_frames_are_refused_before_the_body_is_read() {
        // Header claims 4 GiB; only a few bytes follow. We must fail on the header alone.
        let mut evil = u32::MAX.to_ne_bytes().to_vec();
        evil.extend_from_slice(b"xxxx");
        let mut r = &evil[..];
        assert!(matches!(read_frame(&mut r, MAX_BROWSER_MESSAGE).await, Err(FrameError::TooLarge { .. })));

        let big = framed(&vec![b'a'; MAX_BROWSER_MESSAGE + 1]);
        assert!(matches!(read_frame(&mut &big[..], MAX_BROWSER_MESSAGE).await, Err(FrameError::TooLarge { .. })));
        let exact = framed(&vec![b'a'; MAX_BROWSER_MESSAGE]);
        assert!(read_frame(&mut &exact[..], MAX_BROWSER_MESSAGE).await.unwrap().is_some(), "the limit itself is allowed");
        assert!(matches!(write_frame(&mut Vec::new(), &[0u8; 11], 10).await, Err(FrameError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn truncated_streams_are_errors_not_hangs_or_partial_messages() {
        let mut partial_header = &[1u8, 0][..];
        assert!(matches!(read_frame(&mut partial_header, 100).await, Err(FrameError::Truncated)));
        let mut cut = framed(b"hello world");
        cut.truncate(8);
        assert!(matches!(read_frame(&mut &cut[..], 100).await, Err(FrameError::Truncated)));
    }
}
