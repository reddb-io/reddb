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
use reddb_wire::redwire::{
    build_auth_response_bearer_payload, build_auth_response_frame, build_client_hello_frame,
    decode_frame, encode_frame, read_frame_async, write_frame_async, Frame, MessageKind,
    FRAME_HEADER_SIZE, REDWIRE_MAGIC,
};

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

/// Bridge the binary WebSocket data channel to a RedWire session over the
/// **embedded engine**.
///
/// The session runs on the application side of an in-memory duplex; the
/// pump loop ([`pump_ws_stream`]) moves bytes between the network side of
/// the duplex and the WS. When either side closes, both halves drop and
/// the peer observes EOF.
pub(crate) async fn run_ws_session(socket: WebSocket, server: super::RedDBServer) {
    let runtime = Arc::new(server.runtime().clone());
    // Same auth wiring as the socket listener path: bearer/JWT are
    // negotiated in the RedWire handshake from the runtime's stores.
    let auth_store = runtime.auth_store();
    let oauth = runtime.oauth_validator();

    let (session_io, net_io) = tokio::io::duplex(WS_BRIDGE_BUF);
    let session = tokio::spawn(async move {
        let _ = handle_session_consume_magic(session_io, runtime, auth_store, oauth).await;
    });

    // Pump the WS data channel against the network side of the duplex.
    pump_ws_stream(socket, net_io).await;

    // The pump already dropped its stream halves (signalling EOF to the
    // session side); abort backstops any task still parked (e.g. on a live
    // queue wait).
    session.abort();
}

/// Like [`run_ws_session`] but in **injected-auth mode** (issue #1048): the
/// `red ui` bridge holds the bearer `token` and presents it in the RedWire
/// handshake on the UI's behalf, so the UI never sees or persists the secret.
///
/// The session runs against the embedded engine over the same in-memory
/// duplex; [`inject_bearer_handshake`] mediates the handshake (swapping the
/// UI's auth turn for a bearer `AuthResponse` carrying `token`) before the
/// byte pump takes over.
pub(crate) async fn run_injected_ws_session(
    socket: WebSocket,
    server: super::RedDBServer,
    token: &str,
) {
    let runtime = Arc::new(server.runtime().clone());
    let auth_store = runtime.auth_store();
    let oauth = runtime.oauth_validator();

    let (session_io, net_io) = tokio::io::duplex(WS_BRIDGE_BUF);
    let session = tokio::spawn(async move {
        let _ = handle_session_consume_magic(session_io, runtime, auth_store, oauth).await;
    });

    inject_bearer_handshake(socket, net_io, token).await;

    session.abort();
}

/// Mediate the RedWire handshake in injected-auth mode, then pump the rest of
/// the session byte-for-byte ([`pump_ws_stream`]).
///
/// The UI speaks RedWire as usual (preamble → Hello → wait HelloAck →
/// AuthResponse → wait AuthOk), but the bridge owns the credential: it opens
/// its own handshake with `backend` advertising `bearer`, relays the
/// backend's `HelloAck`/`AuthOk` back to the UI, and **discards the UI's own
/// `AuthResponse`** — substituting a bearer `AuthResponse` carrying `token`
/// toward the backend. Correlation ids are mirrored from the UI's frames so
/// the relayed replies line up with the UI's client state machine. The token
/// thus appears only in the backend-bound handshake, never in anything the UI
/// sent.
pub(crate) async fn inject_bearer_handshake<S>(mut socket: WebSocket, mut backend: S, token: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let mut reader = WsFrameReader::new();

    // 1. UI preamble: discriminator magic + minor-version byte.
    let preamble = match reader.read_exact_bytes(&mut socket, 2).await {
        Some(bytes) => bytes,
        None => return close_ws(socket).await,
    };
    if preamble[0] != REDWIRE_MAGIC {
        return close_ws(socket).await;
    }
    let minor = preamble[1];

    // 2. UI Hello — we keep only its correlation id so the backend's HelloAck
    //    reply_to lines up with what the UI is waiting for.
    let ui_hello = match reader.read_frame(&mut socket).await {
        Some(frame) if frame.kind == MessageKind::Hello => frame,
        _ => return close_ws(socket).await,
    };

    // 3. Open the backend handshake ourselves, advertising bearer first.
    //    `anonymous` is offered as a fallback so a `--token` against an
    //    unauthenticated DB still connects (the server picks anonymous and
    //    ignores the bearer payload); an authenticated DB blocks anonymous
    //    and negotiates bearer, validating the injected token.
    if backend.write_all(&[REDWIRE_MAGIC, minor]).await.is_err() {
        return close_ws(socket).await;
    }
    let our_hello = match build_client_hello_frame(
        ui_hello.correlation_id,
        ["bearer", "anonymous"],
        0,
        Some("red-ui-bridge"),
    ) {
        Ok(frame) => frame,
        Err(_) => return close_ws(socket).await,
    };
    if write_frame_async(&mut backend, &our_hello).await.is_err() {
        return close_ws(socket).await;
    }

    // 4. HelloAck from the backend → relay verbatim to the UI.
    let ack = match read_frame_async(&mut backend).await {
        Ok(frame) => frame,
        Err(_) => return close_ws(socket).await,
    };
    if send_frame(&mut socket, &ack).await.is_err() {
        return;
    }

    // 5. UI AuthResponse — discarded (it carries no secret in injected mode);
    //    we keep only its correlation id for the substituted bearer frame.
    let ui_auth = match reader.read_frame(&mut socket).await {
        Some(frame) => frame,
        None => return close_ws(socket).await,
    };

    // 6. Inject the held bearer token toward the backend.
    let bearer = match build_auth_response_frame(
        ui_auth.correlation_id,
        build_auth_response_bearer_payload(token),
    ) {
        Ok(frame) => frame,
        Err(_) => return close_ws(socket).await,
    };
    if write_frame_async(&mut backend, &bearer).await.is_err() {
        return close_ws(socket).await;
    }

    // 7. AuthOk / AuthFail from the backend → relay verbatim to the UI, so the
    //    UI opens already authenticated (or sees the failure).
    let auth_reply = match read_frame_async(&mut backend).await {
        Ok(frame) => frame,
        Err(_) => return close_ws(socket).await,
    };
    if send_frame(&mut socket, &auth_reply).await.is_err() {
        return;
    }

    // 8. Any post-handshake bytes the UI already pipelined must reach the
    //    backend before the pump's read loop takes over.
    if !reader.buffered().is_empty() {
        let pending = reader.take_buffered();
        if backend.write_all(&pending).await.is_err() {
            return close_ws(socket).await;
        }
    }

    pump_ws_stream(socket, backend).await;
}

