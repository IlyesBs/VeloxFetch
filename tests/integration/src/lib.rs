//! Test support: a tiny range-capable HTTP server with fault injection.
//!
//! Behaviour is chosen by the request path, so one server can play every role a test needs:
//!
//! | path | behaviour |
//! |---|---|
//! | anything else | normal server: ranges, ETag, `Content-Length` |
//! | `/norange/..` | ignores `Range`, never says `Accept-Ranges`: always the whole body with `200` |
//! | `/redir/N/..` | `N` redirects (302) hops, then the file at `/..` |
//! | `/redirloop` | redirects to itself forever |
//! | `/redirext` | redirects to `State::redirect_to` (another server, another origin) |
//! | `/auth/..` | `401` + `WWW-Authenticate: Basic` unless `Authorization` is `Basic dXNlcjpwYXNz` (`user:pass`) |
//! | `/e429/..` | always `429` with `Retry-After: 0` |
//! | `/e500/..` | always `500` |
//! | `/reset/..` | accepts the request and closes the connection with no answer at all |
//! | `/lie/..` | announces one length in `Content-Length` and sends the body of another (`State::lie_extra`) |
//! | `/hang/..` | sends the headers, then never sends the body |
//! | `/huge/..` | claims to be one petabyte (`Content-Length` / `Content-Range`) |
//! | `/big-header/..` | answers with a huge (`State::junk_header_bytes`) header value |
//! | `/html/..` | the file, but announced as `Content-Type: text/html` (a sign-in page where a file was expected) |
//! | `/signed/<token>/..` | the file, or `403` once `<token>` is in `State::expired` (a signed link that expired) or to a client from `State::refuse_peer` |
//! | `/noetag/..` | the file, without an `ETag` (nothing proves its version) |
//! | `../dated/..` | (anywhere in the path) also sends a fixed `Last-Modified` |
//!
//! Server-wide knobs live in [`State`] (pace, failures, a slow answer, one request at a time...).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct State {
    pub data: Mutex<Arc<Vec<u8>>>,
    pub etag: Mutex<String>,
    /// Pause between 32 KiB body chunks. Applies *per connection*, so it models a server that
    /// caps each connection's speed (more connections = more total throughput).
    pub chunk_delay: Mutex<Duration>,
    /// Answer the next N data requests with `503` + `Retry-After: 0`.
    pub fail_next: AtomicUsize,
    /// Cut the connection halfway through the body for the next N data requests.
    pub truncate_next: AtomicUsize,
    /// Number of data (non-probe) GET requests seen.
    pub hits: AtomicUsize,
    /// Body bytes written to clients.
    pub bytes_served: AtomicU64,
    /// Serve several requests per TCP connection (HTTP/1.1 keep-alive).
    pub keep_alive: AtomicBool,
    /// TCP connections accepted.
    pub connections: AtomicUsize,
    /// Requests currently being served (peak is tracked in `peak_concurrent`).
    pub active: AtomicUsize,
    pub peak_concurrent: AtomicUsize,
    /// Answer `429` to data requests while this many are already being served (0 = no limit).
    pub max_concurrent: AtomicUsize,
    /// Number of `429` responses sent.
    pub throttled: AtomicUsize,
    /// Every request head received (lower-cased), so tests can check which headers the client sent.
    pub request_heads: Mutex<Vec<String>>,
    /// Where `/redirext` sends the client.
    pub redirect_to: Mutex<String>,
    /// For `/lie/`: bytes added to (positive) or removed from (negative) the real length in `Content-Length`.
    pub lie_extra: std::sync::atomic::AtomicI64,
    /// For `/big-header/`: size of the junk header.
    pub junk_header_bytes: AtomicUsize,
    /// Delay before the response head is sent (a server slow to answer).
    pub head_delay: Mutex<Duration>,
    /// Sent as `Content-Disposition` on every answer when set: the name a hostile server wants to choose.
    pub content_disposition: Mutex<Option<String>>,
    /// Fixed documents (playlists, segments...) served at exact paths, taking precedence over the file.
    pub routes: Mutex<std::collections::HashMap<String, Arc<Vec<u8>>>>,
    /// Delay before each fixed document is answered.
    pub route_delay: Mutex<Duration>,
    /// Tokens of `/signed/<token>/..` links that have expired: answered with `403`.
    pub expired: Mutex<std::collections::HashSet<String>>,
    /// Serve one data request at a time: the others wait for their answer meanwhile (a server with one slot per
    /// client, or simply a busy one).
    pub one_at_a_time: AtomicBool,
    turn: tokio::sync::Semaphore,
    /// For every answer the client stopped reading: the 32 KiB body chunks it had been sent before.
    pub cut_after_chunks: Mutex<Vec<usize>>,
    /// Body bytes written per client address (which network a request came through).
    pub bytes_by_peer: Mutex<std::collections::HashMap<std::net::IpAddr, u64>>,
    /// `/signed/..` links are refused (`403`) to a client from this address (a link signed for another address).
    pub refuse_peer: Mutex<Option<std::net::IpAddr>>,
}

