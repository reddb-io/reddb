//! Client-side handshake — Hello → HelloAck → AuthResponse → AuthOk.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::handshake::{
    build_auth_response_anonymous_payload, build_auth_response_bearer_payload, build_hello_payload,
    AuthFail, AuthOk, HelloAck,
};

use super::{io, Auth, ConnectOptions};
use reddb_wire::redwire::{Frame, MessageKind, MAX_KNOWN_MINOR_VERSION};

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
    let hello_bytes = build_hello_payload(
        &[MAX_KNOWN_MINOR_VERSION],
        methods,
        0,
        opts.client_name.as_deref(),
    );
    let hello = Frame::new(MessageKind::Hello, 1, hello_bytes);
    io::write_frame(stream, &hello).await?;

    // 2. Read HelloAck.
    let ack = io::read_frame(stream).await?;
    let chosen_auth = match ack.kind {
        MessageKind::HelloAck => parse_hello_ack(&ack.payload)?.auth,
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
        ("anonymous", _) => build_auth_response_anonymous_payload(),
        ("bearer", Auth::Bearer(token)) => build_auth_response_bearer_payload(token),
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
    io::write_frame(stream, &resp).await?;

    // 4. Read AuthOk / AuthFail.
    let final_frame = io::read_frame(stream).await?;
    match final_frame.kind {
        MessageKind::AuthOk => {
            let parsed = parse_auth_ok(&final_frame.payload)?;
            Ok(HandshakeOutcome::Authenticated {
                session_id: parsed.session_id,
                server_features: parsed.features,
            })
        }
        MessageKind::AuthFail => {
            let reason =
                parse_reason(&final_frame.payload).unwrap_or_else(|| "auth refused".into());
            Ok(HandshakeOutcome::Refused(reason))
        }
        other => Err(ClientError::new(
            ErrorCode::Protocol,
            format!("expected AuthOk/AuthFail, got {other:?}"),
        )),
    }
}

fn parse_hello_ack(payload: &[u8]) -> Result<HelloAck> {
    HelloAck::from_payload(payload)
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode hello_ack: {e}")))
}

fn parse_auth_ok(payload: &[u8]) -> Result<AuthOk> {
    AuthOk::from_payload(payload)
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("decode auth_ok: {e}")))
}

fn parse_reason(payload: &[u8]) -> Option<String> {
    AuthFail::from_payload(payload).ok().map(|fail| fail.reason)
}
