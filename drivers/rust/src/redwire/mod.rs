//! RedWire v2 client.
//!
//! Mirrors the server-side codec (`reddb::wire::redwire`) but
//! lives in the driver crate so the client doesn't drag the
//! engine in. The framing is a stable wire contract — both sides
//! re-implement it from the same ADR (`docs/adr/0001-redwire-tcp-protocol.md`).
//!
//! Public surface:
//!   - [`RedWireClient::connect`]: TCP + handshake + auth
//!   - [`RedWireClient::query`]: SQL → server result
//!   - [`RedWireClient::ping`]: keepalive
//!   - [`RedWireClient::close`]: clean shutdown via Bye

mod codec;
mod frame;
mod handshake;

pub use codec::FrameError;
pub use frame::{Flags, Frame, MessageKind};

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::QueryResult;

use codec::{decode_frame, encode_frame};
use frame::FRAME_HEADER_SIZE;
use handshake::HandshakeOutcome;

/// Magic byte that identifies a v2 connection on the shared port.
pub const MAGIC: u8 = 0xFE;

/// Highest minor protocol version this client implements.
pub const SUPPORTED_VERSION: u8 = 0x01;

/// Authentication credentials for the v2 handshake.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Server is configured with `auth.enabled = false`.
    Anonymous,
    /// Bearer token (login-derived session token or API key).
    Bearer(String),
}

/// Configuration for `RedWireClient::connect`.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub auth: Auth,
    pub client_name: Option<String>,
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            auth: Auth::Anonymous,
            client_name: Some(format!("reddb-rs/{}", env!("CARGO_PKG_VERSION"))),
        }
    }

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = Some(name.into());
        self
    }
}

#[derive(Debug)]
pub struct RedWireClient {
    stream: TcpStream,
    next_correlation_id: u64,
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    server_features: u32,
}

impl RedWireClient {
    pub async fn connect(opts: ConnectOptions) -> Result<Self> {
        let addr = format!("{}:{}", opts.host, opts.port);
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| ClientError::new(ErrorCode::Network, format!("{addr}: {e}")))?;

        // Discriminator + minor-version byte.
        stream
            .write_all(&[MAGIC, SUPPORTED_VERSION])
            .await
            .map_err(io_err)?;

        let outcome = handshake::run(&mut stream, &opts).await?;
        match outcome {
            HandshakeOutcome::Authenticated {
                session_id,
                server_features,
            } => Ok(Self {
                stream,
                next_correlation_id: 1,
                session_id,
                server_features,
            }),
            HandshakeOutcome::Refused(reason) => Err(ClientError::new(
                ErrorCode::AuthRefused,
                format!("redwire auth refused: {reason}"),
            )),
        }
    }

    pub async fn query(&mut self, sql: &str) -> Result<QueryResult> {
        let corr = self.next_corr();
        let req = Frame::new(MessageKind::Query, corr, sql.as_bytes().to_vec());
        self.stream
            .write_all(&encode_frame(&req))
            .await
            .map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Result => {
                let value: serde_json::Value = serde_json::from_slice(&resp.payload)
                    .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode result: {e}")))?;
                Ok(QueryResult::from_envelope(value))
            }
            MessageKind::Error => {
                let msg = String::from_utf8_lossy(&resp.payload).to_string();
                Err(ClientError::new(ErrorCode::Engine, msg))
            }
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected Result/Error, got {other:?}"),
            )),
        }
    }

    pub async fn ping(&mut self) -> Result<()> {
        let corr = self.next_corr();
        let req = Frame::new(MessageKind::Ping, corr, vec![]);
        self.stream
            .write_all(&encode_frame(&req))
            .await
            .map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Pong => Ok(()),
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected Pong, got {other:?}"),
            )),
        }
    }

    pub async fn close(mut self) -> Result<()> {
        let corr = self.next_corr();
        let bye = Frame::new(MessageKind::Bye, corr, vec![]);
        let _ = self.stream.write_all(&encode_frame(&bye)).await;
        Ok(())
    }

    fn next_corr(&mut self) -> u64 {
        let c = self.next_correlation_id;
        self.next_correlation_id = self.next_correlation_id.wrapping_add(1);
        c
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        let mut header = [0u8; FRAME_HEADER_SIZE];
        self.stream.read_exact(&mut header).await.map_err(io_err)?;
        let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if length < FRAME_HEADER_SIZE {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                format!("server sent a frame with length {length}"),
            ));
        }
        let mut buf = vec![0u8; length];
        buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
        if length > FRAME_HEADER_SIZE {
            self.stream
                .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
                .await
                .map_err(io_err)?;
        }
        let (frame, _) = decode_frame(&buf)
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode frame: {e}")))?;
        Ok(frame)
    }
}

fn io_err(err: io::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, err.to_string())
}
