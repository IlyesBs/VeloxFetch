//! Live preview over a Tauri v2 asynchronous custom protocol:
//!
//! ```text
//! <video src="http://velox-preview.localhost/12">   (Windows; velox-preview://localhost/12 on macOS / Linux)
//!     -> handler (this file): main window only, id only, GET / HEAD only
//!     -> DownloadManager::read_preview(12, Range)   (only written bytes of an audio / video download)
//!     -> 206 Partial Content + Content-Range, or an error status
//! ```
//! The answer is produced on the async runtime, never on the web view's thread, and every problem is an
//! HTTP status: a broken preview never touches the download.
//!
//! Linux: WebKitGTK's media player only reads http(s), file, blob and data addresses (`isProtocolAllowed` refuses
//! any custom scheme: "FormatError", nothing plays). There the same answers come from a private address instead,
//! [`loopback`]: `http://127.0.0.1:<port>/<secret>/12`, bound to this computer only, the secret (32 random bytes)
//! in every path, the `Host` checked (no DNS rebinding), GET / HEAD only, the same ids and the same
//! `read_preview` (only written bytes of audio / video). The page learns the address with `get_preview_base`. The port
//! is taken before the window is built and that exact origin is added to the CSP's `media-src` (no wildcard): `<video>`
//! / `<audio>` may use it, scripts still cannot (`connect-src` unchanged).

use std::borrow::Cow;

use tauri::http::{header, Method, Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::AppState;

pub const SCHEME: &str = "velox-preview";
/// The only window allowed to use it (the details live in the main window).
const WINDOW: &str = "main";

fn plain(status: StatusCode) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Borrowed(&[][..]))
        .expect("a static response")
}

/// The download id from `/12` (nothing else is accepted).
pub fn id_from_path(path: &str) -> Option<i64> {
    let s = path.strip_prefix('/')?;
    (!s.is_empty() && s.len() <= 15 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// What the player asks for: a download (`/12`) or a video job (`/m3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Download(i64),
    Media(u64),
}

pub fn target_from_path(path: &str) -> Option<Target> {
    if let Some(rest) = path.strip_prefix("/m") {
        return id_from_path(&format!("/{rest}")).and_then(|n| u64::try_from(n).ok()).map(Target::Media);
    }
    id_from_path(path).map(Target::Download)
}

/// The handler given to `register_asynchronous_uri_scheme_protocol`.
pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    if ctx.webview_label() != WINDOW {
        return responder.respond(plain(StatusCode::FORBIDDEN));
    }
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        return responder.respond(plain(StatusCode::METHOD_NOT_ALLOWED));
    }
    let Some(target) = target_from_path(request.uri().path()) else {
        return responder.respond(plain(StatusCode::NOT_FOUND));
    };
    let range = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return responder.respond(plain(StatusCode::SERVICE_UNAVAILABLE));
        };
        responder.respond(answer(&state.manager, target, range.as_deref(), head).await);
    });
}

/// The answer to one preview request (both ways in: the custom protocol and the Linux private address).
async fn answer(
    manager: &download_manager::DownloadManager,
    target: Target,
    range: Option<&str>,
    head: bool,
) -> Response<Cow<'static, [u8]>> {
    {
        let r = match target {
            Target::Download(id) => manager.read_preview(id, range).await,
            Target::Media(id) => manager.read_media_preview(id, range).await,
        };
        let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut b = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, r.content_type)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'")
            .header(header::CONTENT_LENGTH, r.body.len());
        if let Some(cr) = r.content_range {
            b = b.header(header::CONTENT_RANGE, cr);
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            b = b.header(header::RETRY_AFTER, "1");
        }
        let body = if head { Cow::Borrowed(&[][..]) } else { Cow::Owned(r.body) };
        b.body(body).unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR))
    }
}

