//! Client-side handshake — Hello → HelloAck → AuthResponse → AuthOk.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{ClientError, ErrorCode, Result};
use reddb_wire::redwire::handshake::{
    base64_std, build_auth_response_anonymous_payload, build_auth_response_bearer_payload,
    build_auth_response_frame, build_client_hello_frame, AuthFail, AuthOk, HelloAck,
};
use reddb_wire::redwire::scram::{ScramClientError, ScramClientHandshake, ScramClientOutput};

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
        Auth::Basic { .. } => vec!["scram-sha-256"],
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

    if chosen_auth == "scram-sha-256" {
        return match &opts.auth {
            Auth::Basic { user, pass } => run_scram(stream, user, pass).await,
            _ => Err(ClientError::new(
                ErrorCode::AuthRefused,
                "server demanded SCRAM auth but no username/password was supplied",
            )),
        };
    }

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
        ("apikey", Auth::ApiKey(_)) => {
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

async fn run_scram<S>(stream: &mut S, username: &str, password: &str) -> Result<HandshakeOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut nonce_bytes = [0u8; 18];
    getrandom::fill(&mut nonce_bytes).map_err(|error| {
        ClientError::new(
            ErrorCode::Protocol,
            format!("generate SCRAM client nonce: {error}"),
        )
    })?;
    let mut handshake = ScramClientHandshake::new(username, password, base64_std(&nonce_bytes))
        .map_err(scram_error)?;

    let client_first = expect_scram_response(handshake.start())?;
    let first_frame = build_auth_response_frame(2, client_first).map_err(frame_build_err)?;
    io::write_frame(stream, &first_frame).await?;

    let server_first = io::read_frame(stream).await?;
    let client_final = match handshake.step(server_first.kind, &server_first.payload) {
        Ok(output) => expect_scram_output_response(output)?,
        Err(ScramClientError::Refused(reason)) => return Ok(HandshakeOutcome::Refused(reason)),
        Err(error) => return Err(scram_error(error)),
    };
    let final_frame = build_auth_response_frame(3, client_final).map_err(frame_build_err)?;
    io::write_frame(stream, &final_frame).await?;

    let server_final = io::read_frame(stream).await?;
    match handshake.step(server_final.kind, &server_final.payload) {
        Ok(ScramClientOutput::Authenticated(auth_ok)) => Ok(HandshakeOutcome::Authenticated {
            session_id: auth_ok.session_id,
            server_features: auth_ok.features,
        }),
        Ok(ScramClientOutput::Response(_)) => Err(ClientError::new(
            ErrorCode::Protocol,
            "SCRAM driver requested an extra response after server-final",
        )),
        Err(ScramClientError::Refused(reason)) => Ok(HandshakeOutcome::Refused(reason)),
        Err(error) => Err(scram_error(error)),
    }
}

fn expect_scram_response(
    output: std::result::Result<ScramClientOutput, ScramClientError>,
) -> Result<Vec<u8>> {
    output
        .map_err(scram_error)
        .and_then(expect_scram_output_response)
}

fn expect_scram_output_response(output: ScramClientOutput) -> Result<Vec<u8>> {
    match output {
        ScramClientOutput::Response(payload) => Ok(payload),
        ScramClientOutput::Authenticated(_) => Err(ClientError::new(
            ErrorCode::Protocol,
            "SCRAM driver authenticated before server-final",
        )),
    }
}

fn scram_error(error: ScramClientError) -> ClientError {
    let code = match error {
        ScramClientError::Refused(_) => ErrorCode::AuthRefused,
        ScramClientError::Protocol(_) => ErrorCode::Protocol,
    };
    ClientError::new(code, error.to_string())
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
