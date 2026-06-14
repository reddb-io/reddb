//! Local `red ui` bridge — the tracer-bullet spine of the red-ui
//! integration (issue #1042, PRD #1041; ADR 0047 bridge, ADR 0049
//! RedWire-over-WS transport, ADR 0036 WS edge).
//!
//! `red ui --server file://<path>` opens a graphical UI in the browser
//! against a local `.rdb`. This module is the process that makes that
//! work: a single loopback (`127.0.0.1`) HTTP server that
//!
//!   * serves the UI bundle (a `--ui-dir` directory, or the checked-in
//!     minimal fixture page when none is given), and
//!   * mounts `/redwire` — a RedWire-over-WebSocket endpoint over the
//!     **embedded engine** opened from the file. The WS data channel is
//!     bridged into the same async-transport ↔ sync-engine seam the
//!     internet WS edge uses ([`super::ws_edge::run_ws_session`], ADR
//!     0036). This serves RedWire over the embedded engine — it is *not*
//!     a proxy of the HTTP surface.
//!
//! Security (ADR 0036, adapted for loopback):
//!   * **Origin allowlist, default-deny.** WebSocket is not covered by
//!     CORS, so the upgrade validates the `Origin` header against an
//!     explicit, exact-match allowlist ([`loopback_ws_origin_allowed`]).
//!     The list is seeded with the bridge's own served origins
//!     (`http://127.0.0.1:<port>` and `http://localhost:<port>`), so the
//!     served page can connect and a cross-site page cannot.
//!   * **WSS-only is relaxed.** The internet edge requires TLS
//!     ([`super::ws_edge::ws_upgrade_decision`]); the loopback bridge
//!     accepts plain `ws://` because it is bound to `127.0.0.1` and never
//!     leaves the host. This is the one rule that differs from the
//!     internet edge, and it is deliberate.
//!
//! The bridge is session-scoped: [`UiBridge::shutdown`] tears the server
//! down cleanly (graceful shutdown of the listener + the serve task), so
//! closing the UI / interrupting the command leaves no orphaned process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::oneshot;

use super::ws_edge::{
    inject_bearer_handshake, pump_ws_stream, run_injected_ws_session, run_ws_session,
    REDWIRE_WS_PATH, REDWIRE_WS_SUBPROTOCOL,
};
use super::RedDBServer;

/// The checked-in minimal UI fixture served when no `--ui-dir` is given.
/// A real bundle is downloaded at runtime in a later slice (PRD #1041);
/// for now this page is enough to open a RedWire-over-WS session and run
/// a query against the embedded engine.
const FIXTURE_INDEX: &str = include_str!("ui_bridge_fixture/index.html");

/// Configuration for [`spawn_ui_bridge`].
#[derive(Debug, Clone, Default)]
pub struct UiBridgeConfig {
    /// Directory to serve the UI bundle from. `None` serves the embedded
    /// [`FIXTURE_INDEX`] at `/` (and `/index.html`).
    pub ui_dir: Option<PathBuf>,
    /// Loopback port to bind. `0` (the default) picks an ephemeral port —
    /// the resolved address is read back from [`UiBridge::local_addr`].
    pub port: u16,
    /// Bearer token held by the bridge and injected into the RedWire
    /// handshake. The served page never receives the token.
    pub injected_token: Option<String>,
    /// Credential-free auth mode hint injected into served HTML.
    pub auth_mode: super::ui_auth::UiAuthMode,
}

/// A remote RedWire endpoint the bridge fronts for a `red://` / `reds://`
/// target (issue #1044, ADR 0047 bridge, ADR 0049 transport). The local
/// loopback WS endpoint pumps its data channel straight into a fresh
/// TCP (or TLS) connection to this host — the UI is unaware that the
/// engine lives in another process / container.
#[derive(Debug, Clone)]
pub struct RemoteRedwireTarget {
    /// Host to dial (the `red://`/`reds://` authority).
    pub host: String,
    /// Port to dial (defaults to `DEFAULT_PORT_RED` via the URI parser).
    pub port: u16,
    /// Negotiate TLS to the target (`reds://`). The handshake is
    /// transparent to the UI.
    pub tls: bool,
    /// Optional CA bundle (PEM) to trust for the TLS handshake, on top of
    /// the webpki system roots. Needed for a self-signed / private-CA
    /// `reds://` target (a dev container); `None` trusts system roots only.
    pub ca_pem: Option<Vec<u8>>,
}

