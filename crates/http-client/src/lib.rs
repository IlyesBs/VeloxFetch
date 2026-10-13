//! Thin, hardened HTTP layer over `reqwest`.
//!
//! Responsibilities: client configuration (timeouts, proxy, cookies, headers,
//! auth), resource discovery (HEAD, falling back to `Range: bytes=0-0`) and
//! *verified* range support detection. Every byte from the network is treated
//! as hostile: schemes are restricted, headers are parsed defensively and range
//! responses are validated against what was requested.

/// The transport error inside [`HttpError::Transport`] (its source chain tells DNS, TLS and resets apart).
pub use reqwest::Error as ReqwestError;
/// A server's answer (see [`HttpClient::get_patient`]).
pub use reqwest::Response;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

mod lanes;
pub use lanes::{spreadable, system_source_for, LaneState, LaneStatus, RouteSpec};
use lanes::{FamilyResolver, Lane, LaneUse, Lanes};

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("unsupported URL scheme `{0}` (only http/https allowed)")]
    UnsupportedScheme(String),
    #[error("invalid header `{0}`")]
    InvalidHeader(String),
    #[error("invalid proxy: {0}")]
    InvalidProxy(String),
    #[error("HTTP status {status}")]
    Status { status: u16, retry_after: Option<Duration> },
    #[error("server did not honor range request: {0}")]
    RangeNotHonored(String),
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server did not answer the quick check in time: it is preparing the file (a database export, a report
    /// built on request...). Fetch it with [`HttpClient::get_patient`] instead of asking again.
    #[error("the server is slow to answer (it may be preparing the file)")]
    SlowToAnswer,
    /// The file had started coming, then nothing arrived for too long.
    #[error("the server stopped sending")]
    Stalled,
    /// A transfer through one of several networks broke or stopped (that network rests; the part continues on
    /// another one). Not a sign of an overloaded server.
    #[error("a connection through one of several networks was lost")]
    NetworkLost,
    /// A ranged answer describes another version of the file than the one being downloaded (another strong `ETag`,
    /// another total size in `Content-Range`): its bytes must never join the ones already saved.
    #[error("the file changed on the server during the download: {0}")]
    SourceChanged(String),
}

impl HttpError {
    /// Coarse classification, groundwork for the Phase 2 retry engine.
    pub fn is_retryable(&self) -> bool {
        match self {
            HttpError::Status { status, .. } => matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504),
            // Anything that went wrong on the wire is transient (timeouts, resets, truncated bodies —
            // the latter surfaces as a `Decode`-kind error). Local/config failures are not.
            HttpError::Transport(e) => !(e.is_builder() || e.is_redirect() || tls_will_fail_again(e)),
            HttpError::Stalled | HttpError::NetworkLost => true,
            _ => false,
        }
    }

    /// The secure connection failed in a way that does not fix itself (see [`tls_will_fail_again`]).
    pub fn is_tls_permanent(&self) -> bool {
        matches!(self, HttpError::Transport(e) if tls_will_fail_again(e))
    }

    /// No answer within the time allowed.
    pub fn is_timeout(&self) -> bool {
        matches!(self, HttpError::Transport(e) if e.is_timeout())
    }
}

pub type Result<T> = std::result::Result<T, HttpError>;

/// A certificate the client refuses (unknown issuer, expired, wrong name...) or a server that does not speak TLS at
/// all: asking again gives the same answer, so it is not retried. Read from the error's causes only (the top message
/// names the link). Other handshake failures (an alert, a reset during the handshake) stay retryable.
fn tls_will_fail_again(e: &reqwest::Error) -> bool {
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        let t = s.to_string().to_ascii_lowercase();
        if t.contains("invalid peer certificate") || t.contains("certificate verify failed") || t.contains("invalidcontenttype") {
            return true;
        }
        src = s.source();
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicAuth {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    /// Timeout for metadata requests (HEAD / range probe). Body streams are not bound by it.
    pub request_timeout: Duration,
    /// Max idle time between two body reads.
    pub read_timeout: Duration,
    /// How long to wait for a server that prepares the file before answering (see [`HttpError::SlowToAnswer`]).
    pub slow_answer_wait: Duration,
    pub max_redirects: usize,
    /// A proxy for every request (`http://`, `https://`, `socks5://`, `socks5h://`). `None` = the
    /// system's proxy settings (environment variables, and on Windows the Internet Options), unless
    /// `no_proxy` is set.
    pub proxy: Option<String>,
    /// User name / password for `proxy` (sent to the proxy only, never to the servers).
    pub proxy_auth: Option<BasicAuth>,
    /// Connect directly, ignoring the system's proxy settings.
    pub no_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub basic_auth: Option<BasicAuth>,
    pub cookies: bool,
    /// Use HTTP/1.1 only. HTTP/2 would multiplex all segments of a download over a single TCP
    /// connection, defeating multi-connection speedups and per-connection server limits.
    pub http1_only: bool,
    /// Idle connections kept per host for reuse (skips DNS + TCP + TLS on the next request).
    pub pool_max_idle_per_host: usize,
    pub pool_idle_timeout: Duration,
    pub tcp_keepalive: Duration,
    /// In-process DNS cache lifetime; zero disables the cache (system resolver every time).
    pub dns_cache_ttl: Duration,
    /// Tests only (the app never sets it): spread every link over the networks given to
    /// [`HttpClient::set_routes`], not only https links to public addresses (test servers are local, plain http).
    pub spread_any_link: bool,
    /// A transfer through one of several networks that receives nothing for this long is given up: that network
    /// rests and the part continues on another one.
    pub lane_stall: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: format!("VeloxFetch/{}", env!("CARGO_PKG_VERSION")),
            connect_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(30),
            slow_answer_wait: Duration::from_secs(30 * 60),
            max_redirects: 10,
            proxy: None,
            proxy_auth: None,
            no_proxy: false,
            headers: Vec::new(),
            basic_auth: None,
            cookies: true,
            http1_only: true,
            pool_max_idle_per_host: 64,
            pool_idle_timeout: Duration::from_secs(90),
            tcp_keepalive: Duration::from_secs(30),
            dns_cache_ttl: Duration::from_secs(60),
            spread_any_link: false,
            lane_stall: lanes::LANE_STALL,
        }
    }
}

/// Extra headers for one download's requests (what the browser knew when it handed the link over).
///
/// Only `Referer`, `Cookie` and `User-Agent` are ever accepted, whatever is passed in; values that
/// are not valid header values (CR/LF, control characters) are dropped rather than sent. `Cookie`
/// is marked sensitive so it is never logged, and reqwest does not forward it across origins on a
/// redirect.
#[derive(Debug, Clone, Default)]
pub struct RequestExtras {
    headers: Vec<(HeaderName, HeaderValue)>,
}

