//! RedWire-over-binary-WebSocket edge (issue #935, PRD #930, ADR 0036).
//!
//! Gives the browser the **same multiplexed binary RedWire protocol** as
//! native drivers, tunneled over a WSS the browser can open. A browser
//! cannot speak RedWire-over-TCP, so this route upgrades a binary
//! WebSocket and bridges its data channel into the transport-agnostic
//! [`handle_session_consume_magic`] (issue #932): the browser sends the
//! exact native preamble (`0xFE` magic + minor version + `Hello`), and
//! the session runs unchanged over the WS byte stream.
//!
//! Security (ADR 0036, first-class — not a follow-up):
//!   * **WSS only.** The upgrade is accepted on the TLS edge only; a
//!     request arriving on the clear-text listener is rejected even if
//!     the route was mounted there.
//!   * **Origin allowlist.** WebSocket is *not* covered by CORS, so the
//!     upgrade validates the `Origin` header against an explicit,
//!     exact-match allowlist (Cross-Site WebSocket Hijacking defence).
//!     The route is mounted only when the allowlist is non-empty
//!     (default-deny), so reaching this handler already implies one or
//!     more origins are configured.
//!
//! Auth (bearer / OAuth-JWT) is negotiated inside the RedWire handshake
//! exactly as on the socket transports; mTLS stays native-only. The
//! browser credential layer (httpOnly refresh cookie + short-lived access
//! JWT) is issue #936 and rides on top of this transport.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::axum_edge::EdgeState;
use super::http_handler_metrics::HttpTransport;
use crate::wire::redwire::listener::handle_session_consume_magic;

/// Path the browser client (`red+wss://host:port`) resolves to.
pub(super) const REDWIRE_WS_PATH: &str = "/redwire";

/// WebSocket subprotocol the upgrade advertises. Versioned so a future
/// framing revision can coexist with v1 clients.
pub(super) const REDWIRE_WS_SUBPROTOCOL: &str = "reddb.redwire.v1";

/// Duplex buffer bridging the WS data channel and the RedWire session.
/// Sized to hold a couple of typical frames without round-tripping the
/// bridge loop per message; backpressure still applies once it fills.
const WS_BRIDGE_BUF: usize = 64 * 1024;

/// Chunk pulled from the session's write half before wrapping it in one
/// binary WS message. Frame boundaries need not align with WS message
/// boundaries — RedWire framing is self-delimiting (16-byte length
/// header), so the byte stream is reassembled on either end.
const WS_READ_CHUNK: usize = 16 * 1024;

/// Why a WS upgrade was refused. Kept as a distinct type so the gate is
/// unit-testable without spinning a TLS edge or a real WebSocket.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WsRejection {
    /// Arrived on a non-TLS edge — `ws://` is never accepted.
    NotTls,
    /// No `Origin` header — a browser always sends one; its absence is
    /// either a non-browser caller or a stripped header. Reject.
    OriginMissing,
    /// `Origin` is not on the configured allowlist.
    OriginRejected,
}

impl WsRejection {
    fn status_and_msg(&self) -> (StatusCode, &'static str) {
        match self {
            WsRejection::NotTls => (
                StatusCode::FORBIDDEN,
                "redwire websocket requires TLS (wss://)",
            ),
            WsRejection::OriginMissing => (
                StatusCode::FORBIDDEN,
                "redwire websocket upgrade requires an Origin header",
            ),
            WsRejection::OriginRejected => (
                StatusCode::FORBIDDEN,
                "origin not allowed for redwire websocket",
            ),
        }
    }
}

impl IntoResponse for WsRejection {
    fn into_response(self) -> Response {
        let (status, msg) = self.status_and_msg();
        (status, msg).into_response()
    }
}

