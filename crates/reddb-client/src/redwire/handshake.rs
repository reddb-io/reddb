//! Client-side handshake — Hello → HelloAck → AuthResponse → AuthOk.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::handshake::{
    build_auth_response_anonymous_payload, build_auth_response_bearer_payload,
    build_auth_response_frame, build_client_hello_frame, AuthFail, AuthOk, HelloAck,
};

use super::{io, Auth, ConnectOptions};
use reddb_wire::redwire::{BuildError, MessageKind};

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
        Auth::Basic { .. } => vec!["basic"],
        Auth::ApiKey(_) => vec!["apikey"],
        Auth::Anonymous => vec!["anonymous", "bearer"],
    };
    let hello = build_client_hello_frame(1, methods, 0, opts.client_name.as_deref())
        .map_err(frame_build_err)?;
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
        ("basic", Auth::Basic { .. }) | ("apikey", Auth::ApiKey(_)) => {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                format!("client auth response codec is not implemented for {chosen_auth}"),
            ));
        }
        (other, _) => {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                format!("server picked unsupported auth method: {other}"),
            ));
        }
    };
    let resp = build_auth_response_frame(2, resp_payload).map_err(frame_build_err)?;
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

fn frame_build_err(err: BuildError) -> ClientError {
    ClientError::new(ErrorCode::Protocol, format!("build redwire frame: {err}"))
}

fn parse_reason(payload: &[u8]) -> Option<String> {
    AuthFail::from_payload(payload).ok().map(|fail| fail.reason)
}
