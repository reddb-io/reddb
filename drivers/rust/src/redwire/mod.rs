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
#[cfg(feature = "redwire-tls")]
mod tls;

pub use codec::FrameError;
pub use frame::{Flags, Frame, MessageKind};
#[cfg(feature = "redwire-tls")]
pub use tls::TlsConfig;

use std::io;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Boxed read+write trait object. Lets `RedWireClient` carry
/// either a plain `TcpStream` or a `tokio_rustls::TlsStream`
/// without leaking the type up.
pub(crate) type Stream = Pin<Box<dyn AsyncReadWrite + Send + Unpin>>;

pub(crate) trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + ?Sized> AsyncReadWrite for T {}

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

/// Typed value for the binary bulk-insert fast path. Tag bytes
/// match `src/wire/protocol.rs` so the wire shape is identical to
/// what `examples/stress_wire_client.rs` already produces.
#[derive(Debug, Clone)]
pub enum BinaryValue {
    I64(i64),
    F64(f64),
    Text(String),
    Bool(bool),
    Null,
}

impl BinaryValue {
    /// Tag constants — keep in sync with the engine's
    /// `wire::protocol::VAL_*` table.
    const TAG_I64: u8 = 1;
    const TAG_F64: u8 = 2;
    const TAG_TEXT: u8 = 3;
    const TAG_BOOL: u8 = 4;
    const TAG_NULL: u8 = 0;

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            Self::I64(n) => {
                buf.push(Self::TAG_I64);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Self::F64(n) => {
                buf.push(Self::TAG_F64);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Self::Text(s) => {
                buf.push(Self::TAG_TEXT);
                buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
            Self::Bool(b) => {
                buf.push(Self::TAG_BOOL);
                buf.push(if *b { 1 } else { 0 });
            }
            Self::Null => buf.push(Self::TAG_NULL),
        }
    }
}

/// Configuration for `RedWireClient::connect`.
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub auth: Auth,
    pub client_name: Option<String>,
    #[cfg(feature = "redwire-tls")]
    pub tls: Option<TlsConfig>,
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("ConnectOptions");
        s.field("host", &self.host)
            .field("port", &self.port)
            .field("auth", &self.auth)
            .field("client_name", &self.client_name);
        #[cfg(feature = "redwire-tls")]
        s.field("tls", &self.tls.is_some());
        s.finish()
    }
}

impl Clone for ConnectOptions {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            auth: self.auth.clone(),
            client_name: self.client_name.clone(),
            #[cfg(feature = "redwire-tls")]
            tls: self.tls.clone(),
        }
    }
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            auth: Auth::Anonymous,
            client_name: Some(format!("reddb-rs/{}", env!("CARGO_PKG_VERSION"))),
            #[cfg(feature = "redwire-tls")]
            tls: None,
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

    /// Wrap the TCP socket in TLS using the supplied config.
    #[cfg(feature = "redwire-tls")]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }
}

pub struct RedWireClient {
    stream: Stream,
    next_correlation_id: u64,
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    server_features: u32,
}

impl std::fmt::Debug for RedWireClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedWireClient")
            .field("session_id", &self.session_id)
            .field("server_features", &self.server_features)
            .finish()
    }
}

impl RedWireClient {
    pub async fn connect(opts: ConnectOptions) -> Result<Self> {
        let addr = format!("{}:{}", opts.host, opts.port);
        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| ClientError::new(ErrorCode::Network, format!("{addr}: {e}")))?;

        let mut stream: Stream = match () {
            #[cfg(feature = "redwire-tls")]
            _ if opts.tls.is_some() => {
                let tls_cfg = opts.tls.as_ref().unwrap();
                let tls_stream = tls::wrap_client(tcp, &opts.host, tls_cfg).await?;
                Box::pin(tls_stream)
            }
            _ => Box::pin(tcp),
        };

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