/// Pure upgrade gate (ADR 0036): TLS-only, then exact-match `Origin`
/// against the allowlist. Factored out so the policy is tested directly.
pub(super) fn ws_upgrade_decision(
    transport: HttpTransport,
    origin: Option<&str>,
    allowlist: &[String],
) -> Result<(), WsRejection> {
    if transport != HttpTransport::Https {
        return Err(WsRejection::NotTls);
    }
    match origin {
        None => Err(WsRejection::OriginMissing),
        Some(o) if allowlist.iter().any(|allowed| allowed == o) => Ok(()),
        Some(_) => Err(WsRejection::OriginRejected),
    }
}

/// axum handler for `GET /redwire`. Validates the upgrade gate, then —
/// on success — upgrades to a binary WebSocket and runs a RedWire session
/// over it.
pub(super) async fn redwire_ws_upgrade(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());

    if let Err(rejection) = ws_upgrade_decision(
        state.transport,
        origin,
        state.server.websocket_allowed_origins(),
    ) {
        return rejection.into_response();
    }

    let server = state.server.clone();
    ws.protocols([REDWIRE_WS_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            run_ws_session(socket, server).await;
        })
}

/// Classification of one inbound WebSocket poll result for the bridge.
enum WsInbound {
    /// Binary payload — RedWire bytes to feed the session.
    Data(Bytes),
    /// Text / Ping / Pong — not RedWire bytes; tungstenite auto-replies
    /// to pings, so the bridge just skips these.
    Ignore,
    /// Clean close, stream end, or a transport error — stop the bridge.
    Eof,
}

/// Map an inbound WS poll result to a bridge action (pure, so the
/// Message↔bytes mapping is unit-tested without a live socket). Binary
/// payloads pass through byte-for-byte; RedWire's self-delimiting 16-byte
/// length header means a frame split across several binary messages
/// reassembles correctly on the session side.
fn classify_inbound(inbound: Option<Result<Message, axum::Error>>) -> WsInbound {
    match inbound {
        Some(Ok(Message::Binary(bytes))) => WsInbound::Data(bytes),
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => WsInbound::Eof,
        Some(Ok(_)) => WsInbound::Ignore,
    }
}