pub struct Server {
    pub addr: SocketAddr,
    pub state: Arc<State>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Serve `body` at exactly `path`.
    pub fn put(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.state.routes.lock().unwrap().insert(path.to_string(), Arc::new(body.into()));
    }

    pub fn set_data(&self, d: Arc<Vec<u8>>) {
        *self.state.data.lock().unwrap() = d;
    }
}

/// Deterministic pseudo-random bytes.
pub fn random_data(n: usize, seed: u64) -> Arc<Vec<u8>> {
    let mut x = seed ^ 0x9E3779B97F4A7C15;
    Arc::new(
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect(),
    )
}

pub async fn serve(data: Arc<Vec<u8>>) -> Server {
    serve_on("127.0.0.1:0".parse().unwrap(), data).await
}

/// Test folders (`<temp>/velox-<suite>-<pid>-...`) left by test processes that have ended: removed once per process.
/// On Linux the temp folder is often in memory, and thousands of runs add up to gigabytes. Only folders whose
/// process is gone are touched (never this run's, never a running test's).
pub fn sweep_stale_test_folders() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let proc = std::path::Path::new("/proc");
        if !proc.is_dir() {
            return; // only where process liveness can be read cheaply (Linux)
        }
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix("velox-") else { continue };
            let Some(pid) = rest.split('-').find(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())) else { continue };
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && pid.parse::<u32>().is_ok_and(|p| p != std::process::id()) && !proc.join(pid).exists() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
}

/// [`serve`] on a chosen address (e.g. one a test just saw refused, to show a recovery).
pub async fn serve_on(addr: SocketAddr, data: Arc<Vec<u8>>) -> Server {
    sweep_stale_test_folders();
    let state = Arc::new(State {
        data: Mutex::new(data),
        etag: Mutex::new("\"v1\"".into()),
        chunk_delay: Mutex::new(Duration::ZERO),
        fail_next: AtomicUsize::new(0),
        truncate_next: AtomicUsize::new(0),
        hits: AtomicUsize::new(0),
        bytes_served: AtomicU64::new(0),
        keep_alive: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        peak_concurrent: AtomicUsize::new(0),
        max_concurrent: AtomicUsize::new(0),
        throttled: AtomicUsize::new(0),
        request_heads: Mutex::new(Vec::new()),
        redirect_to: Mutex::new(String::new()),
        lie_extra: std::sync::atomic::AtomicI64::new(0),
        junk_header_bytes: AtomicUsize::new(0),
        head_delay: Mutex::new(Duration::ZERO),
        content_disposition: Mutex::new(None),
        routes: Mutex::new(Default::default()),
        route_delay: Mutex::new(Duration::ZERO),
        expired: Mutex::new(Default::default()),
        one_at_a_time: AtomicBool::new(false),
        turn: tokio::sync::Semaphore::new(1),
        cut_after_chunks: Mutex::new(Vec::new()),
        bytes_by_peer: Mutex::new(Default::default()),
        refuse_peer: Mutex::new(None),
    });
    let l = TcpListener::bind(addr).await.unwrap();
    let addr = l.local_addr().unwrap();
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            st.connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(handle(s, st.clone()));
        }
    });
    Server { addr, state }
}

// `fetch_update` is named `try_update` on newer Rust; keep the old name so every toolchain builds.
#[allow(deprecated)]
fn take(counter: &AtomicUsize) -> bool {
    counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok()
}

fn count_peer(s: &tokio::net::TcpStream, st: &State, n: usize) {
    if let Ok(peer) = s.peer_addr() {
        *st.bytes_by_peer.lock().unwrap().entry(peer.ip()).or_default() += n as u64;
    }
}

async fn handle(mut s: tokio::net::TcpStream, st: Arc<State>) {
    while handle_one(&mut s, &st).await {}
}