/// The value of an `Authorization` header for HTTP Basic authentication.
pub fn basic_authorization(user: &str, password: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!("{user}:{password}");
    let mut out = String::from("Basic ");
    for chunk in raw.as_bytes().chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

impl RequestExtras {
    /// Names that may be supplied. Anything else is ignored.
    pub const ALLOWED: [&'static str; 4] = ["referer", "cookie", "user-agent", "authorization"];

    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut headers = Vec::new();
        for (k, v) in pairs {
            let lower = k.to_ascii_lowercase();
            if !Self::ALLOWED.contains(&lower.as_str()) {
                continue;
            }
            let (Ok(name), Ok(mut value)) = (HeaderName::from_bytes(lower.as_bytes()), HeaderValue::from_str(v)) else {
                continue;
            };
            if name == header::COOKIE || name == header::AUTHORIZATION {
                value.set_sensitive(true);
            }
            headers.push((name, value));
        }
        Self { headers }
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// The same, asking the server to close the connection after the answer (`Connection: close`): the next
    /// request opens a new one instead of reusing it. (HTTP/2 has no such header; it is left out there.)
    pub fn closing(&self) -> Self {
        let mut headers = self.headers.clone();
        headers.push((header::CONNECTION, HeaderValue::from_static("close")));
        Self { headers }
    }
}

/// Result of resource discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceInfo {
    /// URL after redirects.
    pub final_url: String,
    /// Set (to the final URL) when the original URL redirected.
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    /// What the server *claims* (`Accept-Ranges: bytes`).
    pub accept_ranges_header: bool,
    /// What the server actually did when asked for `bytes=0-0`.
    pub range_verified: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// `Server` response header (informational).
    pub server: Option<String>,
    /// Status of the request that verified range support (`206` = parallel download possible; `200` = whole
    /// file only). `0` when unknown (information saved by an older version).
    #[serde(default)]
    pub status: u16,
}

impl ResourceInfo {
    pub fn supports_parallel(&self) -> bool {
        self.range_verified && self.content_length.is_some()
    }
}

#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    /// Same settings, but waits up to `slow_answer_wait` for an answer (servers that prepare the file first).
    patient: reqwest::Client,
    auth: Option<BasicAuth>,
    request_timeout: Duration,
    /// Body idle limit on the patient client, once the file started coming.
    idle_body: Duration,
    /// What the clients were built from (lanes are built from it too).
    cfg: Arc<ClientConfig>,
    headers: HeaderMap,
    jar: Option<Arc<reqwest::cookie::Jar>>,
    /// The networks ranged requests are spread over (see [`lanes`]); shared by every copy of this client.
    lanes: Lanes,
    /// The names cache of the lanes (one for all of them).
    lane_dns: CachingResolver,
    /// Requests go through a proxy (given, or the system's from the environment): never spread.
    through_proxy: bool,
}

pub fn parse_url(s: &str) -> Result<Url> {
    let u = Url::parse(s).map_err(|e| HttpError::InvalidUrl(e.to_string()))?;
    match u.scheme() {
        "http" | "https" => Ok(u),
        other => Err(HttpError::UnsupportedScheme(other.to_string())),
    }
}

/// Is this a place only your own machine or network can reach (loopback, private ranges,
/// link-local incl. the cloud metadata address, `localhost`, `.local`...)?
pub fn is_local_host(url: &Url) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    fn v4(a: Ipv4Addr) -> bool {
        let o = a.octets();
        a.is_loopback()
            || a.is_private()
            || a.is_link_local()
            || a.is_unspecified()
            || a.is_broadcast()
            || (o[0] == 100 && (64..128).contains(&o[1]))
    }
    fn v6(a: Ipv6Addr) -> bool {
        if let Some(m) = a.to_ipv4_mapped() {
            return v4(m);
        }
        let seg = a.segments()[0];
        a.is_loopback() || a.is_unspecified() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
    }
    match url.host() {
        Some(url::Host::Ipv4(a)) => v4(a),
        Some(url::Host::Ipv6(a)) => v6(a),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || [".localhost", ".local", ".internal", ".lan", ".home.arpa"].iter().any(|s| d.ends_with(s))
        }
        None => false,
    }
}

/// A public site sending the client to an address on the user's own machine or network. That is how
/// a malicious server (or page) turns a download manager into a probe of local services.
pub fn crosses_into_local(from: &Url, to: &Url) -> bool {
    !is_local_host(from) && is_local_host(to)
}