/// What a running bridge fronts: the embedded engine opened from a
/// `file://` target, or a remote RedWire endpoint (`red://` / `reds://`).
/// Both are reached through the *same* loopback WS endpoint and the same
/// byte-pump seam — only the far end of the pump differs.
enum BridgeBackend {
    /// `file://` — RedWire runs over the embedded engine in-process.
    /// Boxed because `RedDBServer` is by far the largest variant payload
    /// (≥280 bytes vs ≤56 for `Remote`); keeping it inline trips
    /// `clippy::large_enum_variant` and bloats every `BridgeBackend`.
    Embedded(Box<RedDBServer>),
    /// `red://` / `reds://` — RedWire bytes are relayed to a remote
    /// RedWire-over-TCP/TLS instance.
    Remote(RemoteRedwireTarget),
    /// `red+wss://` / `red+ws://` — no loopback relay; the browser
    /// connects directly to the target. The `/redwire` route is not
    /// mounted for this backend.
    Direct,
}

/// How `red ui` should connect the browser to a target URI.
///
/// - [`UiTarget::File`] and [`UiTarget::Remote`] are *bridge-required*:
///   a loopback WS relay is started and the UI only talks to that.
/// - [`UiTarget::Direct`] is *bridge-free* (ADR 0047 direct-when-reachable):
///   the browser connects to `ws_url` directly — only a static HTTP server
///   to serve the UI bundle is started, with no WS relay process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiTarget {
    /// A local file / embedded-engine target. The caller canonicalises
    /// the `file://` path and opens the engine itself.
    File,
    /// A remote RedWire-over-TCP (`red://`) or -TLS (`reds://`) target.
    Remote(RemoteRedwireTargetSpec),
    /// A browser-reachable WS endpoint (`red+wss://` or `red+ws://`).
    /// The browser connects to `ws_url` directly; no loopback relay is
    /// started. The UI bundle is still served from a local HTTP server.
    Direct { ws_url: String },
}

/// Host / port / TLS triple parsed from a `red://` / `reds://` URI —
/// the connection-independent part of [`RemoteRedwireTarget`] (no CA
/// bytes), so it can derive `PartialEq` for classification tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRedwireTargetSpec {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// Classify a `red ui` target URI into the bridge backend it requires.
///
/// `file://` and bare filesystem paths resolve to [`UiTarget::File`];
/// `red://host[:port]` and `reds://host[:port]` resolve to
/// [`UiTarget::Remote`] with the parser's default port
/// ([`reddb_wire::DEFAULT_PORT_RED`], 5050) when none is given and `tls`
/// set for `reds://`. Any other scheme (or a cluster URI) is an error —
/// `red ui` bridges exactly one endpoint.
pub fn classify_ui_target(uri: &str) -> Result<UiTarget, String> {
    // A bare path with no scheme is a file target (matches the existing
    // `red ui ./data.rdb` shorthand).
    if !uri.contains("://") {
        return Ok(UiTarget::File);
    }
    match reddb_wire::parse(uri) {
        Ok(reddb_wire::ConnectionTarget::File { .. }) => Ok(UiTarget::File),
        Ok(reddb_wire::ConnectionTarget::RedWire { host, port, tls }) => {
            Ok(UiTarget::Remote(RemoteRedwireTargetSpec {
                host,
                port,
                tls,
            }))
        }
        Ok(reddb_wire::ConnectionTarget::WsNative { host, port, tls }) => {
            // ADR 0047: browser-reachable WS target — no loopback relay.
            let scheme = if tls { "wss" } else { "ws" };
            let ws_url = format!("{scheme}://{host}:{port}/redwire");
            Ok(UiTarget::Direct { ws_url })
        }
        Ok(_) | Err(_) => Err(format!(
            "unsupported target for red ui; supported schemes: \
             file://, red://, reds://, red+ws://, red+wss://; got: {uri}"
        )),
    }
}

