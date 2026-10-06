use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use downloader_core::{
    ClientConfig, DownloadConfig, DownloadControl, DownloadError, DownloadRequest, Downloader, ProgressHandle, SegmentStatus,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
struct Cfg {
    data: Arc<Vec<u8>>,
    /// Actually honor Range requests.
    ranges: bool,
    /// Send `Accept-Ranges: bytes` (can lie).
    claim_ranges: bool,
    /// Answer HEAD (else 405).
    head: bool,
    disposition: Option<String>,
}

fn data(n: usize) -> Arc<Vec<u8>> {
    let mut x: u64 = 0x9E3779B97F4A7C15;
    Arc::new(
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect(),
    )
}

fn cfg(n: usize) -> Cfg {
    Cfg { data: data(n), ranges: true, claim_ranges: true, head: true, disposition: None }
}

async fn serve(cfg: Cfg) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let cfg = cfg.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let method = text.split_whitespace().next().unwrap_or("").to_string();
                let range = text.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("range").then(|| v.trim().to_string())
                });
                let total = cfg.data.len();
                let mut h = String::new();
                let body: &[u8];
                let status;
                if method == "HEAD" && !cfg.head {
                    status = "405 Method Not Allowed";
                    body = &[];
                    h.push_str("Content-Length: 0\r\n");
                } else if let (true, Some(r), "GET") = (cfg.ranges, range.as_deref(), method.as_str()) {
                    if total == 0 {
                        status = "416 Range Not Satisfiable";
                        body = &[];
                        h.push_str("Content-Length: 0\r\n");
                    } else {
                        let spec = r.trim_start_matches("bytes=");
                        let (a, b) = spec.split_once('-').unwrap();
                        let a: usize = a.parse().unwrap();
                        let b: usize = b.parse::<usize>().unwrap_or(total - 1).min(total - 1);
                        status = "206 Partial Content";
                        body = &cfg.data[a..=b];
                        h.push_str(&format!("Content-Range: bytes {a}-{b}/{total}\r\nContent-Length: {}\r\n", b - a + 1));
                    }
                } else {
                    status = "200 OK";
                    body = &cfg.data;
                    h.push_str(&format!("Content-Length: {total}\r\n"));
                }
                if cfg.claim_ranges {
                    h.push_str("Accept-Ranges: bytes\r\n");
                }
                if let Some(d) = &cfg.disposition {
                    h.push_str(&format!("Content-Disposition: {d}\r\n"));
                }
                let head =
                    format!("HTTP/1.1 {status}\r\nETag: \"v1\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n{h}\r\n");
                let _ = s.write_all(head.as_bytes()).await;
                if method != "HEAD" {
                    let _ = s.write_all(body).await;
                }
                let _ = s.shutdown().await;
            });
        }
    });
    addr
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn dl() -> Downloader {
    let cfg = DownloadConfig { min_segment_size: 64 * 1024, ..Default::default() };
    Downloader::new(&ClientConfig::default(), cfg).unwrap()
}

fn req(addr: SocketAddr, dir: &std::path::Path) -> DownloadRequest {
    DownloadRequest::new(format!("http://{addr}/file.bin"), dir)
}

#[tokio::test]
async fn parallel_download_is_byte_exact() {
    let c = cfg(12 * 1024 * 1024);
    let addr = serve(c.clone()).await;
    let dir = tmpdir("par");
    let progress = ProgressHandle::new();
    let o = dl().download(&req(addr, &dir), DownloadControl::new(), progress.clone()).await.unwrap();
    assert!(o.parallel && o.connections > 1, "{o:?}");
    assert_eq!(std::fs::read(&o.path).unwrap(), *c.data);
    assert!(!dir.join("file.bin.part").exists());
    let s = progress.snapshot();
    assert_eq!(s.downloaded, s.total.unwrap());
    assert!(s.segments.iter().all(|g| g.status == SegmentStatus::Completed && g.is_complete()));
}

