//! The native host's side of the link to the app.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{from_hex, random_bytes, to_hex, Token, NONCE_LEN};
use crate::caller::Caller;
use crate::frame::{read_frame, write_frame, FrameError, MAX_IPC_MESSAGE};
use crate::protocol::Reply;
use crate::server::{Auth, AuthResult, Hello, Response, PROTOCOL_VERSION};
use crate::transport::{connect, BoxStream, Endpoint};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The app is not running (or its endpoint is not there).
    #[error("VeloxFetch is not running")]
    Unavailable,
    #[error("could not authenticate with VeloxFetch")]
    AuthFailed,
    #[error("VeloxFetch does not speak the same protocol")]
    Protocol,
    #[error("connection to VeloxFetch failed: {0}")]
    Frame(#[from] FrameError),
}

pub struct Client {
    stream: BoxStream,
    next_id: u64,
}

async fn send<T: Serialize>(s: &mut BoxStream, v: &T) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(v).map_err(|_| ClientError::Protocol)?;
    Ok(write_frame(s, &bytes, MAX_IPC_MESSAGE).await?)
}

async fn recv<T: for<'de> Deserialize<'de>>(s: &mut BoxStream) -> Result<T, ClientError> {
    let bytes = read_frame(s, MAX_IPC_MESSAGE).await?.ok_or(ClientError::Protocol)?;
    serde_json::from_slice(&bytes).map_err(|_| ClientError::Protocol)
}

impl Client {
    /// Connect and authenticate both ways. Fails (without revealing the token) if the peer is not
    /// the app that wrote the token file.
    pub async fn connect(ep: &Endpoint, token: &Token, caller: &Caller) -> Result<Self, ClientError> {
        let mut stream = connect(ep).await.map_err(|_| ClientError::Unavailable)?;
        let hello: Hello = recv(&mut stream).await?;
        if hello.hello != "velox-bridge" || hello.version != PROTOCOL_VERSION {
            return Err(ClientError::Protocol);
        }
        let server_nonce = from_hex(&hello.nonce).filter(|n| n.len() == NONCE_LEN).ok_or(ClientError::Protocol)?;
        let client_nonce = random_bytes::<NONCE_LEN>();
        send(
            &mut stream,
            &Auth {
                nonce: to_hex(&client_nonce),
                proof: to_hex(&token.prove("client", &server_nonce, &client_nonce)),
                caller: caller.origin(),
            },
        )
        .await?;
        let result: AuthResult = recv(&mut stream).await?;
        if !result.ok {
            return Err(ClientError::AuthFailed);
        }
        // The server must prove it knows the token too: otherwise we may be talking to an impostor.
        let proof = result.proof.as_deref().and_then(from_hex).ok_or(ClientError::AuthFailed)?;
        if !token.verify("server", &server_nonce, &client_nonce, &proof) {
            return Err(ClientError::AuthFailed);
        }
        Ok(Self { stream, next_id: 1 })
    }

    /// Send one (already validated) browser message and wait for the app's reply.
    pub async fn request(&mut self, message: Value) -> Result<Reply, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        send(&mut self.stream, &serde_json::json!({ "id": id, "message": message })).await?;
        let resp: Response = recv(&mut self.stream).await?;
        if resp.id != id {
            return Err(ClientError::Protocol);
        }
        Ok(resp.reply)
    }
}