pub enum RedirectDecision {
    Follow,
    /// Hand the redirect response back as it is (not http/https).
    Stop,
    Refuse(&'static str),
}

/// What to do with a redirect. `previous` is the chain so far, starting with the requested URL.
pub fn redirect_decision(previous: &[Url], next: &Url, max_redirects: usize) -> RedirectDecision {
    if previous.len() >= max_redirects {
        RedirectDecision::Refuse("too many redirects")
    } else if !matches!(next.scheme(), "http" | "https") {
        RedirectDecision::Stop
    } else if previous.last().is_some_and(|from| crosses_into_local(from, next)) {
        RedirectDecision::Refuse("refused a redirect from a public address to an address on this computer or network")
    } else {
        RedirectDecision::Follow
    }
}

impl HttpClient {
    pub fn new(cfg: &ClientConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        // Range math is meaningless on transformed bodies.
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        for (k, v) in &cfg.headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| HttpError::InvalidHeader(k.clone()))?;
            let mut val = HeaderValue::from_str(v).map_err(|_| HttpError::InvalidHeader(k.clone()))?;
            if name == header::AUTHORIZATION || name == header::COOKIE {
                val.set_sensitive(true);
            }
            headers.insert(name, val);
        }
        // One cookie jar for every route: a cookie the server set on one request goes with the next, whatever the
        // network or the client that sends it.
        let jar = cfg.cookies.then(|| Arc::new(reqwest::cookie::Jar::default()));
        Ok(Self {
            inner: build_client(cfg, &headers, jar.as_ref(), cfg.read_timeout, None)?,
            patient: build_client(cfg, &headers, jar.as_ref(), cfg.slow_answer_wait.max(cfg.read_timeout), None)?,
            auth: cfg.basic_auth.clone(),
            request_timeout: cfg.request_timeout,
            idle_body: cfg.read_timeout.max(Duration::from_secs(120)),
            through_proxy: cfg.proxy.is_some() || (!cfg.no_proxy && system_proxy_in_environment()),
            lane_dns: CachingResolver::system(cfg.dns_cache_ttl),
            cfg: Arc::new(cfg.clone()),
            headers,
            jar,
            lanes: Lanes::default(),
        })
    }

    /// Spread ranged requests over these networks from now on (every copy of this client, downloads already running
    /// included). Fewer than two: the normal route only. A network already in use keeps its connections and what was
    /// learned about it. Returns the names of networks that cannot be used on this system (nothing to bind to).
    pub fn set_routes(&self, specs: &[RouteSpec]) -> Vec<String> {
        let old = self.lanes.list();
        let mut lanes = Vec::new();
        let mut unusable = Vec::new();
        for spec in specs {
            // The same network bound the same way keeps its connections and what was learned about it (its other
            // addresses, such as IPv6 temporary ones, may change).
            if let Some(same) = old.iter().find(|l| l.binds_like(spec)) {
                same.set_addresses(spec.addresses.clone());
                lanes.push(same.clone());
                continue;
            }
            let Some(bound) = lanes::binding(spec) else {
                tracing::info!(network = %spec.name, "this network has nothing to bind connections to on this system");
                unusable.push(spec.name.clone());
                continue;
            };
            match build_client(
                &self.cfg,
                &self.headers,
                self.jar.as_ref(),
                self.cfg.read_timeout,
                Some((spec, bound, self.lane_dns.clone())),
            ) {
                Ok(client) => lanes.push(Arc::new(Lane::new(spec.clone(), client))),
                Err(e) => {
                    tracing::info!(network = %spec.name, error = %e, "this network cannot be used for downloads");
                    unusable.push(spec.name.clone());
                }
            }
        }
        // A network no longer listed is gone: its transfers end now (their parts continue on the others).
        for l in old.iter().filter(|l| !lanes.iter().any(|n| Arc::ptr_eq(n, l))) {
            l.mark_gone();
        }
        self.lanes.set(lanes);
        unusable
    }

    /// The networks given to [`set_routes`](Self::set_routes) (to give them to a new client).
    pub fn route_specs(&self) -> Vec<RouteSpec> {
        self.lanes.list().iter().map(|l| l.spec.clone()).collect()
    }

    /// The networks requests are spread over, and what each is doing.
    pub fn routes(&self) -> Vec<LaneStatus> {
        self.lanes.list().iter().map(|l| l.status()).collect()
    }

    /// How many networks this link's ranged requests are spread over (1: the usual route only).
    pub fn spread_networks(&self, url: &Url) -> usize {
        if self.spreads(url) {
            self.lanes.list().len()
        } else {
            1
        }
    }

    /// Would ranged requests for this link be spread over several networks now?
    pub fn spreads(&self, url: &Url) -> bool {
        self.lanes.list().len() >= 2 && (self.cfg.spread_any_link || spreadable(url, self.through_proxy))
    }

    fn req(&self, method: Method, url: &Url, extras: &RequestExtras) -> reqwest::RequestBuilder {
        self.req_on(&self.inner, method, url, extras)
    }

    fn req_on(&self, client: &reqwest::Client, method: Method, url: &Url, extras: &RequestExtras) -> reqwest::RequestBuilder {
        let mut r = client.request(method, url.clone());
        if let Some(a) = &self.auth {
            r = r.basic_auth(&a.username, a.password.as_deref());
        }
        for (name, value) in &extras.headers {
            r = r.header(name.clone(), value.clone());
        }
        r
    }

    fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        let s = resp.status();
        if s.is_success() {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| parse_retry_after(v, std::time::SystemTime::now()));
        Err(HttpError::Status { status: s.as_u16(), retry_after })
    }

    /// Discover size / type / validators and verify real Range behaviour.
    pub async fn probe(&self, url: &Url) -> Result<ResourceInfo> {
        self.probe_with(url, &RequestExtras::default()).await
    }

    /// [`probe`](Self::probe) with extra request headers.
    pub async fn probe_with(&self, url: &Url, extras: &RequestExtras) -> Result<ResourceInfo> {
        let mut head = None;
        // 1. HEAD
        match self.req(Method::HEAD, url, extras).timeout(self.request_timeout).send().await {
            Ok(r) if r.status().is_success() => head = Some(info_from_headers(url, &r)),
            Ok(r) => tracing::debug!(status = %r.status(), "HEAD not usable, falling back to ranged GET"),
            // No answer at all: the server prepares the file before answering anything. Asking again (a second
            // check, retries) would only make it start over; the download itself must wait for it.
            Err(e) if e.is_timeout() => return Err(HttpError::SlowToAnswer),
            Err(e) => tracing::debug!(error = %e, "HEAD failed, falling back to ranged GET"),
        }
        // 2. Ranged GET bytes=0-0: verifies range support against real behaviour.
        let resp = match self.req(Method::GET, url, extras).header(header::RANGE, "bytes=0-0").timeout(self.request_timeout).send().await {
            Err(e) if e.is_timeout() => return Err(HttpError::SlowToAnswer),
            other => other?,
        };
        let status = resp.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            // `bytes=0-0` on an empty resource: it is simply zero bytes long.
            let mut i = head.unwrap_or_else(|| info_from_headers(url, &resp));
            i.content_length = Some(0);
            i.range_verified = false;
            i.status = status.as_u16();
            return Ok(i);
        }
        let resp = Self::check(resp)?;
        let mut probe = info_from_headers(url, &resp);
        let mut range_verified = false;
        // Ranges of a compressed answer count bytes of the compressed form, which can differ from one answer to the
        // next: never cut such a file into ranges.
        if status == StatusCode::PARTIAL_CONTENT && !identity_encoding(&resp) {
            tracing::debug!("the server compresses its answer: ranges are not used");
        } else if status == StatusCode::PARTIAL_CONTENT {
            let cr = resp.headers().get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok());
            match cr.and_then(parse_content_range) {
                Some((0, 0, total)) => {
                    range_verified = true;
                    probe.content_length = total; // Content-Length of a 206 is the slice, not the file
                }
                _ => probe.content_length = None,
            }
        }
        if range_verified {
            // A verified probe body is exactly one byte: consuming it lets the connection go back
            // to the pool instead of being torn down (saves a TCP + TLS handshake per download).
            let _ = resp.bytes().await;
        } else {
            drop(resp); // possibly a whole file: never read it
        }

        let mut out = match head {
            Some(mut h) => {
                if range_verified {
                    h.content_length = probe.content_length.or(h.content_length);
                }
                h.final_url = probe.final_url;
                h.location = probe.location;
                h.etag = h.etag.or(probe.etag);
                h.last_modified = h.last_modified.or(probe.last_modified);
                h.server = h.server.or(probe.server);
                h.content_type = h.content_type.or(probe.content_type);
                h.content_disposition = h.content_disposition.or(probe.content_disposition);
                h.accept_ranges_header |= probe.accept_ranges_header;
                h
            }
            None => probe,
        };
        out.range_verified = range_verified;
        out.status = status.as_u16();
        Ok(out)
    }

    /// Plain GET of the whole resource (sequential mode).
    pub async fn get(&self, url: &Url) -> Result<reqwest::Response> {
        self.get_with(url, &RequestExtras::default()).await
    }

    pub async fn get_with(&self, url: &Url, extras: &RequestExtras) -> Result<reqwest::Response> {
        Self::check(self.req(Method::GET, url, extras).send().await?)
    }

    /// One plain GET that waits as long as `slow_answer_wait` for a server preparing the file
    /// ([`HttpError::SlowToAnswer`]). What the answer says about the file, and the answer itself to save from
    /// (a single connection, from the start: such a file is made for this request only). Read its body with
    /// [`patient_stream`](Self::patient_stream).
    pub async fn get_patient(&self, url: &Url, extras: &RequestExtras) -> Result<(ResourceInfo, reqwest::Response)> {
        let resp = Self::check(self.req_on(&self.patient, Method::GET, url, extras).send().await?)?;
        let mut info = info_from_headers(url, &resp);
        info.range_verified = false;
        info.status = resp.status().as_u16();
        Ok((info, resp))
    }

    /// The body of a [`get_patient`](Self::get_patient) answer. Once the file is coming, a silence longer than
    /// the usual idle limit (at least two minutes) means the connection is gone.
    pub fn patient_stream(&self, resp: reqwest::Response) -> impl Stream<Item = Result<Bytes>> + Send {
        use futures_util::StreamExt;
        let idle = self.idle_body;
        futures_util::stream::unfold(Some(resp.bytes_stream()), move |state| async move {
            let mut body = state?;
            match tokio::time::timeout(idle, body.next()).await {
                Ok(Some(item)) => Some((item.map_err(HttpError::from), Some(body))),
                Ok(None) => None,
                Err(_) => Some((Err(HttpError::Stalled), None)),
            }
        })
        .boxed()
    }

    /// GET an inclusive byte range and validate the server's answer.
    /// `if_range` (an ETag / Last-Modified value) guards against the resource changing mid-download.
    pub async fn get_range(&self, url: &Url, start: u64, end: u64, if_range: Option<&str>) -> Result<RangeStream> {
        self.get_range_with(url, start, end, if_range, &RequestExtras::default()).await
    }

    pub async fn get_range_with(
        &self,
        url: &Url,
        start: u64,
        end: u64,
        if_range: Option<&str>,
        extras: &RequestExtras,
    ) -> Result<RangeStream> {
        self.range_on(&self.inner, url, start, end, if_range, extras).await
    }

    /// [`get_range_with`](Self::get_range_with) for a server that prepares the file before answering
    /// ([`HttpError::SlowToAnswer`]): waits as long as [`get_patient`](Self::get_patient). Read its body with
    /// [`patient_range_stream`](Self::patient_range_stream).
    pub async fn get_range_patient(
        &self,
        url: &Url,
        start: u64,
        end: u64,
        if_range: Option<&str>,
        extras: &RequestExtras,
    ) -> Result<RangeStream> {
        self.range_on(&self.patient, url, start, end, if_range, extras).await
    }

    /// The body of a [`get_range_patient`](Self::get_range_patient) answer (see [`patient_stream`](Self::patient_stream)).
    pub fn patient_range_stream(&self, r: RangeStream) -> impl Stream<Item = Result<Bytes>> + Send {
        self.patient_stream(r.resp)
    }

    async fn range_on(
        &self,
        client: &reqwest::Client,
        url: &Url,
        start: u64,
        end: u64,
        if_range: Option<&str>,
        extras: &RequestExtras,
    ) -> Result<RangeStream> {
        // Spread over the networks (the patient client, for a server preparing the file, keeps one route).
        let host = url.host_str().unwrap_or_default();
        let spread = std::ptr::eq(client, &self.inner) && self.spreads(url) && self.lanes.system_route_is_ours(host, &self.lane_dns).await;
        let lane = if spread { self.lanes.pick(host) } else { None };
        if let Some(lane) = lane {
            let using = LaneUse::new(lane.clone());
            match self.range_once(&lane.client, url, start, end, if_range, extras).await {
                Ok(mut s) => {
                    lane.worked();
                    s.lane = Some((using, self.cfg.lane_stall, self.lanes.clone()));
                    return Ok(s);
                }
                // The network did not carry it (no connection, a broken one, another certificate on the way...): it
                // rests, and this request goes through the normal route now.
                Err(e @ HttpError::Transport(_)) => lane.failed(&e),
                // The server will not answer this network (a link signed for another address...): the normal route.
                Err(HttpError::Status { status: status @ (401 | 403 | 404 | 410 | 451), .. }) => lane.refused_by(host, status),
                Err(e) => return Err(e),
            }
        }
        self.range_once(client, url, start, end, if_range, extras).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn range_once(
        &self,
        client: &reqwest::Client,
        url: &Url,
        start: u64,
        end: u64,
        if_range: Option<&str>,
        extras: &RequestExtras,
    ) -> Result<RangeStream> {
        let mut r = self.req_on(client, Method::GET, url, extras).header(header::RANGE, format!("bytes={start}-{end}"));
        if let Some(hv) = if_range.and_then(|v| HeaderValue::from_str(v).ok()) {
            r = r.header(header::IF_RANGE, hv);
        }
        let resp = Self::check(r.send().await?)?;
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            return Err(HttpError::RangeNotHonored(format!("expected 206, got {} for bytes={start}-{end}", resp.status())));
        }
        if !identity_encoding(&resp) {
            return Err(HttpError::RangeNotHonored(format!("compressed answer (Content-Encoding) for bytes={start}-{end}")));
        }
        // `If-Range` with a strong ETag makes a server that changed the file answer 200 (refused above). One that
        // ignores `If-Range` still says its ETag: another strong one is another file.
        if let (Some(want), Some(got)) = (if_range.filter(|v| is_strong_etag(v)), hdr(&resp, header::ETAG).filter(|v| is_strong_etag(v))) {
            if want != got {
                return Err(HttpError::SourceChanged("the ETag of a ranged answer is not the download's".into()));
            }
        }
        let cr = resp.headers().get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(parse_content_range);
        let total = match cr {
            Some((s, e, total)) if s == start && e >= s && e <= end => total,
            other => return Err(HttpError::RangeNotHonored(format!("Content-Range {other:?} does not match requested {start}-{end}"))),
        };
        Ok(RangeStream { resp, total, lane: None })
    }
}