#[tokio::test]
async fn no_range_support_falls_back_to_sequential() {
    let mut c = cfg(1_000_000);
    c.ranges = false;
    c.claim_ranges = false;
    let addr = serve(c.clone()).await;
    let dir = tmpdir("seq");
    let o = dl().download(&req(addr, &dir), DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(!o.parallel);
    assert_eq!(std::fs::read(&o.path).unwrap(), *c.data);
}

#[tokio::test]
async fn lying_accept_ranges_is_detected() {
    let mut c = cfg(800_000);
    c.ranges = false; // claims bytes, answers 200
    c.claim_ranges = true;
    let addr = serve(c.clone()).await;
    let info = dl().probe(&format!("http://{addr}/x")).await.unwrap();
    assert!(info.accept_ranges_header && !info.range_verified);
    let dir = tmpdir("lie");
    let o = dl().download(&req(addr, &dir), DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert!(!o.parallel);
    assert_eq!(std::fs::read(&o.path).unwrap(), *c.data);
}

#[tokio::test]
async fn head_rejected_still_discovers_via_range_probe() {
    let mut c = cfg(2_000_000);
    c.head = false;
    let addr = serve(c.clone()).await;
    let info = dl().probe(&format!("http://{addr}/x")).await.unwrap();
    assert_eq!(info.content_length, Some(2_000_000));
    assert!(info.range_verified);
    assert_eq!(info.etag.as_deref(), Some("\"v1\""));
}

#[tokio::test]
async fn empty_file() {
    let c = cfg(0);
    let addr = serve(c).await;
    let dir = tmpdir("empty");
    let o = dl().download(&req(addr, &dir), DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert_eq!(o.size, 0);
    assert_eq!(std::fs::read(&o.path).unwrap().len(), 0);
}

#[tokio::test]
async fn hostile_content_disposition_cannot_escape_dir() {
    let mut c = cfg(1000);
    c.disposition = Some("attachment; filename=\"../../evil.txt\"".into());
    let addr = serve(c.clone()).await;
    let dir = tmpdir("evil");
    let o = dl().download(&req(addr, &dir), DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert_eq!(o.path, dir.join("evil.txt"));
    assert!(!dir.parent().unwrap().join("evil.txt").exists());
}

#[tokio::test]
async fn never_overwrites_takes_the_next_free_name_and_rejects_bad_scheme() {
    let addr = serve(cfg(1000)).await;
    let dir = tmpdir("exists");
    let r = req(addr, &dir);
    let first = dl().download(&r, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    let before = std::fs::read(&first.path).unwrap();
    let second = dl().download(&r, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert_ne!(first.path, second.path, "an existing file is never overwritten");
    let (a, b) =
        (first.path.file_name().unwrap().to_string_lossy().into_owned(), second.path.file_name().unwrap().to_string_lossy().into_owned());
    assert_eq!(b, downloader_core::engine::numbered_name(&a, 1), "{a} -> {b}");
    assert_eq!(std::fs::read(&first.path).unwrap(), before);
    // A partial file of another download also takes the name.
    std::fs::write(downloader_core::part_path(&dir.join(downloader_core::engine::numbered_name(&a, 2))), b"someone else's").unwrap();
    let third = dl().download(&r, DownloadControl::new(), ProgressHandle::new()).await.unwrap();
    assert_eq!(third.path.file_name().unwrap().to_string_lossy(), downloader_core::engine::numbered_name(&a, 3));

    let mut bad = r.clone();
    bad.url = "file:///etc/passwd".into();
    let e = dl().download(&bad, DownloadControl::new(), ProgressHandle::new()).await.unwrap_err();
    assert!(matches!(e, DownloadError::Http(_)), "{e}");
}

#[tokio::test]
async fn cancellation_stops_download_and_keeps_part_file() {
    let addr = serve(cfg(8 * 1024 * 1024)).await;
    let dir = tmpdir("cancel");
    let token = DownloadControl::new();
    token.cancel();
    let e = dl().download(&req(addr, &dir), token, ProgressHandle::new()).await.unwrap_err();
    assert!(matches!(e, DownloadError::Cancelled), "{e}");
    assert!(!dir.join("file.bin").exists());
}
