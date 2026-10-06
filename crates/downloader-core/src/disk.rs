//! Disk engine: preallocation and bounded-memory, offset-addressed writes.
//!
//! ```text
//! network -> [reader: coalesce into <= buffer_size] -> bounded channel -> [writer: seek + write] -> disk
//! ```
//! Memory per worker is bounded by `buffer_size * (channel_capacity + 2)`,
//! independent of file or segment size.

use std::io::SeekFrom;
use std::path::Path;

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use http_client::HttpError;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{DownloadError, Result};
use crate::limiter::Throttle;

/// Create (truncating) the `.part` file and reserve its full length.
pub async fn create_preallocated(path: &Path, size: u64) -> Result<()> {
    let f = File::from_std(path_safety::create_no_follow(path)?);
    failpoint::hit_io("disk.preallocate")?;
    f.set_len(size).await?;
    f.sync_all().await?;
    failpoint::hit("disk.after_preallocate");
    Ok(())
}

pub struct PumpParams {
    /// Absolute file offset the first byte is written to.
    pub offset: u64,
    /// Stop after this many bytes (extra bytes from a misbehaving server are discarded).
    pub limit: Option<u64>,
    pub buffer_size: usize,
    pub channel_capacity: usize,
    /// Bandwidth limits applied to every chunk read from the network.
    pub throttle: Option<Throttle>,
}

/// Stream `stream` into `path` starting at `params.offset`. `on_written` is called after each
/// successful write with the byte count. Pending buffers are flushed even when cancelled.
/// Returns bytes written.
pub async fn pump<S, F>(stream: S, path: &Path, params: PumpParams, cancel: &CancellationToken, mut on_written: F) -> Result<u64>
where
    S: Stream<Item = std::result::Result<Bytes, HttpError>> + Send,
    F: FnMut(u64) + Send,
{
    let mut file = File::from_std(path_safety::open_write_no_follow(path)?);
    file.seek(SeekFrom::Start(params.offset)).await?;

    let (tx, mut rx) = mpsc::channel::<Bytes>(params.channel_capacity.max(1));
    let buffer_size = params.buffer_size.max(4096);
    let limit = params.limit;
    let throttle = params.throttle;

    let reader = async move {
        tokio::pin!(stream);
        let mut buf = BytesMut::with_capacity(buffer_size);
        let mut received: u64 = 0;
        let outcome: Result<()> = loop {
            let item = tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(DownloadError::Cancelled),
                item = stream.next() => item,
            };
            let Some(item) = item else { break Ok(()) };
            let mut chunk = match item {
                Ok(c) => c,
                Err(e) => break Err(e.into()),
            };
            if let Some(l) = limit {
                let room = l - received;
                if chunk.len() as u64 > room {
                    chunk.truncate(room as usize);
                }
            }
            received += chunk.len() as u64;
            let chunk_len = chunk.len();
            buf.extend_from_slice(&chunk);
            if buf.len() >= buffer_size {
                if tx.send(buf.split().freeze()).await.is_err() {
                    break Err(DownloadError::Internal("writer stopped".into()));
                }
                buf.reserve(buffer_size);
            }
            // Pacing here (before reading more) leaves the socket unread, so TCP flow control
            // slows the sender: the limit is enforced without buffering or dropping anything.
            // Every chunk is paid for, the last one of a range too: a fast server can deliver a whole
            // range in one chunk, which would otherwise never be counted against any limit.
            if let Some(t) = &throttle {
                if let Err(e) = t.take(chunk_len, cancel).await {
                    break Err(e);
                }
            }
            if limit == Some(received) {
                break Ok(());
            }
        };
        if !buf.is_empty() {
            let _ = tx.send(buf.split().freeze()).await; // flush pending data even on error/cancel
        }
        outcome.map(|()| received)
    };

    // `move`: when the writer stops on a disk error it drops the receiving end, so the reader (blocked on a
    // full channel) fails at once instead of waiting for a writer that is gone.
    let writer = async move {
        let mut written = 0u64;
        while let Some(b) = rx.recv().await {
            failpoint::hit_io("disk.write")?;
            file.write_all(&b).await?;
            failpoint::hit("disk.after_write");
            written += b.len() as u64;
            on_written(b.len() as u64);
        }
        file.flush().await?;
        Ok::<u64, DownloadError>(written)
    };

    let (read, wrote) = tokio::join!(reader, writer);
    let wrote = wrote?; // a disk error is the root cause; report it before reader-side symptoms
    read?;
    Ok(wrote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limiter::TokenBucket;
    use std::time::{Duration, Instant};

    /// A fast server can send a whole range in one chunk. That chunk must be charged to the speed limit
    /// like any other (it once ended the range before paying, so no limit applied at all).
    #[tokio::test]
    async fn a_range_delivered_in_one_chunk_still_pays_the_speed_limit() {
        let dir = std::env::temp_dir().join(format!("velox-pump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one-chunk.part");
        create_preallocated(&path, 400_000).await.unwrap();
        let limit = TokenBucket::new(Some(200_000));
        let throttle = Throttle::new(vec![limit.clone()]);
        let cancel = CancellationToken::new();
        let mut written = 0;
        let t = Instant::now();
        for i in 0..2u64 {
            let body = Bytes::from(vec![7u8; 200_000]);
            let stream = futures_util::stream::iter(vec![Ok::<Bytes, HttpError>(body)]);
            let params = PumpParams {
                offset: i * 200_000,
                limit: Some(200_000),
                buffer_size: 65_536,
                channel_capacity: 2,
                throttle: Some(throttle.clone()),
            };
            written += pump(stream, &path, params, &cancel, |_| {}).await.unwrap();
        }
        assert_eq!(written, 400_000);
        // 400 kB at 200 kB/s, less the first quarter second of burst: well over a second.
        assert!(t.elapsed() >= Duration::from_millis(1200), "the limit was not applied: {:?}", t.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