/// Serve one request. Returns true when the connection may be reused.
async fn handle_one(s: &mut tokio::net::TcpStream, st: &Arc<State>) -> bool {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return false;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    st.request_heads.lock().unwrap().push(text.to_ascii_lowercase());
    let mut first = text.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("/").to_string();
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("range").then(|| v.trim().to_string())
    });
    let data = st.data.lock().unwrap().clone();
    let etag = st.etag.lock().unwrap().clone();
    let total = data.len();
    let keep_alive = st.keep_alive.load(Ordering::SeqCst);
    let conn_hdr = if keep_alive { "keep-alive" } else { "close" };

    let is_probe = range.as_deref() == Some("bytes=0-0");
    let is_data_get = method == "GET" && !is_probe;
    if is_data_get {
        st.hits.fetch_add(1, Ordering::SeqCst);
    }

    let simple = |status: &str, extra: &str| format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n{extra}Connection: {conn_hdr}\r\n\r\n");
    if path.starts_with("/404") {
        return s.write_all(simple("404 Not Found", "").as_bytes()).await.is_ok() && keep_alive;
    }
    let head_delay = *st.head_delay.lock().unwrap();
    if !head_delay.is_zero() {
        tokio::time::sleep(head_delay).await;
    }
    if path.starts_with("/reset") {
        return false; // dropped without a word
    }
    let redirect = |to: &str| simple("302 Found", &format!("Location: {to}\r\n"));
    if path.starts_with("/e429") {
        st.throttled.fetch_add(1, Ordering::SeqCst);
        return s.write_all(simple("429 Too Many Requests", "Retry-After: 0\r\n").as_bytes()).await.is_ok() && keep_alive;
    }
    if path.starts_with("/e500") {
        return s.write_all(simple("500 Internal Server Error", "").as_bytes()).await.is_ok() && keep_alive;
    }
    if path.starts_with("/auth/") {
        let ok = text.lines().any(|l| l.to_ascii_lowercase().starts_with("authorization:") && l.trim_end().ends_with("Basic dXNlcjpwYXNz"));
        if !ok {
            return s.write_all(simple("401 Unauthorized", "WWW-Authenticate: Basic realm=\"velox\"\r\n").as_bytes()).await.is_ok()
                && keep_alive;
        }
    }
    if let Some(token) = path.strip_prefix("/signed/").and_then(|r| r.split('/').next()) {
        let refused = s.peer_addr().ok().is_some_and(|p| *st.refuse_peer.lock().unwrap() == Some(p.ip()));
        if st.expired.lock().unwrap().contains(token) || refused {
            return s.write_all(simple("403 Forbidden", "").as_bytes()).await.is_ok() && keep_alive;
        }
    }
    if path.starts_with("/redirloop") {
        return s.write_all(redirect("/redirloop").as_bytes()).await.is_ok() && keep_alive;
    }
    if path.starts_with("/redirext") {
        let to = st.redirect_to.lock().unwrap().clone();
        return s.write_all(redirect(&to).as_bytes()).await.is_ok() && keep_alive;
    }
    if let Some(rest) = path.strip_prefix("/redir/") {
        if let Some((hops, tail)) = rest.split_once('/') {
            if let Ok(n) = hops.parse::<u32>() {
                let to = if n == 0 { format!("/{tail}") } else { format!("/redir/{}/{tail}", n - 1) };
                return s.write_all(redirect(&to).as_bytes()).await.is_ok() && keep_alive;
            }
        }
    }
    let fixed = st.routes.lock().unwrap().get(path.split('?').next().unwrap_or("")).cloned();
    if let Some(body) = fixed {
        let delay = *st.route_delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: {conn_hdr}\r\n\r\n", body.len());
        let ok = s.write_all(head.as_bytes()).await.is_ok() && (method == "HEAD" || s.write_all(&body).await.is_ok());
        return ok && keep_alive;
    }
    let no_range = path.starts_with("/norange");
    // `/huge/`: the size it claims is a petabyte, whatever it really holds.
    let advertised = if path.starts_with("/huge/") { 1u64 << 50 } else { total as u64 };
    let range = if no_range { None } else { range };
    let cap = st.max_concurrent.load(Ordering::SeqCst);
    if is_data_get && cap > 0 && st.active.load(Ordering::SeqCst) >= cap {
        st.throttled.fetch_add(1, Ordering::SeqCst);
        return s.write_all(simple("429 Too Many Requests", "").as_bytes()).await.is_ok() && keep_alive;
    }
    if is_data_get && take(&st.fail_next) {
        return s.write_all(simple("503 Service Unavailable", "Retry-After: 0\r\n").as_bytes()).await.is_ok() && keep_alive;
    }
    let _turn = if is_data_get && st.one_at_a_time.load(Ordering::SeqCst) { Some(st.turn.acquire().await.unwrap()) } else { None };

    let (status, body, hdrs): (&str, &[u8], String) = match (range.as_deref(), method.as_str()) {
        (Some(r), "GET") if total > 0 => {
            let (a, b) = r.trim_start_matches("bytes=").split_once('-').unwrap();
            let a: usize = a.parse().unwrap();
            let b = b.parse::<usize>().unwrap_or(total - 1).min(total - 1);
            if a > b {
                return s.write_all(simple("416 Range Not Satisfiable", "").as_bytes()).await.is_ok() && keep_alive;
            }
            ("206 Partial Content", &data[a..=b], format!("Content-Range: bytes {a}-{b}/{advertised}\r\nContent-Length: {}\r\n", b - a + 1))
        }
        _ => ("200 OK", &data[..], format!("Content-Length: {advertised}\r\n")),
    };
    // `/chunked/`: streamed without saying the size (an export made while it is sent), then the end-of-body mark.
    // Ranges are ignored there, and a HEAD tells nothing of the size either.
    if path.starts_with("/chunked/") {
        let head =
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: {conn_hdr}\r\n\r\n");
        if s.write_all(head.as_bytes()).await.is_err() {
            return false;
        }
        if method == "HEAD" {
            return keep_alive;
        }
        let delay = *st.chunk_delay.lock().unwrap();
        for chunk in data.chunks(32 * 1024) {
            let framed = [format!("{:x}\r\n", chunk.len()).as_bytes(), chunk, b"\r\n"].concat();
            if s.write_all(&framed).await.is_err() {
                return false;
            }
            st.bytes_served.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            count_peer(s, st, chunk.len());
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        return s.write_all(b"0\r\n\r\n").await.is_ok() && keep_alive;
    }
    let lie = if path.starts_with("/lie/") { st.lie_extra.load(Ordering::SeqCst) } else { 0 };
    let hdrs = if lie != 0 { format!("Content-Length: {}\r\n", (body.len() as i64 + lie).max(0)) } else { hdrs };
    let accept = if no_range { "" } else { "Accept-Ranges: bytes\r\n" };
    let disposition = st.content_disposition.lock().unwrap().as_ref().map(|v| format!("Content-Disposition: {v}\r\n")).unwrap_or_default();
    let junk = if path.starts_with("/big-header/") {
        format!("X-Junk: {}\r\n", "a".repeat(st.junk_header_bytes.load(Ordering::SeqCst)))
    } else {
        String::new()
    };
    let ctype = if path.starts_with("/html/") { "text/html; charset=utf-8" } else { "application/octet-stream" };
    let mut etag_hdr = if path.starts_with("/noetag/") { String::new() } else { format!("ETag: {etag}\r\n") };
    if path.contains("/dated/") {
        etag_hdr.push_str("Last-Modified: Mon, 05 Jan 2026 00:00:00 GMT\r\n");
    }
    let head = format!(
        "HTTP/1.1 {status}\r\n{etag_hdr}Server: velox-test/1.0\r\n{accept}Content-Type: {ctype}\r\nConnection: {conn_hdr}\r\n{disposition}{junk}{hdrs}\r\n"
    );
    if s.write_all(head.as_bytes()).await.is_err() {
        return false;
    }
    if method == "HEAD" {
        return keep_alive;
    }
    if path.starts_with("/hang/") && !is_probe {
        tokio::time::sleep(Duration::from_secs(600)).await; // headers out, body never
        return false;
    }
    let cut = is_data_get && take(&st.truncate_next);
    let body = if cut { &body[..body.len() / 2] } else { body };
    let delay = *st.chunk_delay.lock().unwrap();

    let now_active = st.active.fetch_add(1, Ordering::SeqCst) + 1;
    st.peak_concurrent.fetch_max(now_active, Ordering::SeqCst);
    let mut ok = true;
    let chunks = body.chunks(32 * 1024).count();
    for (i, chunk) in body.chunks(32 * 1024).enumerate() {
        if s.write_all(chunk).await.is_err() {
            st.cut_after_chunks.lock().unwrap().push(i);
            ok = false;
            break;
        }
        st.bytes_served.fetch_add(chunk.len() as u64, Ordering::SeqCst);
        count_peer(s, st, chunk.len());
        // Pace between chunks only: the request must not look "active" after its last byte is out.
        if !delay.is_zero() && i + 1 < chunks {
            tokio::time::sleep(delay).await;
        }
    }
    st.active.fetch_sub(1, Ordering::SeqCst);
    if !ok || cut {
        return false; // dropped: declared Content-Length was larger than what was sent
    }
    if keep_alive {
        true
    } else {
        let _ = s.shutdown().await;
        false
    }
}

#[cfg(test)]
mod sweep_tests {
    #[test]
    fn only_folders_of_ended_processes_are_removed() {
        if !std::path::Path::new("/proc").is_dir() {
            return;
        }
        let t = std::env::temp_dir();
        let dead = t.join("velox-sweeptest-4000000000-x"); // no such process
        let mine = t.join(format!("velox-sweeptest-{}-x", std::process::id()));
        let other = t.join("velox-not-a-test-folder");
        for d in [&dead, &mine, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        super::sweep_stale_test_folders();
        assert!(!dead.exists(), "an ended process's folder goes");
        assert!(mine.exists() && other.exists(), "this run's and unrelated folders stay");
        let _ = std::fs::remove_dir_all(&mine);
        let _ = std::fs::remove_dir_all(&other);
    }
}