/// State threaded into the bridge's axum handlers. Cheap to clone (the
/// backend is shared via `Arc`; the origin list and bundle dir likewise).
#[derive(Clone)]
struct BridgeState {
    backend: Arc<BridgeBackend>,
    allowed_origins: Arc<Vec<String>>,
    ui_dir: Option<Arc<PathBuf>>,
    injected_token: Option<Arc<String>>,
    auth_mode: super::ui_auth::UiAuthMode,
    /// Set for `Direct` targets. When `Some`, the WS URL is injected as
    /// `window.REDDB_WS_URL` into HTML responses so the UI page can
    /// connect directly rather than deriving from `location.host`.
    direct_ws_url: Option<Arc<String>>,
}

/// A running loopback UI bridge. Holds the bound address plus the handles
/// needed to shut the serve task down cleanly. Dropping it without
/// calling [`Self::shutdown`] aborts the serve task on drop of the join
/// handle's runtime; prefer `shutdown().await` for an orderly teardown.
pub struct UiBridge {
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
    /// For `Direct` targets, the WS URL the browser connects to (the
    /// remote endpoint). `None` for bridge targets — `ws_url()` derives
    /// the loopback URL from `local_addr` in that case.
    direct_ws_url: Option<String>,
}

impl UiBridge {
    /// The loopback address the bridge is serving on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// URL of the served UI bundle root — open this in a browser.
    pub fn ui_url(&self) -> String {
        format!("http://{}/", self.local_addr)
    }

    /// The WebSocket URL the served page connects to.
    ///
    /// For bridge targets (`file://`, `red://`, `reds://`) this is the
    /// loopback endpoint on the same server. For direct targets
    /// (`red+wss://`, `red+ws://`) this is the remote endpoint the browser
    /// connects to without a relay.
    pub fn ws_url(&self) -> String {
        self.direct_ws_url
            .clone()
            .unwrap_or_else(|| format!("ws://{}{}", self.local_addr, REDWIRE_WS_PATH))
    }

    /// Signal graceful shutdown and wait for the serve task to wind down.
    /// Session-scoped: the bridge process exits cleanly with no orphaned
    /// listener once the UI is closed / the command is interrupted.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// Exact-match, default-deny Origin gate for the loopback `/redwire`
/// upgrade (ADR 0036, adapted for loopback). An absent `Origin` is
/// rejected (a browser always sends one); a present origin must appear
/// verbatim in `allowed`. Kept pure so the policy is unit-tested without
/// a live socket. The WSS-only rule of the internet edge does *not* apply
/// here — the bridge is `127.0.0.1`-bound, so plain `ws://` is accepted.
pub fn loopback_ws_origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    match origin {
        None => false,
        Some(o) => allowed.iter().any(|a| a == o),
    }
}

/// The served origins a page loaded from this bridge will present on its
/// WebSocket upgrade — both the `127.0.0.1` literal and `localhost` form
/// of the bound port. Seeded into the allowlist so the bundle can connect
/// while a cross-site origin cannot.
fn seed_loopback_origins(port: u16) -> Vec<String> {
    vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ]
}

/// Bind a loopback HTTP server that serves the UI bundle and mounts the
/// RedWire-over-WS endpoint over `server`'s embedded engine, then spawn
/// its serve loop. Returns once the listener is bound; the returned
/// [`UiBridge`] carries the resolved address and a clean-shutdown handle.
///
/// Must be called from within a tokio runtime (it binds a tokio listener
/// and spawns the serve task).
pub async fn spawn_ui_bridge(
    server: RedDBServer,
    config: UiBridgeConfig,
) -> std::io::Result<UiBridge> {
    spawn_ui_bridge_backend(BridgeBackend::Embedded(Box::new(server)), config).await
}

