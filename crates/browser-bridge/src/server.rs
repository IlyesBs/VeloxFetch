//! The app side of the bridge: accepts connections from the native host, authenticates them,
//! re-validates every message and hands accepted downloads to a [`Handler`].
//!
//! The server does not trust the host any more than the host trusts the browser: it demands
//! mutual authentication, enforces the caller allowlist, applies size and rate limits and runs the
//! same strict message validation.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::auth::{from_hex, random_bytes, to_hex, Token, NONCE_LEN};
use crate::caller::{Allowlist, Caller};
use crate::frame::{read_frame, write_frame, FrameError, MAX_IPC_MESSAGE};
use crate::protocol::{parse_value, DownloadRequest, Message, Reply};
use crate::transport::{BoxStream, Listener};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct DownloadAccepted {
    pub id: i64,
    pub filename: String,
}

#[derive(Debug, Clone)]
pub struct HandlerError {
    pub code: &'static str,
    pub message: String,
}

/// What the app does with validated requests. Implemented by the desktop shell on top of the
/// download manager; this crate knows nothing about downloading.
pub trait Handler: Send + Sync + 'static {
    fn app_version(&self) -> String;
    /// The user wants the "Download video" bar in the browser (told to the extension with each ping).
    fn video_bar(&self) -> bool {
        true
    }
    fn download(&self, req: DownloadRequest, caller: Caller) -> BoxFuture<'_, Result<DownloadAccepted, HandlerError>>;
    /// An allowed extension reached the app (after the handshake): it is installed and working.
    fn contacted(&self, _caller: &Caller) {}
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub allow: Allowlist,
    pub max_connections: usize,
    pub handshake_timeout: Duration,
    /// At most `rate_limit.0` requests per `rate_limit.1` on one connection.
    pub rate_limit: (usize, Duration),
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            allow: Allowlist::default(),
            max_connections: 8,
            handshake_timeout: Duration::from_secs(5),
            rate_limit: (30, Duration::from_secs(10)),
        }
    }
}

// ---------------------------------------------------------------------------- wire types

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Hello {
    pub hello: String,
    pub version: u32,
    pub nonce: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Auth {
    pub nonce: String,
    pub proof: String,
    pub caller: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthResult {
    pub ok: bool,
    pub proof: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub id: u64,
    pub message: Value,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Response {
    pub id: u64,
    pub reply: Reply,
}

// ---------------------------------------------------------------------------- server

/// Accept connections until the future is dropped / aborted.
pub async fn serve<H: Handler>(mut listener: Listener, token: Token, handler: Arc<H>, cfg: ServerConfig) {
    let permits = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    loop {
        let stream = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "browser bridge: accept failed");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        // Too many connections: drop the new one instead of queueing without bound.
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            tracing::warn!("browser bridge: connection limit reached, refusing a connection");
            continue;
        };
        let (token, handler, cfg) = (token.clone(), handler.clone(), cfg.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = connection(stream, &token, handler.as_ref(), &cfg).await {
                tracing::debug!(error = %e, "browser bridge: connection ended");
            }
        });
    }
}

async fn send_json<T: Serialize>(s: &mut BoxStream, v: &T) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(v).map_err(|e| FrameError::Io(std::io::Error::other(e)))?;
    write_frame(s, &bytes, MAX_IPC_MESSAGE).await
}

async fn recv_json<T: for<'de> Deserialize<'de>>(s: &mut BoxStream) -> Result<Option<T>, String> {
    match read_frame(s, MAX_IPC_MESSAGE).await {
        Ok(Some(b)) => serde_json::from_slice(&b).map(Some).map_err(|_| "malformed message".to_string()),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

async fn connection<H: Handler>(mut s: BoxStream, token: &Token, handler: &H, cfg: &ServerConfig) -> Result<(), String> {
    let caller = tokio::time::timeout(cfg.handshake_timeout, handshake(&mut s, token, cfg))
        .await
        .map_err(|_| "handshake timed out".to_string())??;
    handler.contacted(&caller);

    let (max, window) = cfg.rate_limit;
    let mut recent: VecDeque<Instant> = VecDeque::new();
    loop {
        let req: Request = match recv_json(&mut s).await? {
            Some(r) => r,
            None => return Ok(()),
        };
        let now = Instant::now();
        while recent.front().is_some_and(|t| now.duration_since(*t) > window) {
            recent.pop_front();
        }
        let reply = if recent.len() >= max {
            Reply::err("rate_limited", "too many requests; slow down")
        } else {
            recent.push_back(now);
            match parse_value(req.message) {
                Err(e) => Reply::from_protocol(&e),
                Ok(Message::Ping) => Reply::ok(serde_json::json!({
                    "app": "VeloxFetch",
                    "version": handler.app_version(),
                    "protocol": PROTOCOL_VERSION,
                    "videoBar": handler.video_bar(),
                })),
                Ok(Message::Download(d)) => match handler.download(d, caller.clone()).await {
                    Ok(a) => Reply::ok(serde_json::json!({ "id": a.id, "filename": a.filename })),
                    Err(e) => Reply::err(e.code, &e.message),
                },
            }
        };
        send_json(&mut s, &Response { id: req.id, reply }).await.map_err(|e| e.to_string())?;
    }
}

async fn handshake(s: &mut BoxStream, token: &Token, cfg: &ServerConfig) -> Result<Caller, String> {
    let server_nonce = random_bytes::<NONCE_LEN>();
    send_json(s, &Hello { hello: "velox-bridge".into(), version: PROTOCOL_VERSION, nonce: to_hex(&server_nonce) })
        .await
        .map_err(|e| e.to_string())?;
    let auth: Auth = recv_json(s).await?.ok_or("closed during handshake")?;

    let fail = |why: &str| AuthResult { ok: false, proof: None, error: Some(why.to_string()) };
    let client_nonce = from_hex(&auth.nonce).filter(|n| n.len() == NONCE_LEN);
    let proof = from_hex(&auth.proof);
    let (Some(client_nonce), Some(proof)) = (client_nonce, proof) else {
        let _ = send_json(s, &fail("bad handshake")).await;
        return Err("bad handshake".into());
    };
    if !token.verify("client", &server_nonce, &client_nonce, &proof) {
        let _ = send_json(s, &fail("authentication failed")).await;
        return Err("authentication failed".into());
    }
    let caller = match Caller::from_origin(&auth.caller).and_then(|c| c.check(&cfg.allow).map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            let _ = send_json(s, &fail("caller not allowed")).await;
            return Err(format!("caller rejected: {e}"));
        }
    };
    let ours = token.prove("server", &server_nonce, &client_nonce);
    send_json(s, &AuthResult { ok: true, proof: Some(to_hex(&ours)), error: None }).await.map_err(|e| e.to_string())?;
    Ok(caller)
}