/// Linux: the private address the media player can read (see the top of this file).
#[cfg(target_os = "linux")]
pub mod loopback {
    use std::io::Read;
    use std::sync::OnceLock;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// The longest request head read (a player's request is a few hundred bytes).
    const MAX_HEAD: usize = 8 * 1024;
    /// Rate limits: connections open at once, and new requests per second (a player makes a few).
    const MAX_OPEN: usize = 16;
    const MAX_PER_SECOND: u32 = 40;
    /// The only page that may send an `Origin` (a media element sends none).
    const APP_ORIGIN: &str = "tauri://localhost";

    static BASE: OnceLock<String> = OnceLock::new();

    /// `http://127.0.0.1:<port>/<secret>/` once started; `None` if it could not start (the page then keeps the
    /// custom protocol: no preview, nothing else affected).
    pub fn base() -> Option<String> {
        BASE.get().cloned()
    }

    fn secret() -> Option<String> {
        let mut b = [0u8; 32];
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).ok()?;
        Some(b.iter().map(|x| format!("{x:02x}")).collect())
    }

    /// Taken before the app is built (its origin goes into the CSP), served by [`start`].
    struct Reserved {
        listener: std::net::TcpListener,
        secret: String,
        port: u16,
    }

    static RESERVED: std::sync::Mutex<Option<Reserved>> = std::sync::Mutex::new(None);

    /// Take a free port on this computer only and return its origin (`http://127.0.0.1:<port>`, for the CSP).
    /// `None` (no random source, no port): previews keep the custom protocol.
    pub fn reserve() -> Option<String> {
        let secret = secret()?;
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| tracing::warn!(error = %e, "preview: no private address")).ok()?;
        listener.set_nonblocking(true).ok()?;
        let port = listener.local_addr().ok()?.port();
        *RESERVED.lock().ok()? = Some(Reserved { listener, secret, port });
        Some(format!("http://127.0.0.1:{port}"))
    }

    /// `media-src` gets exactly this origin (the production and the development policy).
    pub fn allow_in_csp(config: &mut tauri::Config, origin: &str) {
        use tauri::utils::config::Csp;
        for csp in [&mut config.app.security.csp, &mut config.app.security.dev_csp].into_iter().flatten() {
            match csp {
                Csp::DirectiveMap(map) => {
                    if let Some(src) = map.get_mut("media-src") {
                        src.push(origin);
                    }
                }
                Csp::Policy(p) => {
                    *p = p.replace("media-src ", &format!("media-src {origin} "));
                }
            }
        }
    }

    /// Serve (for the life of the app).
    pub async fn start(manager: download_manager::DownloadManager) {
        let Some(Reserved { listener, secret, port }) = RESERVED.lock().ok().and_then(|mut r| r.take()) else { return };
        let listener = match TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "preview: the private address could not start");
                return;
            }
        };
        let _ = BASE.set(format!("http://127.0.0.1:{port}/{secret}/"));
        let host = format!("127.0.0.1:{port}");
        let open = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_OPEN));
        let mut rate = Rate::default();
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { continue };
            let permit = open.clone().try_acquire_owned();
            if !rate.allow(std::time::Instant::now()) || permit.is_err() {
                let _ = sock.write_all(&to_bytes(&plain(StatusCode::TOO_MANY_REQUESTS))).await;
                continue;
            }
            let (manager, secret, host) = (manager.clone(), secret.clone(), host.clone());
            tauri::async_runtime::spawn(async move {
                let _permit = permit;
                let _ = serve(sock, &manager, &secret, &host).await;
            });
        }
    }

    /// New requests in the current second.
    #[derive(Default)]
    pub struct Rate {
        second: Option<std::time::Instant>,
        count: u32,
    }

    impl Rate {
        pub fn allow(&mut self, now: std::time::Instant) -> bool {
            match self.second {
                Some(s) if now.duration_since(s) < std::time::Duration::from_secs(1) => {
                    self.count += 1;
                    self.count <= MAX_PER_SECOND
                }
                _ => {
                    self.second = Some(now);
                    self.count = 1;
                    true
                }
            }
        }
    }

    /// What one request asks for, or the status that refuses it.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Asked {
        pub target: Target,
        pub range: Option<String>,
        pub head: bool,
    }

    pub fn parse(request: &str, secret: &str, host: &str) -> Result<Asked, StatusCode> {
        let mut lines = request.split("\r\n");
        let mut first = lines.next().unwrap_or_default().split(' ');
        let (method, path) = (first.next().unwrap_or_default(), first.next().unwrap_or_default());
        let head = match method {
            "GET" => false,
            "HEAD" => true,
            _ => return Err(StatusCode::METHOD_NOT_ALLOWED),
        };
        let (mut host_ok, mut origin_ok, mut range) = (false, true, None);
        for l in lines {
            let Some((k, v)) = l.split_once(':') else { continue };
            match k.trim().to_ascii_lowercase().as_str() {
                "host" => host_ok = v.trim() == host,
                // A media element sends no Origin; a script on another page would (and could not read it anyway).
                "origin" => origin_ok = v.trim() == APP_ORIGIN,
                "range" => range = Some(v.trim().to_string()),
                _ => {}
            }
        }
        // Another name for this address (DNS rebinding), or another page: not this app's player.
        if !host_ok || !origin_ok {
            return Err(StatusCode::FORBIDDEN);
        }
        let rest = path.strip_prefix('/').and_then(|p| p.strip_prefix(secret)).ok_or(StatusCode::NOT_FOUND)?;
        let target = target_from_path(rest).ok_or(StatusCode::NOT_FOUND)?;
        Ok(Asked { target, range, head })
    }

    async fn serve(mut sock: TcpStream, manager: &download_manager::DownloadManager, secret: &str, host: &str) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            if buf.len() > MAX_HEAD {
                return Ok(());
            }
            let n = tokio::time::timeout(std::time::Duration::from_secs(10), sock.read(&mut chunk)).await.unwrap_or(Ok(0))?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let request = String::from_utf8_lossy(&buf).into_owned();
        match parse(&request, secret, host) {
            Ok(a) => stream(&mut sock, manager, a).await?,
            Err(status) => sock.write_all(&to_bytes(&plain(status))).await?,
        }
        sock.shutdown().await
    }

    /// No new bytes for this long while streaming: the connection is closed (the player asks again).
    const STALL: std::time::Duration = std::time::Duration::from_secs(120);
    /// A "not written yet" faster than this: nothing is downloading it any more.
    const QUICK: std::time::Duration = std::time::Duration::from_millis(500);

    /// `bytes a-b/total` -> (a, b, total); `bytes a-b/*` (a video job whose size is not known yet) -> total `None`.
    pub fn content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
        let (ab, total) = v.strip_prefix("bytes ")?.split_once('/')?;
        let (a, b) = ab.split_once('-')?;
        let total = if total == "*" { None } else { Some(total.parse().ok()?) };
        Some((a.parse().ok()?, b.parse().ok()?, total))
    }

    /// The last byte the player asked for (`bytes=s-e`; `bytes=s-` or no range: the end of the file).
    pub fn requested_end(range: Option<&str>, total: u64) -> u64 {
        let last = total.saturating_sub(1);
        range
            .and_then(|r| r.trim().strip_prefix("bytes="))
            .and_then(|r| r.split_once('-'))
            .filter(|(s, _)| !s.trim().is_empty())
            .and_then(|(_, e)| e.trim().parse::<u64>().ok())
            .map_or(last, |e| e.min(last))
    }

    /// Answer like an ordinary web server serving a file that is still being written: the full length the player
    /// asked for up front, then the bytes as the download writes them. (Answering with only what is written so far
    /// makes WebKit's player take that for the whole file: a 60 s song showed as 3.7 s and stopped there.) A video
    /// job whose size is not known yet is sent from the start as one answer without a length, until it is done.
    /// An error is answered as the custom protocol does.
    async fn stream(sock: &mut TcpStream, manager: &download_manager::DownloadManager, asked: Asked) -> std::io::Result<()> {
        let target = asked.target;
        let read = |range: String| async move {
            match target {
                Target::Download(id) => manager.read_preview(id, Some(&range)).await,
                Target::Media(id) => manager.read_media_preview(id, Some(&range)).await,
            }
        };
        let first = answer(manager, asked.target, asked.range.as_deref(), asked.head).await;
        let known = first.headers().get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(content_range);
        let Some((start, got_to, total)) = known.filter(|_| first.status() == StatusCode::PARTIAL_CONTENT) else {
            return sock.write_all(&to_bytes(&first)).await;
        };
        // Size not known yet: only from the start, as one answer that ends when the job does.
        if total.is_none() && start != 0 {
            return sock.write_all(&to_bytes(&first)).await;
        }
        let end = total.map_or(u64::MAX - 1, |t| requested_end(asked.range.as_deref(), t));
        let partial = asked.range.is_some() && total.is_some();
        let mut b = Response::builder().status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK });
        if total.is_some() {
            b = b.header(header::CONTENT_LENGTH, end - start + 1);
        }
        for (k, v) in first.headers() {
            if k != header::CONTENT_LENGTH && k != header::CONTENT_RANGE && (total.is_some() || k != header::ACCEPT_RANGES) {
                b = b.header(k, v);
            }
        }
        if let (true, Some(t)) = (partial, total) {
            b = b.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{t}"));
        }
        let head = b.body(Cow::Borrowed(&[][..])).unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR));
        let mut bytes = to_bytes(&head);
        if total.is_none() {
            // `to_bytes` wrote a zero length for the empty body: no length here, the answer ends with the connection.
            let text = String::from_utf8_lossy(&bytes).replace("content-length: 0\r\n", "");
            bytes = text.into_bytes();
        }
        if asked.head {
            return sock.write_all(&bytes).await;
        }
        let first_body = first.body();
        bytes.extend_from_slice(&first_body[..first_body.len().min((end - start + 1) as usize)]);
        sock.write_all(&bytes).await?;
        let mut at = got_to + 1;
        let mut last_progress = std::time::Instant::now();
        while at <= end {
            let asked_at = std::time::Instant::now();
            let ask = if total.is_some() { format!("bytes={at}-{end}") } else { format!("bytes={at}-") };
            let r = read(ask).await;
            match (r.status, r.content_range.as_deref().and_then(content_range)) {
                (206, Some((a, b, _))) if a == at && !r.body.is_empty() => {
                    sock.write_all(&r.body).await?;
                    at = b + 1;
                    last_progress = std::time::Instant::now();
                }
                // Not written yet: the reader waits while the download runs, so a quick "not there" means it stopped
                // (finished, paused, cancelled). Otherwise wait more, up to a limit.
                (503, _) if asked_at.elapsed() >= QUICK && last_progress.elapsed() < STALL => {}
                // Done, gone or stalled: the answer ends here (a player that needs more asks again).
                _ => break,
            }
        }
        Ok(())
    }

    /// An HTTP/1.1 answer as bytes (one answer per connection).
    pub fn to_bytes(r: &Response<Cow<'static, [u8]>>) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", r.status().as_u16(), r.status().canonical_reason().unwrap_or(""));
        for (k, v) in r.headers() {
            out.push_str(&format!("{}: {}\r\n", k.as_str(), v.to_str().unwrap_or_default()));
        }
        if !r.headers().contains_key(header::CONTENT_LENGTH) {
            out.push_str(&format!("content-length: {}\r\n", r.body().len()));
        }
        out.push_str("connection: close\r\n\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(r.body());
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn the_private_address_answers_only_its_secret_its_host_and_reads() {
        use loopback::{parse, to_bytes, Asked};
        let (s, h) = ("abc123", "127.0.0.1:4100");
        let req = |line: &str, host: &str| format!("{line}\r\nHost: {host}\r\nRange: bytes=0-99\r\n\r\n");
        assert_eq!(
            parse(&req("GET /abc123/12 HTTP/1.1", h), s, h),
            Ok(Asked { target: Target::Download(12), range: Some("bytes=0-99".into()), head: false })
        );
        assert_eq!(parse(&req("HEAD /abc123/m3 HTTP/1.1", h), s, h).map(|a| (a.target, a.head)), Ok((Target::Media(3), true)));
        assert_eq!(parse(&req("GET /wrong/12 HTTP/1.1", h), s, h), Err(StatusCode::NOT_FOUND), "no secret, nothing");
        assert_eq!(parse(&req("GET /12 HTTP/1.1", h), s, h), Err(StatusCode::NOT_FOUND));
        assert_eq!(parse(&req("GET /abc123/../velox.db HTTP/1.1", h), s, h), Err(StatusCode::NOT_FOUND), "ids only");
        assert_eq!(parse(&req("GET /abc123/12 HTTP/1.1", "evil.example:4100"), s, h), Err(StatusCode::FORBIDDEN), "DNS rebinding");
        assert_eq!(parse(&req("POST /abc123/12 HTTP/1.1", h), s, h), Err(StatusCode::METHOD_NOT_ALLOWED));
        let with_origin = |o: &str| format!("GET /abc123/12 HTTP/1.1\r\nHost: {h}\r\nOrigin: {o}\r\n\r\n");
        assert_eq!(parse(&with_origin("https://evil.example"), s, h), Err(StatusCode::FORBIDDEN), "another page");
        assert!(parse(&with_origin("tauri://localhost"), s, h).is_ok(), "the app's own page");
        // Rate limit: 40 new requests a second, then 429 until the next second.
        let mut rate = loopback::Rate::default();
        let t = std::time::Instant::now();
        assert!((0..40).all(|_| rate.allow(t)));
        assert!(!rate.allow(t + std::time::Duration::from_millis(500)));
        assert!(rate.allow(t + std::time::Duration::from_millis(1001)));
        // The CSP gets exactly this origin in media-src, nothing else changes (scripts still cannot connect to it).
        let json = std::fs::read_to_string(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")).unwrap();
        let mut config: tauri::Config = serde_json::from_str(&json).unwrap();
        let before = serde_json::to_value(&config.app.security).unwrap();
        loopback::allow_in_csp(&mut config, "http://127.0.0.1:4100");
        let after = serde_json::to_value(&config.app.security).unwrap();
        for key in ["csp", "devCsp"] {
            assert_eq!(after[key]["media-src"], format!("{} http://127.0.0.1:4100", before[key]["media-src"].as_str().unwrap()), "{key}");
            assert_eq!(after[key]["connect-src"], before[key]["connect-src"], "{key}");
        }
        assert_eq!(loopback::content_range("bytes 0-99/5000"), Some((0, 99, Some(5000))));
        assert_eq!(loopback::content_range("bytes 0-99/*"), Some((0, 99, None)), "a video job's size not known yet");
        assert_eq!(loopback::content_range("bytes 0-99/x"), None);
        assert_eq!(loopback::requested_end(Some("bytes=100-"), 5000), 4999);
        assert_eq!(loopback::requested_end(Some("bytes=100-199"), 5000), 199);
        assert_eq!(loopback::requested_end(Some("bytes=100-99999"), 5000), 4999);
        assert_eq!(loopback::requested_end(None, 5000), 4999);
        let bytes = to_bytes(&plain(StatusCode::NOT_FOUND));
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.starts_with("HTTP/1.1 404 Not Found\r\n") && text.contains("content-length: 0") && text.ends_with("\r\n\r\n"),
            "{text}"
        );
    }

    #[test]
    fn a_video_job_is_asked_for_by_its_id_only() {
        assert_eq!(target_from_path("/m3"), Some(Target::Media(3)));
        assert_eq!(target_from_path("/12"), Some(Target::Download(12)));
        for bad in ["/m", "/m-1", "/m3/x", "/mm3", "/m../x", "/m%33", "/x3"] {
            assert_eq!(target_from_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_download_id_is_accepted() {
        assert_eq!(id_from_path("/12"), Some(12));
        for bad in ["/", "", "12", "/12/x", "/-1", "/1e3", "/../velox.db", "/C:/x.mp4", "/1234567890123456", "/%31"] {
            assert_eq!(id_from_path(bad), None, "{bad}");
        }
    }
}
