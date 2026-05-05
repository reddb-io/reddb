//! RedWire native client.
//!
//! Speaks the binary frame protocol defined by ADR 0001 directly
//! over TCP — no engine, no tonic, no HTTP. The codec and frame
//! types come from [`reddb_wire::redwire`]; this module handles
//! the connect + handshake + per-request frame exchange.
//!
//! Auth: anonymous and bearer for now. SCRAM-SHA-256, OAuth/JWT,
//! and mTLS are tracked as follow-up work in the parent issue.
//!
//! TLS (`reds://`) is not yet implemented in this slice — see the
//! `TlsNotImplemented` error variant. The plain TCP path is the
//! primary deliverable.

use std::fmt;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use reddb_wire::redwire::{
    decode_frame, encode_frame, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};

#[derive(Debug, Clone)]
pub enum Auth {
    Anonymous,
    Bearer(String),
}

#[derive(Debug)]
pub enum RedWireError {
    Network(String),
    Protocol(String),
    AuthRefused(String),
    Engine(String),
    TlsNotImplemented,
}

impl fmt::Display for RedWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "network: {m}"),
            Self::Protocol(m) => write!(f, "protocol: {m}"),
            Self::AuthRefused(m) => write!(f, "auth refused: {m}"),
            Self::Engine(m) => write!(f, "engine error: {m}"),
            Self::TlsNotImplemented => write!(
                f,
                "RedWire-over-TLS (reds://) is not yet wired through red_client; \
                 use red:// (plain) or the full `red` binary for now"
            ),
        }
    }
}

impl std::error::Error for RedWireError {}

type Result<T> = std::result::Result<T, RedWireError>;

pub struct RedWireClient {
    stream: TcpStream,
    next_corr: u64,
    #[allow(dead_code)]
    session_id: String,
}

impl RedWireClient {
    pub async fn connect(host: &str, port: u16, tls: bool, auth: Auth) -> Result<Self> {
        if tls {
            return Err(RedWireError::TlsNotImplemented);
        }
        let addr = format!("{host}:{port}");
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| RedWireError::Network(format!("{addr}: {e}")))?;
        // Magic discriminator + supported minor version.
        stream
            .write_all(&[REDWIRE_MAGIC, MAX_KNOWN_MINOR_VERSION])
            .await
            .map_err(|e| RedWireError::Network(e.to_string()))?;
        let session_id = handshake(&mut stream, &auth).await?;
        Ok(Self {
            stream,
            next_corr: 1,
            session_id,
        })
    }

    pub async fn query(&mut self, sql: &str) -> Result<String> {
        let corr = self.next_corr_id();
        let frame = Frame::new(MessageKind::Query, corr, sql.as_bytes().to_vec());
        self.stream
            .write_all(&encode_frame(&frame))
            .await
            .map_err(|e| RedWireError::Network(e.to_string()))?;
        let resp = read_frame(&mut self.stream).await?;
        match resp.kind {
            MessageKind::Result => Ok(String::from_utf8_lossy(&resp.payload).to_string()),
            MessageKind::Error => Err(RedWireError::Engine(
                String::from_utf8_lossy(&resp.payload).to_string(),
            )),
            other => Err(RedWireError::Protocol(format!(
                "expected Result/Error, got {other:?}"
            ))),
        }
    }

    fn next_corr_id(&mut self) -> u64 {
        let n = self.next_corr;
        self.next_corr = self.next_corr.wrapping_add(1);
        n
    }
}

async fn handshake(stream: &mut TcpStream, auth: &Auth) -> Result<String> {
    let methods: Vec<&str> = match auth {
        Auth::Bearer(_) => vec!["bearer"],
        Auth::Anonymous => vec!["anonymous", "bearer"],
    };
    let mut hello_obj = serde_json::Map::new();
    hello_obj.insert(
        "versions".into(),
        serde_json::Value::Array(vec![serde_json::Value::Number(serde_json::Number::from(
            MAX_KNOWN_MINOR_VERSION,
        ))]),
    );
    hello_obj.insert(
        "auth_methods".into(),
        serde_json::Value::Array(
            methods
                .iter()
                .map(|s| serde_json::Value::String((*s).to_string()))
                .collect(),
        ),
    );
    hello_obj.insert(
        "features".into(),
        serde_json::Value::Number(serde_json::Number::from(0u32)),
    );
    let hello_bytes = serde_json::to_vec(&serde_json::Value::Object(hello_obj))
        .map_err(|e| RedWireError::Protocol(format!("encode hello: {e}")))?;
    let hello = Frame::new(MessageKind::Hello, 1, hello_bytes);
    stream
        .write_all(&encode_frame(&hello))
        .await
        .map_err(|e| RedWireError::Network(e.to_string()))?;

    let ack = read_frame(stream).await?;
    let chosen = match ack.kind {
        MessageKind::HelloAck => parse_chosen_auth(&ack.payload)?,
        MessageKind::AuthFail => {
            return Err(RedWireError::AuthRefused(
                parse_reason(&ack.payload).unwrap_or_else(|| "AuthFail at HelloAck".into()),
            ));
        }
        other => {
            return Err(RedWireError::Protocol(format!(
                "expected HelloAck, got {other:?}"
            )));
        }
    };

    let resp_payload = match (chosen.as_str(), auth) {
        ("anonymous", _) => Vec::new(),
        ("bearer", Auth::Bearer(token)) => {
            let mut obj = serde_json::Map::new();
            obj.insert("token".into(), serde_json::Value::String(token.clone()));
            serde_json::to_vec(&serde_json::Value::Object(obj))
                .map_err(|e| RedWireError::Protocol(format!("encode auth: {e}")))?
        }
        ("bearer", Auth::Anonymous) => {
            return Err(RedWireError::AuthRefused(
                "server demands bearer auth but no token was supplied".into(),
            ));
        }
        (other, _) => {
            return Err(RedWireError::Protocol(format!(
                "server picked unsupported auth method: {other}"
            )));
        }
    };
    let resp = Frame::new(MessageKind::AuthResponse, 2, resp_payload);
    stream
        .write_all(&encode_frame(&resp))
        .await
        .map_err(|e| RedWireError::Network(e.to_string()))?;

    let final_frame = read_frame(stream).await?;
    match final_frame.kind {
        MessageKind::AuthOk => {
            let parsed: serde_json::Value = serde_json::from_slice(&final_frame.payload)
                .map_err(|e| RedWireError::Protocol(format!("decode auth_ok: {e}")))?;
            let session_id = parsed
                .as_object()
                .and_then(|o| o.get("session_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(session_id)
        }
        MessageKind::AuthFail => Err(RedWireError::AuthRefused(
            parse_reason(&final_frame.payload).unwrap_or_else(|| "auth refused".into()),
        )),
        other => Err(RedWireError::Protocol(format!(
            "expected AuthOk/AuthFail, got {other:?}"
        ))),
    }
}

async fn read_frame(stream: &mut TcpStream) -> Result<Frame> {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| RedWireError::Network(e.to_string()))?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        stream
            .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
            .await
            .map_err(|e| RedWireError::Network(e.to_string()))?;
    }
    let (frame, _) = decode_frame(&buf)
        .map_err(|e| RedWireError::Protocol(format!("decode: {e}")))?;
    Ok(frame)
}

fn parse_chosen_auth(payload: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| RedWireError::Protocol(format!("decode hello_ack: {e}")))?;
    v.as_object()
        .and_then(|o| o.get("auth"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| RedWireError::Protocol("hello_ack missing auth field".into()))
}

fn parse_reason(payload: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.as_object()?.get("reason")?.as_str().map(String::from)
}