/// Like [`spawn_ui_bridge`] but fronting a *remote* RedWire endpoint
/// (`red://` / `reds://`, issue #1044) rather than the embedded engine.
/// The served UI still talks only to the loopback WS endpoint; each WS
/// session opens a fresh TCP/TLS connection to `target`, and the byte
/// stream is pumped through transparently.
pub async fn spawn_ui_bridge_remote(
    target: RemoteRedwireTarget,
    config: UiBridgeConfig,
) -> std::io::Result<UiBridge> {
    spawn_ui_bridge_backend(BridgeBackend::Remote(target), config).await
}

async fn spawn_ui_bridge_backend(
    backend: BridgeBackend,
    config: UiBridgeConfig,
) -> std::io::Result<UiBridge> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port)).await?;
    let local_addr = listener.local_addr()?;

    let state = BridgeState {
        backend: Arc::new(backend),
        allowed_origins: Arc::new(seed_loopback_origins(local_addr.port())),
        ui_dir: config.ui_dir.map(Arc::new),
        injected_token: config.injected_token.map(Arc::new),
        auth_mode: config.auth_mode,
        direct_ws_url: None,
    };

    let router = axum::Router::new()
        .route(REDWIRE_WS_PATH, get(loopback_redwire_upgrade))
        .fallback(serve_ui)
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(UiBridge {
        local_addr,
        shutdown_tx: Some(shutdown_tx),
        join,
        direct_ws_url: None,
    })
}

/// Serve the UI bundle for a **direct** `red+wss://` / `red+ws://` target
/// (ADR 0047). No loopback WS relay is started — the browser connects to
/// `ws_url` directly. A `window.REDDB_WS_URL` config is injected into HTML
/// responses so the UI page knows the target without user input.
pub async fn spawn_direct_ui_server(
    ws_url: String,
    config: UiBridgeConfig,
) -> std::io::Result<UiBridge> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port)).await?;
    let local_addr = listener.local_addr()?;

    let state = BridgeState {
        backend: Arc::new(BridgeBackend::Direct),
        allowed_origins: Arc::new(vec![]),
        ui_dir: config.ui_dir.map(Arc::new),
        injected_token: config.injected_token.map(Arc::new),
        auth_mode: config.auth_mode,
        direct_ws_url: Some(Arc::new(ws_url.clone())),
    };

    // No /redwire route — the browser owns its own WS connection.
    let router = axum::Router::new().fallback(serve_ui).with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(UiBridge {
        local_addr,
        shutdown_tx: Some(shutdown_tx),
        join,
        direct_ws_url: Some(ws_url),
    })
}

/// axum handler for `GET /redwire`. Enforces the loopback Origin gate,
/// then upgrades to a binary WebSocket and runs a RedWire session over it
/// against the embedded engine (the same seam as the internet WS edge).
async fn loopback_redwire_upgrade(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());

    if !loopback_ws_origin_allowed(origin, &state.allowed_origins) {
        return (
            StatusCode::FORBIDDEN,
            "origin not allowed for loopback redwire websocket",
        )
            .into_response();
    }

    let backend = Arc::clone(&state.backend);
    let injected_token = state.injected_token.clone();
    ws.protocols([REDWIRE_WS_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            match &*backend {
                // `file://` — RedWire over the in-process embedded engine.
                BridgeBackend::Embedded(server) => {
                    if let Some(token) = injected_token.as_deref().map(String::as_str) {
                        run_injected_ws_session(socket, (**server).clone(), token).await;
                    } else {
                        run_ws_session(socket, (**server).clone()).await;
                    }
                }
                // `red://` / `reds://` — relay to a remote RedWire instance.
                BridgeBackend::Remote(target) => {
                    run_remote_ws_session(
                        socket,
                        target,
                        injected_token.as_deref().map(String::as_str),
                    )
                    .await;
                }
                // `red+wss://` / `red+ws://` — the `/redwire` route is not
                // mounted for direct targets, so this arm is unreachable.
                BridgeBackend::Direct => {
                    close_ws(socket).await;
                }
            }
        })
}

