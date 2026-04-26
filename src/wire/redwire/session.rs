//! Per-connection v2 session: handshake → frame loop → bye.
//!
//! The dispatch loop is intentionally narrow in this initial cut.
//! It accepts:
//!   - Hello / AuthResponse (handshake only — once)
//!   - Query  → runs SQL, replies with one Result frame (JSON payload)
//!   - Ping   → Pong
//!   - Bye    → break loop
//!
//! Bulk inserts, prepared statements, multiplexed streams,
//! VectorSearch, GraphTraverse, Cancel — all land in subsequent
//! PRs. The framing + auth pieces are the load-bearing parts of
//! this PR; data-plane completeness is a follow-up.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};

use super::auth::{
    build_auth_fail, build_auth_ok, build_hello_ack, pick_auth_method, validate_auth_response,
    AuthOutcome, Hello,
};
use super::codec::{decode_frame, encode_frame};
use super::frame::{Frame, MessageKind, FRAME_HEADER_SIZE};
use super::{MAX_KNOWN_MINOR_VERSION, REDWIRE_V2_MAGIC};

#[derive(Debug)]
struct AuthedSession {
    username: String,
    #[allow(dead_code)]
    session_id: String,
}

pub async fn handle_session(
    mut stream: TcpStream,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
) -> io::Result<()> {
    // Discriminator byte was already consumed by the service-router
    // detector when it dispatched here. If callers wire this from
    // a non-router path they must consume it themselves first.
    let session = perform_handshake(&mut stream, auth_store.as_deref()).await?;
    if session.is_none() {
        return Ok(());
    }
    let _session = session.unwrap();

    let mut buf = vec![0u8; FRAME_HEADER_SIZE];
    loop {
        // Read header.
        if let Err(err) = stream.read_exact(&mut buf[..FRAME_HEADER_SIZE]).await {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(err);
        }
        let length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if length < FRAME_HEADER_SIZE || length > super::frame::MAX_FRAME_SIZE as usize {
            return Err(io::Error::other(format!("invalid frame length {length}")));
        }
        if buf.len() < length {
            buf.resize(length, 0);
        }
        let payload_len = length - FRAME_HEADER_SIZE;
        if payload_len > 0 {
            stream
                .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
                .await?;
        }
        let (frame, _) = decode_frame(&buf[..length])
            .map_err(|e| io::Error::other(format!("decode frame: {e}")))?;

        match frame.kind {
            MessageKind::Bye => {
                let bye = encode_frame(&Frame::new(MessageKind::Bye, frame.correlation_id, vec![]));
                let _ = stream.write_all(&bye).await;
                return Ok(());
            }
            MessageKind::Ping => {
                let pong = encode_frame(&Frame::new(MessageKind::Pong, frame.correlation_id, vec![]));
                stream.write_all(&pong).await?;
            }
            MessageKind::Query => {
                let response = run_query(&runtime, &frame);
                let bytes = encode_frame(&response);
                stream.write_all(&bytes).await?;
            }
            other => {
                let err = encode_frame(&Frame::new(
                    MessageKind::Error,
                    frame.correlation_id,
                    format!("v2 server cannot dispatch {other:?} yet").into_bytes(),
                ));
                stream.write_all(&err).await?;
            }
        }
    }
}

/// Run the handshake. Returns `Ok(None)` when the client disconnected
/// or the auth was refused (the failure frame is already on the wire).
async fn perform_handshake(
    stream: &mut TcpStream,
    auth_store: Option<&AuthStore>,
) -> io::Result<Option<AuthedSession>> {
    // Step 1: read minor version byte.
    let mut minor_buf = [0u8; 1];
    stream.read_exact(&mut minor_buf).await?;
    let minor = minor_buf[0];
    if minor > MAX_KNOWN_MINOR_VERSION {
        // Future client speaking a version we don't know — refuse
        // immediately. We do not send a frame because the client
        // hasn't agreed on the framing version yet.
        return Ok(None);
    }

    // Step 2: read the Hello frame.
    let hello = read_frame(stream).await?;
    if hello.kind != MessageKind::Hello {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            hello.correlation_id,
            build_auth_fail("first frame after magic must be Hello"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let hello_msg = match Hello::from_payload(&hello.payload) {
        Ok(h) => h,
        Err(e) => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                hello.correlation_id,
                build_auth_fail(&e),
            ));
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    let chosen_version = hello_msg
        .versions
        .iter()
        .copied()
        .filter(|v| *v <= MAX_KNOWN_MINOR_VERSION)
        .max()
        .unwrap_or(0);
    if chosen_version == 0 {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            hello.correlation_id,
            build_auth_fail("no overlapping protocol version"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    let server_anon_ok = auth_store.map(|s| !s.is_enabled()).unwrap_or(true);
    let chosen = match pick_auth_method(&hello_msg.auth_methods, server_anon_ok) {
        Some(m) => m,
        None => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                hello.correlation_id,
                build_auth_fail("no overlapping auth method"),
            ));
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // Step 3: HelloAck.
    let server_features = 0u32;
    let ack = encode_frame(&Frame::new(
        MessageKind::HelloAck,
        hello.correlation_id,
        build_hello_ack(chosen_version, chosen, server_features),
    ));
    stream.write_all(&ack).await?;

    // Step 4: AuthResponse (no challenge in v2.1 since bearer/anonymous
    // are zero-round-trip).
    let resp = read_frame(stream).await?;
    if resp.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            resp.correlation_id,
            build_auth_fail("expected AuthResponse"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    match validate_auth_response(chosen, &resp.payload, auth_store) {
        AuthOutcome::Authenticated {
            username,
            role,
            session_id,
        } => {
            let ok = encode_frame(&Frame::new(
                MessageKind::AuthOk,
                resp.correlation_id,
                build_auth_ok(&session_id, &username, role, server_features),
            ));
            stream.write_all(&ok).await?;
            Ok(Some(AuthedSession {
                username,
                session_id,
            }))
        }
        AuthOutcome::Refused(reason) => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                resp.correlation_id,
                build_auth_fail(&reason),
            ));
            let _ = stream.write_all(&fail).await;
            Ok(None)
        }
    }
}

async fn read_frame(stream: &mut TcpStream) -> io::Result<Frame> {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if length < FRAME_HEADER_SIZE || length > super::frame::MAX_FRAME_SIZE as usize {
        return Err(io::Error::other(format!(
            "redwire frame length {length} out of range"
        )));
    }
    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        stream
            .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
            .await?;
    }
    let (frame, _) = decode_frame(&buf)
        .map_err(|e| io::Error::other(format!("decode frame: {e}")))?;
    Ok(frame)
}

fn run_query(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let sql = match std::str::from_utf8(&frame.payload) {
        Ok(s) => s,
        Err(_) => {
            return Frame::new(
                MessageKind::Error,
                frame.correlation_id,
                b"Query payload must be UTF-8 SQL".to_vec(),
            );
        }
    };
    match runtime.execute_query(sql) {
        Ok(result) => {
            let mut obj = crate::serde_json::Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(true));
            obj.insert(
                "statement".to_string(),
                JsonValue::String(result.statement_type.to_string()),
            );
            obj.insert(
                "affected".to_string(),
                JsonValue::Number(result.affected_rows as f64),
            );
            let payload = serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default();
            Frame::new(MessageKind::Result, frame.correlation_id, payload)
        }
        Err(err) => Frame::new(
            MessageKind::Error,
            frame.correlation_id,
            err.to_string().into_bytes(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_byte_is_0xfe() {
        assert_eq!(REDWIRE_V2_MAGIC, 0xFE);
    }
}