/// Encode and send a RedWire frame as one binary WS message.
async fn send_frame(socket: &mut WebSocket, frame: &Frame) -> Result<(), axum::Error> {
    socket
        .send(Message::Binary(Bytes::from(encode_frame(frame))))
        .await
}

/// Best-effort close on a WS the bridge is abandoning mid-handshake.
async fn close_ws(mut socket: WebSocket) {
    let _ = socket.send(Message::Close(None)).await;
}

/// Buffered reader that reassembles the self-delimiting RedWire byte stream
/// (raw preamble bytes + framed messages) out of a binary WebSocket's
/// messages, which need not align with frame boundaries.
struct WsFrameReader {
    buf: Vec<u8>,
}

impl WsFrameReader {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Bytes buffered past the last frame/preamble already returned.
    fn buffered(&self) -> &[u8] {
        &self.buf
    }

    /// Take the buffered tail, leaving the reader empty.
    fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Pull binary messages until at least `n` bytes are buffered, then split
    /// the first `n` off and return them. `None` on close/EOF beforehand.
    async fn read_exact_bytes(&mut self, socket: &mut WebSocket, n: usize) -> Option<Vec<u8>> {
        while self.buf.len() < n {
            if !self.fill(socket).await {
                return None;
            }
        }
        let tail = self.buf.split_off(n);
        Some(std::mem::replace(&mut self.buf, tail))
    }

    /// Reassemble and return the next full RedWire frame. `None` on close/EOF
    /// before a frame completes.
    async fn read_frame(&mut self, socket: &mut WebSocket) -> Option<Frame> {
        loop {
            if self.buf.len() >= FRAME_HEADER_SIZE {
                if let Ok((frame, consumed)) = decode_frame(&self.buf) {
                    self.buf.drain(..consumed);
                    return Some(frame);
                }
            }
            if !self.fill(socket).await {
                return None;
            }
        }
    }

    /// Append one inbound binary message; skip control/text frames. Returns
    /// `false` on close, error, or stream end.
    async fn fill(&mut self, socket: &mut WebSocket) -> bool {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Binary(bytes))) => {
                    self.buf.extend_from_slice(&bytes);
                    return true;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return false,
                Some(Ok(_)) => continue,
            }
        }
    }
}

/// Pump bytes between a binary WebSocket and an arbitrary byte stream.
///
/// This is the transport-agnostic seam (ADR 0036): inbound `Binary`
/// messages are written to `stream`, and bytes read back from `stream`
/// are sent as outbound `Binary` messages. The byte stream may be the
/// network side of the embedded-engine duplex ([`run_ws_session`]) or a
/// remote RedWire-over-TCP/TLS connection (the `red ui` bridge to a
/// `red://` / `reds://` target, issue #1044) — the loop is identical
/// because RedWire framing is self-delimiting on either end.
///
/// Returns once either side closes; both stream halves are dropped on the
/// way out so the peer observes EOF.
pub(crate) async fn pump_ws_stream<S>(mut socket: WebSocket, stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let (mut net_read, mut net_write) = tokio::io::split(stream);
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
                    // Stream closed its write half → no more output.
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

    // Dropping the network halves signals EOF to the stream side.
    drop(net_write);
    drop(net_read);
    let _ = socket.send(Message::Close(None)).await;
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