/// Bridge the binary WebSocket to a remote RedWire-over-TCP/TLS endpoint.
///
/// Opens a fresh connection to `target` per WS session and pumps the byte
/// stream straight through ([`pump_ws_stream`]). The browser sends the
/// exact native preamble (`0xFE` magic + minor + `Hello`), so the remote
/// listener's standalone session handles it unchanged — the bridge is a
/// pure byte relay and never parses RedWire frames. On a connection
/// failure the WS is closed so the UI surfaces the error rather than
/// hanging.
async fn run_remote_ws_session(
    socket: WebSocket,
    target: &RemoteRedwireTarget,
    injected_token: Option<&str>,
) {
    let addr = (target.host.as_str(), target.port);
    let tcp = match tokio::net::TcpStream::connect(addr).await {
        Ok(tcp) => tcp,
        Err(err) => {
            tracing::warn!(
                host = %target.host,
                port = target.port,
                err = %err,
                "ui bridge: connect to remote redwire target failed"
            );
            close_ws(socket).await;
            return;
        }
    };

    if !target.tls {
        if let Some(token) = injected_token {
            inject_bearer_handshake(socket, tcp, token).await;
        } else {
            pump_ws_stream(socket, tcp).await;
        }
        return;
    }

    match wrap_remote_tls(tcp, target).await {
        Ok(tls) => {
            if let Some(token) = injected_token {
                inject_bearer_handshake(socket, tls, token).await;
            } else {
                pump_ws_stream(socket, tls).await;
            }
        }
        Err(err) => {
            tracing::warn!(
                host = %target.host,
                port = target.port,
                err = %err,
                "ui bridge: TLS handshake to remote redwire target failed"
            );
            close_ws(socket).await;
        }
    }
}

/// Send a best-effort close frame on a WS the bridge is abandoning.
async fn close_ws(mut socket: WebSocket) {
    let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
}

/// Negotiate client-side TLS to a `reds://` target. Trusts the webpki
/// system roots, plus any caller-supplied CA bundle (PEM) — enough for a
/// public-cert target out of the box and a self-signed / private-CA dev
/// container when a `--tls-ca` bundle is passed. Server-only TLS (no
/// client cert): RedWire auth is negotiated inside the handshake, exactly
/// as on the native socket transports.
async fn wrap_remote_tls(
    tcp: tokio::net::TcpStream,
    target: &RemoteRedwireTarget,
) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};

    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = RootCertStore::empty();
    // Seed the webpki system roots so a public-cert `reds://` works
    // without an explicit CA.
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Add any caller-supplied CA (a self-signed dev container / private CA).
    if let Some(pem) = &target.ca_pem {
        let mut reader = std::io::BufReader::new(&pem[..]);
        for cert in rustls_pemfile::certs(&mut reader) {
            let cert = cert.map_err(std::io::Error::other)?;
            roots
                .add(cert)
                .map_err(|e| std::io::Error::other(format!("add CA cert: {e}")))?;
        }
    }

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|e| std::io::Error::other(format!("invalid TLS server name: {e}")))?;
    connector.connect(server_name, tcp).await
}