    /// Insert a single row. `payload` is a JSON object with column
    /// → value pairs. Returns the engine's affected-rows count.
    pub async fn insert(&mut self, collection: &str, payload: serde_json::Value) -> Result<u64> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "collection".into(),
            serde_json::Value::String(collection.to_string()),
        );
        obj.insert("payload".into(), payload);
        self.send_insert_frame(serde_json::Value::Object(obj)).await
    }

    /// Bulk insert. Each entry in `payloads` is a JSON object.
    pub async fn bulk_insert(
        &mut self,
        collection: &str,
        payloads: Vec<serde_json::Value>,
    ) -> Result<u64> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "collection".into(),
            serde_json::Value::String(collection.to_string()),
        );
        obj.insert("payloads".into(), serde_json::Value::Array(payloads));
        self.send_insert_frame(serde_json::Value::Object(obj)).await
    }

    async fn send_insert_frame(&mut self, body: serde_json::Value) -> Result<u64> {
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode insert: {e}")))?;
        let corr = self.next_corr();
        let req = Frame::new(MessageKind::BulkInsert, corr, bytes);
        self.stream
            .write_all(&encode_frame(&req))
            .await
            .map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::BulkOk => {
                let v: serde_json::Value = serde_json::from_slice(&resp.payload)
                    .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode bulk_ok: {e}")))?;
                let affected = v
                    .as_object()
                    .and_then(|o| o.get("affected"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                Ok(affected)
            }
            MessageKind::Error => {
                let msg = String::from_utf8_lossy(&resp.payload).to_string();
                Err(ClientError::new(ErrorCode::Engine, msg))
            }
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected BulkOk/Error, got {other:?}"),
            )),
        }
    }

    /// Fetch one row by primary id. Returns the JSON envelope the
    /// server emits on a `Get` frame: `{ ok, found, ... }`.
    pub async fn get(&mut self, collection: &str, id: &str) -> Result<serde_json::Value> {
        let mut obj = serde_json::Map::new();
        obj.insert("collection".into(), serde_json::Value::String(collection.to_string()));
        obj.insert("id".into(), serde_json::Value::String(id.to_string()));
        let bytes = serde_json::to_vec(&serde_json::Value::Object(obj))
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode get: {e}")))?;
        let corr = self.next_corr();
        let req = Frame::new(MessageKind::Get, corr, bytes);
        self.stream.write_all(&encode_frame(&req)).await.map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Result => serde_json::from_slice(&resp.payload)
                .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode get: {e}"))),
            MessageKind::Error => Err(ClientError::new(
                ErrorCode::Engine,
                String::from_utf8_lossy(&resp.payload).to_string(),
            )),
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected Result/Error, got {other:?}"),
            )),
        }
    }

    /// Delete by primary id. Returns the affected count.
    pub async fn delete(&mut self, collection: &str, id: &str) -> Result<u64> {
        let mut obj = serde_json::Map::new();
        obj.insert("collection".into(), serde_json::Value::String(collection.to_string()));
        obj.insert("id".into(), serde_json::Value::String(id.to_string()));
        let bytes = serde_json::to_vec(&serde_json::Value::Object(obj))
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode delete: {e}")))?;
        let corr = self.next_corr();
        let req = Frame::new(MessageKind::Delete, corr, bytes);
        self.stream.write_all(&encode_frame(&req)).await.map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::DeleteOk => {
                let v: serde_json::Value = serde_json::from_slice(&resp.payload)
                    .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode delete_ok: {e}")))?;
                Ok(v.as_object()
                    .and_then(|o| o.get("affected"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0))
            }
            MessageKind::Error => Err(ClientError::new(
                ErrorCode::Engine,
                String::from_utf8_lossy(&resp.payload).to_string(),
            )),
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected DeleteOk/Error, got {other:?}"),
            )),
        }
    }

    /// Bulk-insert via the v1 binary fast path. Same wire shape as
    /// `MSG_BULK_INSERT_BINARY` (0x06): typed values, no JSON
    /// encode/decode. Use this for hot inserts where the column
    /// types are known up front.
    ///
    /// `columns`: column names in fixed order.
    /// `rows`: each inner Vec must have one entry per column, in
    /// column order. Mixed types are encoded by the value writer.
    pub async fn bulk_insert_binary(
        &mut self,
        collection: &str,
        columns: &[&str],
        rows: &[Vec<BinaryValue>],
    ) -> Result<u64> {
        let mut payload = Vec::with_capacity(64 + rows.len() * columns.len() * 16);
        // Collection name
        payload.extend_from_slice(&(collection.len() as u16).to_le_bytes());
        payload.extend_from_slice(collection.as_bytes());
        // Column count + names
        payload.extend_from_slice(&(columns.len() as u16).to_le_bytes());
        for c in columns {
            payload.extend_from_slice(&(c.len() as u16).to_le_bytes());
            payload.extend_from_slice(c.as_bytes());
        }
        // Row count + rows
        payload.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for row in rows {
            if row.len() != columns.len() {
                return Err(ClientError::new(
                    ErrorCode::Protocol,
                    format!(
                        "row had {} values for {} columns",
                        row.len(),
                        columns.len()
                    ),
                ));
            }
            for v in row {
                v.encode(&mut payload);
            }
        }

        let corr = self.next_corr();
        let req = Frame::new(MessageKind::BulkInsertBinary, corr, payload);
        self.stream
            .write_all(&encode_frame(&req))
            .await
            .map_err(io_err)?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::BulkOk => {
                if resp.payload.len() < 8 {
                    return Err(ClientError::new(
                        ErrorCode::Protocol,
                        "BulkOk truncated: expected 8-byte count",
                    ));
                }
                Ok(u64::from_le_bytes([
                    resp.payload[0],
                    resp.payload[1],
                    resp.payload[2],
                    resp.payload[3],
                    resp.payload[4],
                    resp.payload[5],
                    resp.payload[6],
                    resp.payload[7],
                ]))
            }
            MessageKind::Error => Err(ClientError::new(
                ErrorCode::Engine,
                String::from_utf8_lossy(&resp.payload).to_string(),
            )),
            other => Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected BulkOk/Error, got {other:?}"),
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
