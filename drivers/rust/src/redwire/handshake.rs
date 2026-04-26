//! Client-side handshake — Hello → HelloAck → AuthResponse → AuthOk.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ClientError, ErrorCode, Result};

use super::codec::{decode_frame, encode_frame};
use super::frame::{Frame, MessageKind, FRAME_HEADER_SIZE};
use super::{Auth, ConnectOptions, SUPPORTED_VERSION};

#[derive(Debug)]
pub(super) enum HandshakeOutcome {
    Authenticated {
        session_id: String,
        server_features: u32,
    },
    Refused(String),
}

pub(super) async fn run<S>(stream: &mut S, opts: &ConnectOptions) -> Result<HandshakeOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 1. Send Hello.
    let methods: Vec<&str> = match &opts.auth {
        Auth::Bearer(_) => vec!["bearer"],
        Auth::Anonymous => vec!["anonymous", "bearer"],
    };
    let mut hello_obj = serde_json::Map::new();
    hello_obj.insert(
        "versions".into(),
        serde_json::Value::Array(vec![serde_json::Value::Number(
            serde_json::Number::from(SUPPORTED_VERSION),
        )]),
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
    if let Some(name) = &opts.client_name {
        hello_obj.insert(
            "client_name".into(),
            serde_json::Value::String(name.clone()),
        );
    }
    let hello_bytes = serde_json::to_vec(&serde_json::Value::Object(hello_obj))
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode hello: {e}")))?;
    let hello = Frame::new(MessageKind::Hello, 1, hello_bytes);
    stream
        .write_all(&encode_frame(&hello))
        .await
        .map_err(io_err)?;

    // 2. Read HelloAck.
    let ack = read_frame(stream).await?;
    let chosen_auth = match ack.kind {
        MessageKind::HelloAck => parse_chosen_auth(&ack.payload)?,
        MessageKind::AuthFail => {
            return Ok(HandshakeOutcome::Refused(
                parse_reason(&ack.payload).unwrap_or_else(|| "AuthFail at HelloAck".into()),
            ));
        }
        other => {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                format!("expected HelloAck, got {other:?}"),
            ));
        }
    };

    // 3. Send AuthResponse for the chosen method.
    let resp_payload = match (chosen_auth.as_str(), &opts.auth) {
        ("anonymous", _) => Vec::new(),
        ("bearer", Auth::Bearer(token)) => {
            let mut obj = serde_json::Map::new();
            obj.insert("token".into(), serde_json::Value::String(token.clone()));
            serde_json::to_vec(&serde_json::Value::Object(obj))
                .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("encode auth: {e}")))?
        }
        ("bearer", Auth::Anonymous) => {
            return Err(ClientError::new(
                ErrorCode::AuthRefused,
                "server demanded bearer auth but no token was supplied",
            ));
        }
        (other, _) => {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                format!("server picked unsupported auth method: {other}"),
            ));
        }
    };
    let resp = Frame::new(MessageKind::AuthResponse, 2, resp_payload);
    stream
        .write_all(&encode_frame(&resp))
        .await
        .map_err(io_err)?;

    // 4. Read AuthOk / AuthFail.
    let final_frame = read_frame(stream).await?;
    match final_frame.kind {
        MessageKind::AuthOk => {
            let parsed: serde_json::Value = serde_json::from_slice(&final_frame.payload)
                .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode auth_ok: {e}")))?;
            let session_id = parsed
                .as_object()
                .and_then(|o| o.get("session_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let server_features = parsed
                .as_object()
                .and_then(|o| o.get("features"))
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(0);
            Ok(HandshakeOutcome::Authenticated {
                session_id,
                server_features,
            })
        }
        MessageKind::AuthFail => {
            let reason = parse_reason(&final_frame.payload)
                .unwrap_or_else(|| "auth refused".into());
            Ok(HandshakeOutcome::Refused(reason))
        }
        other => Err(ClientError::new(
            ErrorCode::Protocol,
            format!("expected AuthOk/AuthFail, got {other:?}"),
        )),
    }
}

async fn read_frame<S>(stream: &mut S) -> Result<Frame>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await.map_err(io_err)?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        stream
            .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
            .await
            .map_err(io_err)?;
    }
    let (frame, _) = decode_frame(&buf)
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode: {e}")))?;
    Ok(frame)
}

fn parse_chosen_auth(payload: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode hello_ack: {e}")))?;
    v.as_object()
        .and_then(|o| o.get("auth"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| ClientError::new(ErrorCode::Protocol, "hello_ack missing auth field"))
}

fn parse_reason(payload: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.as_object()?.get("reason")?.as_str().map(String::from)
}

fn io_err(err: std::io::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, err.to_string())
}