/// A client with these settings; with `bind`, one whose connections leave through that network only.
fn build_client(
    cfg: &ClientConfig,
    headers: &HeaderMap,
    jar: Option<&Arc<reqwest::cookie::Jar>>,
    read_timeout: Duration,
    bind: Option<(&RouteSpec, lanes::Binding, CachingResolver)>,
) -> Result<reqwest::Client> {
    let max_redirects = cfg.max_redirects;
    let mut b = reqwest::Client::builder()
        .user_agent(cfg.user_agent.clone())
        .default_headers(headers.clone())
        .connect_timeout(cfg.connect_timeout)
        .read_timeout(read_timeout)
        .pool_max_idle_per_host(cfg.pool_max_idle_per_host)
        .pool_idle_timeout(cfg.pool_idle_timeout)
        .tcp_keepalive(cfg.tcp_keepalive)
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            match redirect_decision(attempt.previous(), attempt.url(), max_redirects) {
                RedirectDecision::Follow => attempt.follow(),
                RedirectDecision::Stop => attempt.stop(),
                RedirectDecision::Refuse(why) => attempt.error(why),
            }
        }));
    if let Some(jar) = jar {
        b = b.cookie_provider(jar.clone());
    }
    if cfg.http1_only {
        b = b.http1_only();
    }
    match bind {
        Some((spec, lanes::Binding { local, v4, v6 }, dns)) => {
            // A network that does not answer is found out quickly: its request goes through another one.
            b = b.connect_timeout(cfg.connect_timeout.min(Duration::from_secs(8))).dns_resolver(Arc::new(FamilyResolver {
                inner: dns,
                v4,
                v6,
            }));
            if let Some(ip) = local {
                b = b.local_address(ip);
            }
            b = lanes::bind_interface(b, spec);
        }
        None if !cfg.dns_cache_ttl.is_zero() => b = b.dns_resolver(Arc::new(CachingResolver::system(cfg.dns_cache_ttl))),
        None => {}
    }
    if let Some(p) = &cfg.proxy {
        let mut proxy = reqwest::Proxy::all(p).map_err(|e| HttpError::InvalidProxy(e.to_string()))?;
        if let Some(a) = &cfg.proxy_auth {
            proxy = proxy.basic_auth(&a.username, a.password.as_deref().unwrap_or(""));
        }
        b = b.proxy(proxy);
    } else if cfg.no_proxy {
        b = b.no_proxy();
    }
    Ok(b.build()?)
}