/// Static-file fallback: serve the UI bundle. With a `--ui-dir`, files are
/// read from that directory (`/` → `index.html`), guarded against path
/// traversal; without one, the embedded fixture answers `/` and
/// `/index.html` and everything else is 404.
///
/// For direct targets (`BridgeBackend::Direct`), the WS URL config
/// (`window.REDDB_WS_URL`) is injected before `</head>` in HTML responses
/// so the UI page can connect to the remote endpoint directly.
async fn serve_ui(State(state): State<BridgeState>, uri: Uri) -> Response {
    let raw = uri.path();
    let rel = raw.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    let (content_type, mut body) = match &state.ui_dir {
        None => {
            if rel == "index.html" {
                (
                    "text/html; charset=utf-8",
                    FIXTURE_INDEX.as_bytes().to_vec(),
                )
            } else {
                return not_found();
            }
        }
        Some(dir) => {
            // Reject traversal: no `..` or `.` segments, no absolute
            // re-rooting. Only plain, forward components are served.
            if rel
                .split('/')
                .any(|seg| seg == ".." || seg == "." || seg.is_empty())
            {
                return not_found();
            }
            let full = dir.join(rel);
            // Read off the async runtime — `tokio::fs` is not enabled, and
            // these are small local bundle assets on loopback anyway.
            match tokio::task::spawn_blocking(move || std::fs::read(&full)).await {
                Ok(Ok(bytes)) => (content_type_for(rel), bytes),
                _ => return not_found(),
            }
        }
    };

    // For direct targets, inject `window.REDDB_WS_URL` before </head> so
    // the UI page knows the remote WS endpoint without a loopback relay.
    if content_type.starts_with("text/html") {
        if let Some(ws_url) = &state.direct_ws_url {
            body = inject_ws_url_config(body, ws_url);
        }
        body = super::ui_auth::inject_auth_mode_config(body, state.auth_mode);
    }

    (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

/// Inject `<script>window.REDDB_WS_URL="<ws_url>";</script>` just before
/// `</head>` in an HTML document. The ws_url is constructed from a
/// validated URI (scheme + host + port) and contains no `"` or `\`, so
/// simple string interpolation is safe. Returns the original bytes
/// unchanged when `</head>` is not found.
fn inject_ws_url_config(html: Vec<u8>, ws_url: &str) -> Vec<u8> {
    let snippet = format!("<script>window.REDDB_WS_URL=\"{ws_url}\";</script>");
    let marker = b"</head>";
    match html.windows(marker.len()).position(|w| w == marker) {
        Some(pos) => {
            let mut out = Vec::with_capacity(html.len() + snippet.len());
            out.extend_from_slice(&html[..pos]);
            out.extend_from_slice(snippet.as_bytes());
            out.extend_from_slice(&html[pos..]);
            out
        }
        None => html,
    }
}

/// Guess a content type from a file extension. Minimal map covering the
/// asset kinds a UI bundle ships; anything unknown is served as opaque
/// bytes. Shared with the server-side static surface ([`super::ui_static`],
/// `red server --ui`) so the two bundle-serving paths agree on MIME types.
pub(crate) fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn content_type_response(path: &str, body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type_for(path))],
        body,
    )
        .into_response()
}