/// Bridge the binary WebSocket data channel to a RedWire session.
///
/// The session runs on the application side of an in-memory duplex; this
/// loop pumps bytes between the network side of the duplex and the WS:
/// inbound `Binary` messages become session input, session output becomes
/// outbound `Binary` messages. When either side closes, both halves drop
/// and the peer observes EOF.
///
/// Exposed `pub(crate)` so the local `red ui` bridge (issue #1042, ADR
/// 0047/0049) reuses the exact same async-transport ↔ sync-engine seam
/// over a loopback WebSocket rather than re-deriving the bridge loop.
pub(crate) async fn run_ws_session(mut socket: WebSocket, server: super::RedDBServer) {
    let runtime = Arc::new(server.runtime().clone());
    // Same auth wiring as the socket listener path: bearer/JWT are
    // negotiated in the RedWire handshake from the runtime's stores.
    let auth_store = runtime.auth_store();
    let oauth = runtime.oauth_validator();

    let (session_io, net_io) = tokio::io::duplex(WS_BRIDGE_BUF);
    let session = tokio::spawn(async move {
        let _ = handle_session_consume_magic(session_io, runtime, auth_store, oauth).await;
    });

    let (mut net_read, mut net_write) = tokio::io::split(net_io);
    let mut out_buf = vec![0u8; WS_READ_CHUNK];

    loop {
        tokio::select! {
            inbound = socket.recv() => {
                match classify_inbound(inbound) {
                    WsInbound::Data(bytes) => {
                        if net_write.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    WsInbound::Ignore => {}
                    WsInbound::Eof => break,
                }
            }
            outbound = net_read.read(&mut out_buf) => {
                match outbound {
                    // Session closed its write half → no more output.
                    Ok(0) => break,
                    Ok(n) => {
                        let msg = Message::Binary(Bytes::copy_from_slice(&out_buf[..n]));
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Dropping the network halves signals EOF to the session side; abort
    // backstops any task still parked (e.g. on a live queue wait).
    drop(net_write);
    drop(net_read);
    let _ = socket.send(Message::Close(None)).await;
    session.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist() -> Vec<String> {
        vec![
            "https://app.example.com".to_string(),
            "https://admin.example.com".to_string(),
        ]
    }

    #[test]
    fn allowed_origin_over_tls_is_accepted() {
        let result = ws_upgrade_decision(
            HttpTransport::Https,
            Some("https://app.example.com"),
            &allowlist(),
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn non_allowlisted_origin_is_rejected() {
        let result = ws_upgrade_decision(
            HttpTransport::Https,
            Some("https://evil.example.com"),
            &allowlist(),
        );
        assert_eq!(result, Err(WsRejection::OriginRejected));
    }

    #[test]
    fn missing_origin_is_rejected() {
        let result = ws_upgrade_decision(HttpTransport::Https, None, &allowlist());
        assert_eq!(result, Err(WsRejection::OriginMissing));
    }

    #[test]
    fn plain_ws_over_http_edge_is_rejected_even_when_origin_allowed() {
        // WSS-only: the TLS check precedes the origin check, so an
        // otherwise-allowed origin on the clear-text edge still fails.
        let result = ws_upgrade_decision(
            HttpTransport::Http,
            Some("https://app.example.com"),
            &allowlist(),
        );
        assert_eq!(result, Err(WsRejection::NotTls));
    }

    #[test]
    fn empty_allowlist_rejects_every_origin() {
        let result =
            ws_upgrade_decision(HttpTransport::Https, Some("https://app.example.com"), &[]);
        assert_eq!(result, Err(WsRejection::OriginRejected));
    }

    #[test]
    fn origin_match_is_exact_not_prefix() {
        // A suffix/prefix of an allowed origin must not slip through.
        let result = ws_upgrade_decision(
            HttpTransport::Https,
            Some("https://app.example.com.evil.com"),
            &allowlist(),
        );
        assert_eq!(result, Err(WsRejection::OriginRejected));
    }

    /// Helper: assert a `WsInbound` is `Data` carrying exactly `expected`.
    fn assert_data(got: WsInbound, expected: &[u8]) {
        match got {
            WsInbound::Data(b) => assert_eq!(&b[..], expected),
            WsInbound::Ignore => panic!("expected Data, got Ignore"),
            WsInbound::Eof => panic!("expected Data, got Eof"),
        }
    }

    #[test]
    fn binary_message_passes_through_byte_for_byte() {
        // A framed RedWire message (here the magic+minor preamble plus a
        // stub header) must reach the session unaltered.
        let frame = vec![0xFE, 0x01, 0x10, 0x00, 0x00, 0x00];
        let got = classify_inbound(Some(Ok(Message::Binary(Bytes::from(frame.clone())))));
        assert_data(got, &frame);
    }

    #[test]
    fn binary_messages_reassemble_in_order() {
        // RedWire framing is self-delimiting, so one logical frame split
        // across two binary messages must arrive as the concatenation in
        // order — the bridge writes each chunk to the session stream as
        // it lands.
        let first = classify_inbound(Some(Ok(Message::Binary(Bytes::from_static(&[0xFE, 0x01])))));
        let second = classify_inbound(Some(Ok(Message::Binary(Bytes::from_static(&[0x10, 0x00])))));
        assert_data(first, &[0xFE, 0x01]);
        assert_data(second, &[0x10, 0x00]);
    }

    #[test]
    fn close_and_stream_end_map_to_eof() {
        assert!(matches!(
            classify_inbound(Some(Ok(Message::Close(None)))),
            WsInbound::Eof
        ));
        assert!(matches!(classify_inbound(None), WsInbound::Eof));
    }

    #[test]
    fn control_frames_are_ignored_not_forwarded() {
        // Ping/Pong/Text are not RedWire bytes and must never reach the
        // session as input.
        assert!(matches!(
            classify_inbound(Some(Ok(Message::Ping(Bytes::new())))),
            WsInbound::Ignore
        ));
        assert!(matches!(
            classify_inbound(Some(Ok(Message::Pong(Bytes::new())))),
            WsInbound::Ignore
        ));
    }
}