/// A proxy for https links in the environment (`HTTPS_PROXY`, `ALL_PROXY`, in either case), which the system mode uses.
pub fn system_proxy_in_environment() -> bool {
    ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"].iter().any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

type LookupFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
type DnsCache = Arc<Mutex<HashMap<String, (Instant, Vec<SocketAddr>)>>>;
type LookupFn = Arc<dyn Fn(String) -> LookupFuture + Send + Sync>;

/// DNS resolver with a small TTL cache in front of the system resolver.
/// Parallel segments and back-to-back downloads to the same host resolve once, not per connection.
#[derive(Clone)]
pub struct CachingResolver {
    ttl: Duration,
    cache: DnsCache,
    lookup: LookupFn,
    lookups: Arc<AtomicUsize>,
}

impl CachingResolver {
    pub fn system(ttl: Duration) -> Self {
        Self::with_lookup(ttl, |host| Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect()) }))
    }

    pub fn with_lookup(ttl: Duration, lookup: impl Fn(String) -> LookupFuture + Send + Sync + 'static) -> Self {
        Self { ttl, cache: Default::default(), lookup: Arc::new(lookup), lookups: Default::default() }
    }

    /// Number of real (uncached) lookups performed so far.
    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }

    pub async fn resolve_host(&self, host: &str) -> std::io::Result<Vec<SocketAddr>> {
        let key = host.to_ascii_lowercase();
        if let Some((at, addrs)) = self.cache.lock().unwrap().get(&key) {
            if at.elapsed() < self.ttl {
                return Ok(addrs.clone());
            }
        }
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let addrs = (self.lookup)(key.clone()).await?;
        if addrs.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses"));
        }
        self.cache.lock().unwrap().insert(key, (Instant::now(), addrs.clone()));
        Ok(addrs)
    }
}

