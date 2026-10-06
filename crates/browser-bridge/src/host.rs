//! The native messaging host loop: what runs when a browser starts `velox-native-host`.
//!
//! ```text
//! browser stdin --frame--> validate --> app link --> reply --frame--> browser stdout
//! ```
//! stdout is the protocol channel: nothing else may ever be written to it.
//!
//! Every message is validated here *and* again by the app. If the app is not running the host can
//! start it (a fixed executable next to the host, never a path taken from a message), but only
//! for a real `download` request, never for a `ping`.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::auth::Token;
use crate::caller::Caller;
use crate::client::{Client, ClientError};
use crate::frame::{read_frame, write_frame, FrameError, MAX_BROWSER_MESSAGE, MAX_REPLY};
use crate::protocol::{parse_message, Message, Reply};
use crate::transport::Endpoint;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub endpoint: Endpoint,
    /// Where the app keeps its token file.
    pub data_dir: PathBuf,
    /// The desktop app to start when it is not running (`None` = never start it).
    pub app_exe: Option<PathBuf>,
    /// How long to wait for a freshly started app to come up.
    pub launch_wait: Duration,
}

struct Link<'a> {
    cfg: &'a HostConfig,
    caller: &'a Caller,
    client: Option<Client>,
    launched: bool,
}

fn launch(exe: &std::path::Path) -> io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Outlive the browser and the host; if the browser's job forbids breakaway, fall back.
        if cmd.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(DETACHED_PROCESS).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    cmd.spawn().map(|_| ())
}

impl Link<'_> {
    async fn try_connect(&self) -> Result<Client, ClientError> {
        // Re-read the token every time: a restarted app writes a new one.
        let token = Token::read_file(&self.cfg.data_dir).map_err(|_| ClientError::Unavailable)?;
        Client::connect(&self.cfg.endpoint, &token, self.caller).await
    }

    /// A connected client, starting the app first if allowed and needed.
    async fn client(&mut self, may_launch: bool) -> Result<&mut Client, Reply> {
        if self.client.is_none() {
            let c = match self.try_connect().await {
                Ok(c) => c,
                Err(ClientError::Unavailable)
                    if may_launch && !self.launched && self.cfg.app_exe.as_deref().is_some_and(|p| p.is_file()) =>
                {
                    self.launched = true;
                    let exe = self.cfg.app_exe.clone().unwrap();
                    launch(&exe).map_err(|_| Reply::err("app_not_running", "VeloxFetch is not running and could not be started"))?;
                    let deadline = tokio::time::Instant::now() + self.cfg.launch_wait;
                    loop {
                        match self.try_connect().await {
                            Ok(c) => break c,
                            Err(_) if tokio::time::Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(250)).await,
                            Err(_) => return Err(Reply::err("app_not_running", "VeloxFetch did not start in time")),
                        }
                    }
                }
                Err(ClientError::Unavailable) => return Err(Reply::err("app_not_running", "VeloxFetch is not running")),
                Err(ClientError::AuthFailed) => return Err(Reply::err("auth_failed", "could not authenticate with VeloxFetch")),
                Err(_) => return Err(Reply::err("link_error", "could not talk to VeloxFetch")),
            };
            self.client = Some(c);
        }
        Ok(self.client.as_mut().expect("just connected"))
    }

    async fn forward(&mut self, value: serde_json::Value, may_launch: bool) -> Reply {
        // One reconnect if the app was restarted since the last message.
        for attempt in 0..2 {
            let client = match self.client(may_launch).await {
                Ok(c) => c,
                Err(r) => return r,
            };
            match client.request(value.clone()).await {
                Ok(r) => return r,
                Err(_) if attempt == 0 => self.client = None,
                Err(_) => return Reply::err("link_error", "lost the connection to VeloxFetch"),
            }
        }
        Reply::err("link_error", "lost the connection to VeloxFetch")
    }
}

async fn reply<W: AsyncWrite + Unpin>(out: &mut W, r: &Reply) -> Result<(), FrameError> {
    write_frame(out, &r.to_bytes(), MAX_REPLY).await
}

/// Serve one browser connection until it closes. Returns when the browser closes stdin, after an
/// oversized message (the stream is out of sync), or when the caller is not allowed.
pub async fn run<R, W>(input: &mut R, output: &mut W, caller: Caller, cfg: &HostConfig) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut link = Link { cfg, caller: &caller, client: None, launched: false };
    loop {
        let bytes = match read_frame(input, MAX_BROWSER_MESSAGE).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()),
            Err(FrameError::TooLarge { .. }) => {
                let _ = reply(output, &Reply::err("too_large", "message is too large")).await;
                return Ok(()); // cannot resynchronise after an unread body: end the session
            }
            Err(FrameError::Truncated) => return Ok(()),
            Err(FrameError::Io(e)) => return Err(e),
        };
        let out = match parse_message(&bytes) {
            Err(e) => Reply::from_protocol(&e),
            Ok(msg) => {
                let may_launch = matches!(msg, Message::Download(_));
                // Forward the validated original; the app validates it again.
                match serde_json::from_slice(&bytes) {
                    Ok(v) => link.forward(v, may_launch).await,
                    Err(_) => Reply::err("bad_json", "the message is not valid JSON"),
                }
            }
        };
        if reply(output, &out).await.is_err() {
            return Ok(()); // the browser went away
        }
    }
}
