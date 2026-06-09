//! RedWire client.
//!
//! Uses the canonical RedWire contracts from `reddb-wire` so the
//! client does not duplicate the engine-facing frame and payload
//! definitions. The framing is a stable wire contract defined by
//! ADR 0001 (`.red/adr/0001-redwire-tcp-protocol.md`).
//!
//! Public surface:
//!   - [`RedWireClient::connect`][]: TCP + handshake + auth
//!   - [`RedWireClient::query`][]: SQL → server result
//!   - [`RedWireClient::ping`][]: keepalive
//!   - [`RedWireClient::close`][]: clean shutdown via Bye

mod handshake;
mod io;
#[cfg(feature = "redwire")]
pub mod scram;
#[cfg(feature = "redwire-tls")]
mod tls;

pub use reddb_wire::redwire::{Flags, Frame, FrameError, MessageKind};
#[cfg(feature = "redwire-tls")]
pub use tls::TlsConfig;

use std::io as std_io;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use reddb_wire::query_with_params::{
    encode_query_with_params, ParamValue as RedWireParamValue, FEATURE_PARAMS,
};

/// Boxed read+write trait object. Lets `RedWireClient` carry
/// either a plain `TcpStream` or a `tokio_rustls::TlsStream`
/// without leaking the type up.
pub(crate) type Stream = Pin<Box<dyn AsyncReadWrite + Send + Unpin>>;

pub(crate) trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + ?Sized> AsyncReadWrite for T {}

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::{BulkInsertResult, QueryResult};

use handshake::HandshakeOutcome;
use reddb_wire::legacy::WireValue;
use reddb_wire::redwire::{
    build_bulk_insert_binary_frame, build_bulk_insert_frame, build_bye_frame, build_delete_frame,
    build_get_frame, build_ping_frame, build_query_frame, build_query_with_params_frame,
    decode_bulk_ok_count_payload, decode_bulk_ok_payload, decode_delete_ok_affected,
    decode_get_result_payload, decode_query_result_payload, encode_bulk_binary_payload,
    encode_bulk_insert_payload, encode_insert_payload, encode_key_payload,
    supported_client_preface, BuildError,
};

/// Authentication credentials for the RedWire handshake.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Server is configured with `auth.enabled = false`.
    Anonymous,
    /// Bearer token (login-derived session token or API key).
    Bearer(String),
}

/// Typed value for the binary bulk-insert fast path. Encoding delegates
/// to `reddb-wire::legacy`, which owns the byte-level value tags.
#[derive(Debug, Clone)]
pub enum BinaryValue {
    I64(i64),
    F64(f64),
    Text(String),
    Bool(bool),
    Null,
}