impl reqwest::dns::Resolve for CachingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        Box::pin(async move {
            let addrs = this.resolve_host(name.as_str()).await?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct RangeStream {
    resp: reqwest::Response,
    total: Option<u64>,
    /// The network it came through, when requests are spread (see [`HttpClient::set_routes`]).
    lane: Option<(LaneUse, Duration, Lanes)>,
}

impl RangeStream {
    /// The file's total size according to this answer's `Content-Range` (`None`: `*`, not said).
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// The name of the network it came through, when requests are spread over several.
    pub fn network(&self) -> Option<&str> {
        self.lane.as_ref().map(|(l, _, _)| l.0.spec.name.as_str())
    }

    pub fn into_stream(self) -> impl Stream<Item = Result<Bytes>> + Send {
        use futures_util::StreamExt;
        let Some((lane, stall, lanes)) = self.lane else {
            return self.resp.bytes_stream().map(|r| r.map_err(HttpError::from)).boxed();
        };
        // Through one of several networks:
        // * a network that stops sending (a Wi-Fi that froze, a phone that lost its signal) is given up after
        //   `ClientConfig::lane_stall` instead of the usual idle limit; it rests and the part continues on another one;
        // * a transfer that crawls (under an eighth of the best network's speed per connection, over `lanes::CRAWL_WINDOW`
        //   of waiting for bytes) is ended without resting its network: its part continues at once on a faster one, so
        //   the end of a file never waits for it.
        // Only the time spent waiting for bytes counts: a speed limit that keeps the reader waiting is not slowness.
        struct Lap {
            bytes: u64,
            waited: Duration,
        }
        futures_util::stream::unfold(Some((self.resp.bytes_stream(), lane, Lap { bytes: 0, waited: Duration::ZERO })), move |state| {
            let lanes = lanes.clone();
            async move {
                let (mut body, lane, mut lap) = state?;
                let asked = Instant::now();
                let next = tokio::select! {
                    biased;
                    r = tokio::time::timeout(stall, body.next()) => r,
                    _ = lane.0.until_gone() => {
                        tracing::debug!(network = %lane.0.spec.name, "the network went away: its part continues on another one");
                        return Some((Err(HttpError::NetworkLost), None));
                    }
                };
                match next {
                    Ok(Some(Ok(b))) => {
                        lane.received(b.len());
                        lap.bytes += b.len() as u64;
                        lap.waited += asked.elapsed();
                        if lap.waited >= lanes::CRAWL_WINDOW {
                            let speed = lap.bytes as f64 / lap.waited.as_secs_f64();
                            if lanes::crawls(speed, lanes.fastest_rate()) {
                                tracing::debug!(network = %lane.0.spec.name, speed, "a transfer crawls: its part moves to a faster network");
                                return Some((Err(HttpError::NetworkLost), None));
                            }
                            lap = Lap { bytes: 0, waited: Duration::ZERO };
                        }
                        Some((Ok(b), Some((body, lane, lap))))
                    }
                    // The transfer broke on this network: it rests (the part continues on another one).
                    Ok(Some(Err(e))) => {
                        lane.0.failed(&e);
                        Some((Err(HttpError::NetworkLost), None))
                    }
                    Ok(None) => None,
                    Err(_) => {
                        lane.0.failed(&"nothing received for a while");
                        Some((Err(HttpError::NetworkLost), None))
                    }
                }
            }
        })
        .boxed()
    }
}

/// Body stream of a plain (non-range) response.
pub fn response_stream(resp: reqwest::Response) -> impl Stream<Item = Result<Bytes>> + Send {
    use futures_util::StreamExt;
    resp.bytes_stream().map(|r| r.map_err(HttpError::from))
}

fn hdr(resp: &reqwest::Response, name: header::HeaderName) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

/// No `Content-Encoding`, or `identity`: the bytes are the file's own.
fn identity_encoding(resp: &reqwest::Response) -> bool {
    hdr(resp, header::CONTENT_ENCODING)
        .is_none_or(|v| v.split(',').all(|p| p.trim().is_empty() || p.trim().eq_ignore_ascii_case("identity")))
}

/// A strong entity tag (`"..."`); a weak one (`W/"..."`) only says "about the same".
pub fn is_strong_etag(v: &str) -> bool {
    let v = v.trim();
    v.len() >= 2 && v.starts_with('"') && v.ends_with('"')
}

fn info_from_headers(orig: &Url, resp: &reqwest::Response) -> ResourceInfo {
    let final_url = resp.url().clone();
    let redirected = &final_url != orig;
    ResourceInfo {
        final_url: final_url.to_string(),
        location: redirected.then(|| final_url.to_string()),
        content_length: hdr(resp, header::CONTENT_LENGTH).and_then(|v| v.trim().parse().ok()),
        content_type: hdr(resp, header::CONTENT_TYPE),
        content_disposition: hdr(resp, header::CONTENT_DISPOSITION),
        accept_ranges_header: hdr(resp, header::ACCEPT_RANGES).map(|v| v.to_ascii_lowercase().contains("bytes")).unwrap_or(false),
        range_verified: false,
        etag: hdr(resp, header::ETAG),
        last_modified: hdr(resp, header::LAST_MODIFIED),
        server: hdr(resp, header::SERVER),
        status: resp.status().as_u16(),
    }
}

/// `Retry-After`: a number of seconds, or an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`, the form the
/// standard names first). A date already in the past means "now". Anything else is ignored.
pub fn parse_retry_after(v: &str, now: std::time::SystemTime) -> Option<Duration> {
    let v = v.trim();
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse::<u64>().ok().map(Duration::from_secs);
    }
    let when = parse_http_date(v)?;
    Some(when.duration_since(now).unwrap_or(Duration::ZERO))
}

/// IMF-fixdate only (`Sun, 06 Nov 1994 08:49:37 GMT`), with strict ranges.
fn parse_http_date(v: &str) -> Option<std::time::SystemTime> {
    let mut it = v.split_ascii_whitespace();
    let (wday, day, mon, year, time, zone) = (it.next()?, it.next()?, it.next()?, it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || zone != "GMT" || !wday.ends_with(',') || wday.len() != 4 {
        return None;
    }
    let day: u64 = day.parse().ok().filter(|d| (1..=31).contains(d))?;
    let month =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == mon)? as u64 + 1;
    let year: u64 = year.parse().ok().filter(|y| (1970..=9999).contains(y) && year.len() == 4)?;
    let mut t = time.split(':');
    let (h, mi, s): (u64, u64, u64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    if t.next().is_some() || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days since 1970-01-01 (proleptic Gregorian, Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146_097 + doe).checked_sub(719_468)?;
    Some(std::time::UNIX_EPOCH + Duration::from_secs(days * 86_400 + h * 3600 + mi * 60 + s))
}

/// Parse `bytes START-END/TOTAL` (TOTAL may be `*`).
pub fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (s, e) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((s.trim().parse().ok()?, e.trim().parse().ok()?, total))
}

/// Extract a filename from `Content-Disposition` (RFC 6266 / 5987). Unsanitized.
pub fn filename_from_content_disposition(v: &str) -> Option<String> {
    let mut plain = None;
    for part in v.split(';').skip(1) {
        let Some((k, val)) = part.trim().split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let val = val.trim();
        if k == "filename*" {
            // charset'lang'percent-encoded
            if let Some(s) = val.splitn(3, '\'').nth(2).and_then(percent_decode) {
                return Some(s);
            }
        } else if k == "filename" {
            plain = Some(val.trim_matches('"').to_string());
        }
    }
    plain
}

pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn local_addresses_are_recognised() {
        for local in [
            "http://127.0.0.1/",
            "http://127.1.2.3:8080/",
            "http://localhost/",
            "http://LOCALHOST./",
            "http://a.localhost/",
            "http://10.1.2.3/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://0.0.0.0/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fd12:3456::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://nas.local/",
            "http://printer.lan/",
            "http://db.internal/",
            "http://x.home.arpa/",
        ] {
            assert!(is_local_host(&u(local)), "{local}");
        }
        for public in [
            "http://example.com/",
            "https://8.8.8.8/",
            "http://172.32.0.1/",
            "http://100.128.0.1/",
            "http://[2001:4860:4860::8888]/",
            "http://localhost.example.com/",
            "http://notlocal/",
        ] {
            assert!(!is_local_host(&u(public)), "{public}");
        }
    }

    #[test]
    fn a_public_site_cannot_redirect_into_the_local_network_but_local_to_local_is_fine() {
        let chain = |from: &str| vec![u(from)];
        let d = |from: &str, to: &str| redirect_decision(&chain(from), &u(to), 5);
        assert!(matches!(d("https://cdn.example.com/a", "http://127.0.0.1:9200/_cluster"), RedirectDecision::Refuse(_)));
        assert!(matches!(d("https://cdn.example.com/a", "http://169.254.169.254/latest/meta-data/"), RedirectDecision::Refuse(_)));
        assert!(matches!(d("https://cdn.example.com/a", "https://intranet.local/x"), RedirectDecision::Refuse(_)));
        assert!(matches!(d("https://cdn.example.com/a", "https://other.example.org/b"), RedirectDecision::Follow));
        assert!(matches!(d("http://192.168.1.5/a", "http://192.168.1.6/b"), RedirectDecision::Follow), "LAN to LAN (a NAS) is normal");
        assert!(matches!(d("http://127.0.0.1:8000/a", "http://localhost:8000/b"), RedirectDecision::Follow), "a developer's own server");
        assert!(matches!(d("http://192.168.1.5/a", "https://example.com/b"), RedirectDecision::Follow), "leaving the LAN is fine");
        assert!(matches!(d("https://example.com/a", "ftp://example.com/b"), RedirectDecision::Stop));
        assert!(matches!(redirect_decision(&vec![u("https://a.example/"); 5], &u("https://b.example/"), 5), RedirectDecision::Refuse(_)));
    }

    #[test]
    fn basic_authorization_matches_the_rfc_examples() {
        assert_eq!(basic_authorization("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(basic_authorization("a", ""), "Basic YTo=");
        assert_eq!(basic_authorization("ab", "c"), "Basic YWI6Yw==");
        assert_eq!(basic_authorization("\u{e9}", "p:w"), "Basic w6k6cDp3");
    }

    #[test]
    fn extras_accept_only_the_allowed_headers_and_drop_bad_values() {
        let pairs = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let e = RequestExtras::from_pairs(&pairs(&[
            ("Referer", "https://a.test/page"),
            ("COOKIE", "sid=1; theme=dark"),
            ("User-Agent", "Mozilla/5.0"),
            ("Authorization", "Bearer x"),
            ("Host", "evil.test"),
            ("X-Anything", "1"),
            ("Range", "bytes=0-"),
            ("Cookie", "a=b\r\nX-Injected: 1"),
            ("Referer", "bad\0value"),
        ]));
        let names: Vec<_> = e.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["referer", "cookie", "user-agent", "authorization"], "only allowed names, injection attempts dropped");
        assert!(e.headers[1].1.is_sensitive() && e.headers[3].1.is_sensitive(), "cookies and credentials are marked sensitive");
        assert!(RequestExtras::from_pairs(&[]).is_empty());
    }

    /// A server that answers every request with `answer` (a whole raw HTTP response).
    async fn answering(answer: &'static str) -> Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(answer.as_bytes()).await;
                });
            }
        });
        Url::parse(&format!("http://{addr}/f.bin")).unwrap()
    }

    /// Several networks at once: a request goes out through a network, with the cookie the server set on the normal
    /// route, and is checked like any other.
    #[cfg(any(target_os = "linux", windows))] // macOS has only 127.0.0.1 on its loopback
    #[tokio::test]
    async fn requests_spread_over_networks_share_the_cookies_and_the_checks() {
        use std::net::{IpAddr, Ipv4Addr};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(IpAddr, String)>>> = Default::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, peer)) = listener.accept().await else { return };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    log.lock().unwrap().push((peer.ip(), head.clone()));
                    let answer = if head.starts_with("head") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nSet-Cookie: sid=s3cr3t; Path=/\r\nConnection: close\r\n\r\n".to_string()
                    } else if head.contains("range: bytes=1-2") {
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1-2/4\r\nContent-Length: 2\r\nConnection: close\r\n\r\nbc"
                            .to_string()
                    } else {
                        // A range the client did not ask for: refused by the checks, whatever the route.
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1/4\r\nContent-Length: 2\r\nConnection: close\r\n\r\nab"
                            .to_string()
                    };
                    let _ = sock.write_all(answer.as_bytes()).await;
                });
            }
        });
        let url = Url::parse(&format!("http://{addr}/f.bin")).unwrap();
        let client = HttpClient::new(&ClientConfig { spread_any_link: true, dns_cache_ttl: Duration::ZERO, ..Default::default() }).unwrap();
        let net = |n: &str, last: u8| RouteSpec {
            name: n.into(),
            interface: cfg!(target_os = "linux").then(|| "lo".to_string()),
            v4: Some(Ipv4Addr::new(127, 0, 0, last)),
            v6: None,
            addresses: vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)), IpAddr::V4(Ipv4Addr::LOCALHOST)],
        };
        assert!(client.set_routes(&[net("a", 2), net("b", 3)]).is_empty());
        assert!(client.spreads(&url));
        let _ = client.req(Method::HEAD, &url, &RequestExtras::default()).send().await.unwrap();
        let r = client.get_range(&url, 1, 2, None).await.unwrap();
        let through = r.network().map(str::to_owned);
        assert!(matches!(through.as_deref(), Some("a" | "b")), "{through:?}");
        let body: Vec<u8> =
            futures_util::StreamExt::collect::<Vec<_>>(r.into_stream()).await.into_iter().flat_map(|c| c.unwrap()).collect();
        assert_eq!(body, b"bc");
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_ne!(last.0, IpAddr::V4(Ipv4Addr::LOCALHOST), "it went out through a network");
        assert!(last.1.contains("cookie: sid=s3cr3t"), "the cookie set on the normal route went with it: {}", last.1);
        assert!(matches!(client.get_range(&url, 2, 3, None).await, Err(HttpError::RangeNotHonored(_))), "checked like any other");
        assert!(client.routes().iter().any(|l| l.state == LaneState::Working && l.bytes == 2));
        // Read again with another (IPv6 temporary) address: the same networks, what was learned is kept.
        let mut again = vec![net("a", 2), net("b", 3)];
        again[0].addresses.push(IpAddr::V6(std::net::Ipv6Addr::new(0x2a01, 0, 0, 0, 0, 0, 0, 9)));
        assert!(client.set_routes(&again).is_empty());
        assert!(client.routes().iter().any(|l| l.state == LaneState::Working && l.bytes == 2), "kept: {:?}", client.routes());
        assert_eq!(client.spread_networks(&url), 2);
    }

    /// A network that stops sending in the middle of a transfer is given up quickly and rests.
    #[cfg(any(target_os = "linux", windows))]
    #[tokio::test]
    async fn a_network_that_stops_sending_is_given_up_and_rests() {
        use std::net::{IpAddr, Ipv4Addr};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    // Half the answer, then silence (the connection stays open).
                    let _ = sock
                        .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-9/10\r\nContent-Length: 10\r\n\r\nabcde")
                        .await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let url = Url::parse(&format!("http://{addr}/f.bin")).unwrap();
        let cfg = ClientConfig { spread_any_link: true, lane_stall: Duration::from_millis(300), ..Default::default() };
        let client = HttpClient::new(&cfg).unwrap();
        let net = |n: &str, last: u8| RouteSpec {
            name: n.into(),
            interface: cfg!(target_os = "linux").then(|| "lo".to_string()),
            v4: Some(Ipv4Addr::new(127, 0, 0, last)),
            v6: None,
            addresses: vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)), IpAddr::V4(Ipv4Addr::LOCALHOST)],
        };
        client.set_routes(&[net("a", 2), net("b", 3)]);
        let r = client.get_range(&url, 0, 9, None).await.unwrap();
        let through = r.network().unwrap().to_string();
        let t = Instant::now();
        let got: Vec<_> = futures_util::StreamExt::collect::<Vec<_>>(r.into_stream()).await;
        assert!(t.elapsed() < Duration::from_secs(5), "given up quickly");
        assert!(matches!(got.last(), Some(Err(HttpError::NetworkLost))), "{got:?}");
        assert!(client.routes().iter().any(|l| l.name == through && l.state == LaneState::Resting), "{:?}", client.routes());
    }

    #[tokio::test]
    async fn ranged_answers_are_checked_before_their_bytes_are_used() {
        let client = HttpClient::new(&ClientConfig::default()).unwrap();
        let ok = answering("HTTP/1.1 206 Partial Content\r\nETag: \"v1\"\r\nContent-Range: bytes 5-9/100\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        let r = client.get_range(&ok, 5, 9, Some("\"v1\"")).await.unwrap();
        assert_eq!(r.total(), Some(100), "the answer's total is passed on for the caller to compare");

        // Another strong ETag than the one asked for in If-Range (a server that ignores If-Range).
        let r = client.get_range(&ok, 5, 9, Some("\"v0\"")).await;
        assert!(matches!(r, Err(HttpError::SourceChanged(_))), "{:?}", r.err());
        // A weak validator proves nothing either way: not refused on that ground.
        assert!(client.get_range(&ok, 5, 9, Some("W/\"v0\"")).await.is_ok());

        let gz = answering("HTTP/1.1 206 Partial Content\r\nContent-Encoding: gzip\r\nContent-Range: bytes 5-9/100\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(matches!(client.get_range(&gz, 5, 9, None).await, Err(HttpError::RangeNotHonored(_))), "compressed ranges are refused");
        let identity = answering("HTTP/1.1 206 Partial Content\r\nContent-Encoding: identity\r\nContent-Range: bytes 5-9/100\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(client.get_range(&identity, 5, 9, None).await.is_ok());

        let whole = answering("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(matches!(client.get_range(&whole, 5, 9, None).await, Err(HttpError::RangeNotHonored(_))), "200 to a range");
        let shifted = answering(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-8/100\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )
        .await;
        assert!(matches!(client.get_range(&shifted, 5, 9, None).await, Err(HttpError::RangeNotHonored(_))), "another start");
        let beyond = answering(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 5-12/100\r\nContent-Length: 8\r\nConnection: close\r\n\r\nhello123",
        )
        .await;
        assert!(matches!(client.get_range(&beyond, 5, 9, None).await, Err(HttpError::RangeNotHonored(_))), "past the asked end");
        let missing = answering("HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(matches!(client.get_range(&missing, 5, 9, None).await, Err(HttpError::RangeNotHonored(_))), "no Content-Range");
    }

    #[tokio::test]
    async fn a_compressed_answer_is_never_cut_into_ranges() {
        let client = HttpClient::new(&ClientConfig::default()).unwrap();
        let gz = answering("HTTP/1.1 206 Partial Content\r\nContent-Encoding: gzip\r\nContent-Range: bytes 0-0/100\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx").await;
        let i = client.probe(&gz).await.unwrap();
        assert!(!i.range_verified, "ranges of compressed bytes are not the file's");
    }

    #[tokio::test]
    async fn extras_reach_the_server_and_do_not_leak_into_requests_without_them() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let seen = seen2.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    seen.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
                    let _ = sock
                        .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/10\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx")
                        .await;
                });
            }
        });
        let client = HttpClient::new(&ClientConfig::default()).unwrap();
        let url = Url::parse(&format!("http://{addr}/f.bin")).unwrap();
        let extras = RequestExtras::from_pairs(&[
            ("referer".into(), "https://origin.test/page".into()),
            ("cookie".into(), "sid=abc123".into()),
            ("user-agent".into(), "BrowserUA/9".into()),
        ]);
        client.probe_with(&url, &extras).await.unwrap();
        let with_extras = seen.lock().unwrap().clone();
        assert!(!with_extras.is_empty());
        for r in &with_extras {
            assert!(r.contains("cookie: sid=abc123"), "cookie missing: {r}");
            assert!(r.contains("referer: https://origin.test/page"), "referer missing: {r}");
            assert!(r.contains("user-agent: browserua/9"), "browser UA missing: {r}");
            assert!(!r.contains("velox"), "our own UA must be replaced: {r}");
        }
        let n = with_extras.len();
        client.probe(&url).await.unwrap();
        let all = seen.lock().unwrap().clone();
        for r in &all[n..] {
            assert!(!r.contains("cookie:") && !r.contains("referer:"), "extras leaked into a plain request: {r}");
            assert!(r.contains("velox"), "plain requests keep the default UA: {r}");
        }
    }

    #[test]
    fn content_range() {
        assert_eq!(parse_content_range("bytes 0-0/1234"), Some((0, 0, Some(1234))));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("garbage"), None);
    }

    #[test]
    fn disposition() {
        assert_eq!(filename_from_content_disposition("attachment; filename=\"a.zip\""), Some("a.zip".into()));
        assert_eq!(filename_from_content_disposition("attachment; filename=\"x\"; filename*=UTF-8''h%C3%A9.txt"), Some("hé.txt".into()));
    }

    #[tokio::test]
    async fn dns_cache_hits_expires_and_skips_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let r = CachingResolver::with_lookup(Duration::from_millis(80), move |h| {
            let c = c2.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                if h == "bad.test" {
                    return Err(std::io::Error::other("nxdomain"));
                }
                Ok(vec!["127.0.0.1:0".parse().unwrap()])
            })
        });
        for _ in 0..5 {
            r.resolve_host("Example.COM").await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "case-insensitive cache hit");
        tokio::time::sleep(Duration::from_millis(120)).await;
        r.resolve_host("example.com").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "expired entry is refreshed");
        assert!(r.resolve_host("bad.test").await.is_err());
        assert!(r.resolve_host("bad.test").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 4, "failures are never cached");
        assert_eq!(r.lookups(), 4);
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates_and_nothing_else() {
        use std::time::{Duration as D, UNIX_EPOCH};
        let now = UNIX_EPOCH + D::from_secs(784_111_777); // Sun, 06 Nov 1994 08:49:37 GMT
        assert_eq!(parse_retry_after("120", now), Some(D::from_secs(120)));
        assert_eq!(parse_retry_after(" 0 ", now), Some(D::ZERO));
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:50:07 GMT", now), Some(D::from_secs(30)), "a date in the future");
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now), Some(D::ZERO));
        assert_eq!(parse_retry_after("Sat, 05 Nov 1994 08:49:37 GMT", now), Some(D::ZERO), "a date in the past means now");
        assert_eq!(parse_retry_after("Mon, 07 Nov 1994 08:49:37 GMT", now), Some(D::from_secs(86_400)));
        // A leap day and a year boundary.
        assert_eq!(
            parse_retry_after("Thu, 29 Feb 2024 00:00:00 GMT", UNIX_EPOCH + D::from_secs(1_709_164_800 - 10)),
            Some(D::from_secs(10))
        );
        for bad in [
            "",
            " ",
            "-1",
            "1.5",
            "1e3",
            "soon",
            "tomorrow",
            "0x10",
            "99999999999999999999",
            "Sun, 06 Nov 1994 08:49:37",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "Sun, 06 Foo 1994 08:49:37 GMT",
            "Sun, 06 Nov 94 08:49:37 GMT",
            "Sun, 06 Nov 1969 08:49:37 GMT",
            "Sun, 06 Nov 1994 25:49:37 GMT",
            "Sun, 06 Nov 1994 08:61:37 GMT",
            "Sun 06 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 08:49:37 GMT extra",
        ] {
            assert_eq!(parse_retry_after(bad, now), None, "{bad:?}");
        }
    }

    #[test]
    fn scheme_rules() {
        assert!(parse_url("https://a.b/c").is_ok());
        assert!(matches!(parse_url("file:///etc/passwd"), Err(HttpError::UnsupportedScheme(_))));
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

    #[allow(dead_code)]
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
    fn hostile_header_values_never_crash_the_parsers() {
        let pool = [
            "attachment",
            "filename=",
            "filename*=UTF-8''",
            "%",
            "%zz",
            "%00",
            "%E2%80",
            "\"",
            ";",
            "bytes ",
            "*/",
            "-",
            "99999999999999999999",
            "..",
            "\\",
            "\r\n",
        ];
        let mut rng = Rng(11);
        for _ in 0..20_000 {
            let mut s = String::new();
            for _ in 0..rng.below(14) {
                if rng.below(3) == 0 {
                    s.push(char::from_u32(rng.next() as u32 % 0x800).unwrap_or('?'));
                } else {
                    s.push_str(pool[rng.below(pool.len())]);
                }
            }
            let _ = parse_content_range(&s);
            let _ = filename_from_content_disposition(&s);
            let _ = percent_decode(&s);
            let _ = RequestExtras::from_pairs(&[(s.clone(), s.clone())]);
            if let Ok(u) = Url::parse(&s) {
                let _ = is_local_host(&u);
            }
        }
    }

    #[test]
    fn content_range_arithmetic_cannot_overflow() {
        for v in [
            "bytes 0-18446744073709551615/18446744073709551615",
            "bytes 18446744073709551615-18446744073709551615/*",
            "bytes 5-2/10",
            "bytes */0",
        ] {
            let _ = parse_content_range(v);
        }
    }
}