fn html_response(body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origins() -> Vec<String> {
        seed_loopback_origins(7777)
    }

    #[test]
    fn served_origin_is_allowed() {
        assert!(loopback_ws_origin_allowed(
            Some("http://127.0.0.1:7777"),
            &origins()
        ));
        assert!(loopback_ws_origin_allowed(
            Some("http://localhost:7777"),
            &origins()
        ));
    }

    #[test]
    fn missing_origin_is_rejected() {
        assert!(!loopback_ws_origin_allowed(None, &origins()));
    }

    #[test]
    fn cross_site_origin_is_rejected() {
        assert!(!loopback_ws_origin_allowed(
            Some("http://evil.example.com"),
            &origins()
        ));
        // A different port is a different origin — exact match only.
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:9999"),
            &origins()
        ));
    }

    #[test]
    fn empty_allowlist_denies_every_origin() {
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:7777"),
            &[]
        ));
    }

    // ----------------------------------------------------------------
    // Scheme classification (issue #1044). `red://` / `reds://` are
    // bridge-required remote targets and resolve to the parser's default
    // port; `file://` and bare paths stay local.
    // ----------------------------------------------------------------

    #[test]
    fn red_scheme_classifies_as_remote_plaintext_default_port() {
        // No port → the shared parser's DEFAULT_PORT_RED (5050), no TLS.
        assert_eq!(
            classify_ui_target("red://db.internal").unwrap(),
            UiTarget::Remote(RemoteRedwireTargetSpec {
                host: "db.internal".to_string(),
                port: reddb_wire::DEFAULT_PORT_RED,
                tls: false,
            })
        );
        assert_eq!(reddb_wire::DEFAULT_PORT_RED, 5050);
    }

    #[test]
    fn reds_scheme_classifies_as_remote_tls_default_port() {
        assert_eq!(
            classify_ui_target("reds://db.internal").unwrap(),
            UiTarget::Remote(RemoteRedwireTargetSpec {
                host: "db.internal".to_string(),
                port: reddb_wire::DEFAULT_PORT_RED,
                tls: true,
            })
        );
    }

    #[test]
    fn red_scheme_honours_explicit_port() {
        assert_eq!(
            classify_ui_target("red://127.0.0.1:6000").unwrap(),
            UiTarget::Remote(RemoteRedwireTargetSpec {
                host: "127.0.0.1".to_string(),
                port: 6000,
                tls: false,
            })
        );
        assert_eq!(
            classify_ui_target("reds://host:7001").unwrap(),
            UiTarget::Remote(RemoteRedwireTargetSpec {
                host: "host".to_string(),
                port: 7001,
                tls: true,
            })
        );
    }

    #[test]
    fn file_and_bare_path_classify_as_local() {
        assert_eq!(
            classify_ui_target("file:///var/lib/db.rdb").unwrap(),
            UiTarget::File
        );
        assert_eq!(classify_ui_target("./data.rdb").unwrap(), UiTarget::File);
        assert_eq!(classify_ui_target("data.rdb").unwrap(), UiTarget::File);
    }

    #[test]
    fn unsupported_scheme_is_rejected() {
        // gRPC / http / a cluster URI are not single RedWire endpoints.
        assert!(classify_ui_target("grpc://host:5055").is_err());
        assert!(classify_ui_target("http://host").is_err());
        assert!(classify_ui_target("red://a,b").is_err());
    }

    // ----------------------------------------------------------------
    // Direct targets (issue #1045, ADR 0047 direct-when-reachable).
    // `red+wss://` and `red+ws://` are browser-reachable WS endpoints —
    // no loopback relay is needed.
    // ----------------------------------------------------------------

    #[test]
    fn red_plus_wss_classifies_as_direct_default_port() {
        assert_eq!(
            classify_ui_target("red+wss://mydb.db.reddb.io").unwrap(),
            UiTarget::Direct {
                ws_url: "wss://mydb.db.reddb.io:443/redwire".to_string(),
            }
        );
    }

    #[test]
    fn red_plus_wss_with_explicit_port_classifies_as_direct() {
        assert_eq!(
            classify_ui_target("red+wss://host:5055").unwrap(),
            UiTarget::Direct {
                ws_url: "wss://host:5055/redwire".to_string(),
            }
        );
    }

    #[test]
    fn red_plus_ws_classifies_as_direct_plaintext() {
        assert_eq!(
            classify_ui_target("red+ws://host:8080").unwrap(),
            UiTarget::Direct {
                ws_url: "ws://host:8080/redwire".to_string(),
            }
        );
    }

    #[test]
    fn unsupported_scheme_error_names_supported_set() {
        let err = classify_ui_target("mongodb://host").unwrap_err();
        for scheme in ["file://", "red://", "reds://", "red+ws://", "red+wss://"] {
            assert!(
                err.contains(scheme),
                "error must mention {scheme}: got: {err}"
            );
        }
    }

    // ----------------------------------------------------------------
    // inject_ws_url_config — config injection into HTML.
    // ----------------------------------------------------------------

    #[test]
    fn inject_ws_url_inserts_before_head_close() {
        let html = b"<html><head></head><body></body></html>".to_vec();
        let out = inject_ws_url_config(html, "wss://host:443/redwire");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("<script>window.REDDB_WS_URL=\"wss://host:443/redwire\";</script></head>"),
            "snippet must appear before </head>: {s}"
        );
    }

    #[test]
    fn inject_ws_url_noop_when_no_head_close() {
        let html = b"<html><body>no head close</body></html>".to_vec();
        let orig = html.clone();
        let out = inject_ws_url_config(html, "wss://host/redwire");
        assert_eq!(out, orig, "html without </head> must be returned unchanged");
    }

    #[test]
    fn content_types_cover_bundle_assets() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("data.bin"), "application/octet-stream");
    }
}