impl From<&BinaryValue> for WireValue {
    fn from(value: &BinaryValue) -> Self {
        match value {
            BinaryValue::I64(n) => WireValue::I64(*n),
            BinaryValue::F64(n) => WireValue::F64(*n),
            BinaryValue::Text(value) => WireValue::Text(value.clone()),
            BinaryValue::Bool(value) => WireValue::Bool(*value),
            BinaryValue::Null => WireValue::Null,
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
            .write_all(&supported_client_preface())
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
        let raw = self.query_raw(sql).await?;
        let value = decode_query_result_payload(raw.as_bytes())
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode result: {e}")))?;
        Ok(QueryResult::from_envelope(value))
    }

    /// Send a query and return the raw `Result` payload string. This is
    /// used by the legacy `red_client` compatibility shim so the CLI
    /// output stays byte-for-byte aligned with its old RedWire path.
    pub async fn query_raw(&mut self, sql: &str) -> Result<String> {
        let corr = self.next_corr();
        let req = build_query_frame(corr, sql).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Result => Ok(String::from_utf8_lossy(&resp.payload).to_string()),
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

    pub async fn query_with(
        &mut self,
        sql: &str,
        params: &[crate::params::Value],
    ) -> Result<QueryResult> {
        if params.is_empty() {
            return self.query(sql).await;
        }
        if self.server_features & FEATURE_PARAMS == 0 {
            return Err(ClientError::new(
                ErrorCode::ParamsUnsupported,
                "server did not advertise RedWire parameter support",
            ));
        }

        let wire_params = params.iter().map(param_to_redwire).collect::<Vec<_>>();
        let payload = encode_query_with_params(sql, &wire_params)
            .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode params: {e}")))?;
        let corr = self.next_corr();
        let req = build_query_with_params_frame(corr, payload).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Result => {
                let value = decode_query_result_payload(&resp.payload).map_err(|e| {
                    ClientError::new(ErrorCode::Protocol, format!("decode result: {e}"))
                })?;
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
        self.send_insert_frame(encode_insert_payload(collection, payload))
            .await
            .map(|result| result.affected)
    }

    /// Bulk insert. Each entry in `payloads` is a JSON object.
    pub async fn bulk_insert(
        &mut self,
        collection: &str,
        payloads: Vec<serde_json::Value>,
    ) -> Result<BulkInsertResult> {
        self.send_insert_frame(encode_bulk_insert_payload(collection, payloads))
            .await
    }

    async fn send_insert_frame(&mut self, bytes: Vec<u8>) -> Result<BulkInsertResult> {
        let corr = self.next_corr();
        let req = build_bulk_insert_frame(corr, bytes).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::BulkOk => {
                let payload = decode_bulk_ok_payload(&resp.payload).map_err(|e| {
                    ClientError::new(ErrorCode::Protocol, format!("decode bulk_ok: {e}"))
                })?;
                Ok(BulkInsertResult {
                    affected: payload.affected,
                    rids: payload.rids,
                    ids: payload.ids,
                })
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
        let corr = self.next_corr();
        let req =
            build_get_frame(corr, encode_key_payload(collection, id)).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::Result => decode_get_result_payload(&resp.payload)
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
        let corr = self.next_corr();
        let req = build_delete_frame(corr, encode_key_payload(collection, id))
            .map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::DeleteOk => decode_delete_ok_affected(&resp.payload).map_err(|e| {
                ClientError::new(ErrorCode::Protocol, format!("decode delete_ok: {e}"))
            }),
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

    /// Bulk-insert via the binary fast path. Same wire shape as
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
        let wire_rows = rows
            .iter()
            .map(|row| row.iter().map(WireValue::from).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let payload = encode_bulk_binary_payload(collection, columns, &wire_rows)
            .map_err(|e| ClientError::new(ErrorCode::Protocol, e.to_string()))?;

        let corr = self.next_corr();
        let req = build_bulk_insert_binary_frame(corr, payload).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
        let resp = self.read_frame().await?;
        match resp.kind {
            MessageKind::BulkOk => decode_bulk_ok_count_payload(&resp.payload)
                .map_err(|e| ClientError::new(ErrorCode::Protocol, e.to_string())),
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
        let req = build_ping_frame(corr).map_err(frame_build_err)?;
        io::write_frame(&mut self.stream, &req).await?;
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
        let bye = build_bye_frame(corr).map_err(frame_build_err)?;
        let _ = io::write_frame(&mut self.stream, &bye).await;
        Ok(())
    }

    fn next_corr(&mut self) -> u64 {
        let c = self.next_correlation_id;
        self.next_correlation_id = self.next_correlation_id.wrapping_add(1);
        c
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        io::read_frame(&mut self.stream).await
    }
}

fn param_to_redwire(value: &crate::params::Value) -> RedWireParamValue {
    match value {
        crate::params::Value::Null => RedWireParamValue::Null,
        crate::params::Value::Bool(value) => RedWireParamValue::Bool(*value),
        crate::params::Value::Int(value) => RedWireParamValue::Int(*value),
        crate::params::Value::Float(value) => RedWireParamValue::Float(*value),
        crate::params::Value::Text(value) => RedWireParamValue::Text(value.clone()),
        crate::params::Value::Bytes(value) => RedWireParamValue::Bytes(value.clone()),
        crate::params::Value::Vector(value) => RedWireParamValue::Vector(value.clone()),
        crate::params::Value::Json(value) => {
            RedWireParamValue::Json(value.to_json_string().into_bytes())
        }
        crate::params::Value::Timestamp(value) => RedWireParamValue::Timestamp(*value),
        crate::params::Value::Uuid(value) => RedWireParamValue::Uuid(*value),
    }
}

fn io_err(err: std_io::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, err.to_string())
}

fn frame_build_err(err: BuildError) -> ClientError {
    ClientError::new(ErrorCode::Protocol, format!("build redwire frame: {err}"))
}

#[cfg(test)]
mod tests {
    use super::param_to_redwire;
    use crate::{JsonValue, Value};
    use reddb_wire::query_with_params::ParamValue as WireValue;

    #[test]
    fn param_to_redwire_preserves_all_wire_variants() {
        let uuid = [0x11; 16];
        let cases = vec![
            (Value::Null, WireValue::Null),
            (Value::Bool(true), WireValue::Bool(true)),
            (Value::Int64(42), WireValue::Int(42)),
            (Value::Float(1.5), WireValue::Float(1.5)),
            (Value::Text("Ada".into()), WireValue::Text("Ada".into())),
            (
                Value::Bytes(vec![0xde, 0xad]),
                WireValue::Bytes(vec![0xde, 0xad]),
            ),
            (
                Value::Vector(vec![0.25, 0.5]),
                WireValue::Vector(vec![0.25, 0.5]),
            ),
            (
                Value::Json(JsonValue::object([("role", JsonValue::string("admin"))])),
                WireValue::Json(br#"{"role":"admin"}"#.to_vec()),
            ),
            (
                Value::Timestamp(1_700_000_000),
                WireValue::Timestamp(1_700_000_000),
            ),
            (Value::Uuid(uuid), WireValue::Uuid(uuid)),
        ];

        for (input, expected) in cases {
            assert_eq!(param_to_redwire(&input), expected);
        }
    }
}
