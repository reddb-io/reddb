//! Local `red ui` bridge for `file://` targets (issue #1042, PRD #1041).
//!
//! `red ui --server file://<path>` opens a graphical UI in the browser
//! against a local `.rdb`. The browser can only speak HTTP/WS, so per
//! ADR 0047 the `red` binary is the **launcher + bridge**: it stands up a
//! local endpoint on `127.0.0.1:<port>` that
//!
//!   * serves the UI bundle (a directory via `--ui-dir`, or a checked-in
//!     minimal fixture page) over plain HTTP, and
//!   * fronts the **embedded engine** opened from the file with a
//!     RedWire-over-WebSocket endpoint at `/redwire` (ADR 0049) — *not* a
//!     proxy of the HTTP surface. The WS data channel is bridged into the
//!     transport-agnostic [`run_ws_session`] seam (ADR 0036), exactly the
//!     same async-transport ↔ sync-engine bridge the internet-facing WS
//!     edge uses.
//!
//! Security (ADR 0036): the WS upgrade is **default-deny** on the `Origin`
//! header. Where the internet-exposed edge additionally demands WSS, the
//! loopback bridge accepts plain `ws://` — the endpoint is bound to
//! `127.0.0.1` and never reachable off-box — but the Origin allowlist is
//! still enforced exactly. The bridge seeds the allowlist with its own
//! served loopback origins so the page it serves can connect, and rejects
//! every other (or missing) `Origin`, which is the Cross-Site WebSocket
//! Hijacking defence the ADR mandates.

use std::io;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::oneshot;

use super::ws_edge::{run_ws_session, REDWIRE_WS_PATH, REDWIRE_WS_SUBPROTOCOL};
use super::RedDBServer;

/// Embedded fixture bundle served when no `--ui-dir` is supplied. A
/// minimal page that opens a RedWire-over-WebSocket session against the
/// embedded engine and runs a query, plus the node-free browser codec it
/// imports (ADR 0036 `redwire-core.js` and its `protocol.js` /
/// `core/errors.js` closure). Checked-in fixture for the tracer-bullet
/// slice; runtime bundle download lands in a later slice.
const FIXTURE_INDEX_HTML: &str = include_str!("ui_bridge_fixture/index.html");
const DRIVER_REDWIRE_CORE_JS: &str =
    include_str!("../../../../drivers/js-client/src/redwire-core.js");
const DRIVER_PROTOCOL_JS: &str = include_str!("../../../../drivers/js-client/src/protocol.js");
const DRIVER_ERRORS_JS: &str = include_str!("../../../../drivers/js-client/src/core/errors.js");

/// Canonicalize a `file://` URI to an absolute `file:///…` form.
///
/// A relative path (`file://./relative.rdb`, `file://data/x.rdb`) is
/// resolved against the current working directory and lexically cleaned
/// into an absolute `file:///abs/path` target before anything downstream
/// uses it (ADR 0047). An already-absolute `file:///abs/path` is returned
/// normalised. Resolution is purely lexical — the file need not exist
/// yet, matching the connection-string parser's side-effect-free shape.
pub fn canonicalize_file_uri(uri: &str) -> Result<String, String> {
    // Lowercase only the scheme so `FILE://…` dispatches identically; the
    // path keeps its original casing.
    let scheme_end = uri
        .find("://")
        .ok_or_else(|| format!("not a file uri: {uri}"))?;
    let scheme = uri[..scheme_end].to_ascii_lowercase();
    if scheme != "file" {
        return Err(format!("not a file uri: {uri}"));
    }
    let rest = &uri[scheme_end + 3..];
    if rest.is_empty() {
        return Err("file:// URI is missing a path".to_string());
    }

    let raw = PathBuf::from(rest);
    let absolute = if raw.is_absolute() {
        raw
    } else {
        let cwd = std::env::current_dir().map_err(|e| format!("cannot read current dir: {e}"))?;
        cwd.join(raw)
    };

    let cleaned = lexically_clean(&absolute);
    let path_str = cleaned
        .to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", cleaned.display()))?;

    // An absolute POSIX path already begins with `/`, so `file://` + path
    // yields the canonical triple-slash form `file:///abs/path`.
    Ok(format!("file://{path_str}"))
}

/// Lexically resolve `.`/`..` components without touching the filesystem
/// (`std::fs::canonicalize` would require the path to exist). The leading
/// `RootDir` is preserved so the result stays absolute.
fn lexically_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop a normal segment, but never above the root.
                if out
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, Component::Normal(_)))
                {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Decide whether a `/redwire` WS upgrade may proceed on the loopback
/// bridge. Default-deny on the `Origin` header (ADR 0036): a missing
/// origin or one absent from the allowlist is rejected; only an exact
/// match is accepted. WSS is *not* required here — the endpoint is bound
/// to loopback only (see the module docs).
pub fn loopback_ws_origin_allowed(origin: Option<&str>, allowlist: &[String]) -> bool {
    match origin {
        Some(o) => allowlist.iter().any(|allowed| allowed == o),
        None => false,
    }
}

/// The loopback origins the bridge serves the UI on for a given bound
/// port. The page loaded from `http://127.0.0.1:<port>/` (or the
/// `localhost` alias) carries one of these as its `Origin`, so seeding
/// the allowlist with them lets the served page connect while every other
/// origin stays default-denied.
pub fn loopback_origins_for_port(port: u16) -> Vec<String> {
    vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ]
}

/// Configuration for a local `red ui` bridge.
#[derive(Debug, Clone)]
pub struct UiBridgeConfig {
    /// Address to bind. Use port `0` for an ephemeral port (the test and
    /// the default CLI path both do).
    pub bind: SocketAddr,
    /// Directory holding the UI bundle to serve. `None` serves the
    /// checked-in minimal fixture page.
    pub ui_dir: Option<PathBuf>,
    /// Extra `Origin`s to allow on the WS upgrade beyond the bridge's own
    /// loopback origins (rarely needed; empty by default).
    pub extra_allowed_origins: Vec<String>,
}

impl Default for UiBridgeConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            ui_dir: None,
            extra_allowed_origins: Vec::new(),
        }
    }
}

/// Shared state threaded into the bridge handlers.
#[derive(Clone)]
struct BridgeState {
    server: RedDBServer,
    ui_dir: Option<Arc<PathBuf>>,
    allowed_origins: Arc<Vec<String>>,
}

/// A running bridge. Holds the bound address and a shutdown handle; the
/// background task tears the listener down cleanly when [`Self::shutdown`]
/// is called or the handle is dropped.
pub struct UiBridge {
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

impl UiBridge {
    /// The address the bridge bound to (resolves an ephemeral `:0` port).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The `http://…/` URL a browser should open to load the UI.
    pub fn ui_url(&self) -> String {
        format!("http://{}/", self.local_addr)
    }

    /// The `ws://…/redwire` URL the UI uses for the RedWire transport.
    pub fn ws_url(&self) -> String {
        format!("ws://{}{}", self.local_addr, REDWIRE_WS_PATH)
    }

    /// Signal the bridge to stop and wait for the listener task to wind
    /// down. Idempotent-ish: a second call simply awaits the handle.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.handle).await;
    }
}

/// Bind the bridge listener, derive the loopback origin allowlist for the
/// bound port, build the router, and spawn the server on the current
/// tokio runtime. Returns once the socket is listening.
pub async fn spawn_ui_bridge(server: RedDBServer, config: UiBridgeConfig) -> io::Result<UiBridge> {
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let local_addr = listener.local_addr()?;

    let mut allowed = loopback_origins_for_port(local_addr.port());
    allowed.extend(config.extra_allowed_origins);

    let state = BridgeState {
        server,
        ui_dir: config.ui_dir.map(Arc::new),
        allowed_origins: Arc::new(allowed),
    };
    let router = build_bridge_router(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(err) = serve.await {
            tracing::warn!(target: "reddb::ui_bridge", error = %err, "ui bridge serve ended");
        }
    });

    Ok(UiBridge {
        local_addr,
        shutdown_tx: Some(shutdown_tx),
        handle,
    })
}

/// Build the axum router: the `/redwire` WS upgrade over the embedded
/// engine plus the static UI fallback.
fn build_bridge_router(state: BridgeState) -> axum::Router {
    axum::Router::new()
        .route(REDWIRE_WS_PATH, axum::routing::get(redwire_ws_upgrade))
        .fallback(serve_ui)
        .with_state(state)
}

/// `GET /redwire`: enforce the default-deny Origin gate, then upgrade to a
/// binary WebSocket and run a RedWire session over the embedded engine.
async fn redwire_ws_upgrade(
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
            "origin not allowed for redwire websocket",
        )
            .into_response();
    }

    let server = state.server.clone();
    ws.protocols([REDWIRE_WS_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            run_ws_session(socket, server).await;
        })
}

/// Static UI fallback. Serves the `--ui-dir` bundle when configured,
/// otherwise the checked-in minimal fixture set.
async fn serve_ui(State(state): State<BridgeState>, req: Request) -> Response {
    let raw_path = req.uri().path();
    let rel = raw_path.trim_start_matches('/');

    match state.ui_dir.as_deref() {
        Some(dir) => serve_from_dir(dir, rel),
        None => serve_fixture(rel),
    }
}

/// Serve `rel` from `dir`, defaulting an empty path to `index.html` and
/// rejecting any path that escapes the directory.
fn serve_from_dir(dir: &Path, rel: &str) -> Response {
    let rel = if rel.is_empty() { "index.html" } else { rel };

    // Reject traversal: only plain, non-`..` components are allowed.
    let requested = PathBuf::from(rel);
    if requested
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    let full = dir.join(&requested);

    match std::fs::read(&full) {
        Ok(bytes) => file_response(rel, bytes),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Serve the embedded fixture set by virtual path.
fn serve_fixture(rel: &str) -> Response {
    let (body, content_type): (&str, &str) = match rel {
        "" | "index.html" => (FIXTURE_INDEX_HTML, "text/html; charset=utf-8"),
        "redwire-core.js" => (DRIVER_REDWIRE_CORE_JS, JS_CONTENT_TYPE),
        "protocol.js" => (DRIVER_PROTOCOL_JS, JS_CONTENT_TYPE),
        "core/errors.js" => (DRIVER_ERRORS_JS, JS_CONTENT_TYPE),
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    ([(header::CONTENT_TYPE, content_type)], body.to_string()).into_response()
}

const JS_CONTENT_TYPE: &str = "text/javascript; charset=utf-8";

/// Build a byte response with a content-type guessed from the extension.
fn file_response(rel: &str, bytes: Vec<u8>) -> Response {
    let content_type = content_type_for(rel);
    ([(header::CONTENT_TYPE, content_type)], Body::from(bytes)).into_response()
}

/// Minimal extension → content-type map (no `mime_guess` dependency).
fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => JS_CONTENT_TYPE,
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- file:// canonicalization (AC: relative→absolute) ---------------

    #[test]
    fn relative_file_uri_resolves_to_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let out = canonicalize_file_uri("file://./relative.rdb").unwrap();
        let expected = format!("file://{}/relative.rdb", cwd.display());
        assert_eq!(out, expected);
        assert!(
            out.starts_with("file:///"),
            "must be triple-slash absolute: {out}"
        );
    }

    #[test]
    fn bare_relative_file_uri_resolves_to_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let out = canonicalize_file_uri("file://data/reddb.rdb").unwrap();
        let expected = format!("file://{}/data/reddb.rdb", cwd.display());
        assert_eq!(out, expected);
    }

    #[test]
    fn absolute_file_uri_is_preserved() {
        let out = canonicalize_file_uri("file:///var/lib/reddb/data.rdb").unwrap();
        assert_eq!(out, "file:///var/lib/reddb/data.rdb");
    }

    #[test]
    fn dotdot_components_are_lexically_cleaned() {
        let out = canonicalize_file_uri("file:///var/lib/../lib/reddb/./data.rdb").unwrap();
        assert_eq!(out, "file:///var/lib/reddb/data.rdb");
    }

    #[test]
    fn uppercase_scheme_is_accepted() {
        let out = canonicalize_file_uri("FILE:///abs/x.rdb").unwrap();
        assert_eq!(out, "file:///abs/x.rdb");
    }

    #[test]
    fn non_file_uri_is_rejected() {
        assert!(canonicalize_file_uri("red://host:5050").is_err());
        assert!(canonicalize_file_uri("file://").is_err());
    }

    // -- default-deny Origin allowlist (ADR 0036) -----------------------

    fn allowlist() -> Vec<String> {
        loopback_origins_for_port(7777)
    }

    #[test]
    fn served_loopback_origin_is_allowed() {
        assert!(loopback_ws_origin_allowed(
            Some("http://127.0.0.1:7777"),
            &allowlist()
        ));
        assert!(loopback_ws_origin_allowed(
            Some("http://localhost:7777"),
            &allowlist()
        ));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert!(!loopback_ws_origin_allowed(
            Some("https://evil.example.com"),
            &allowlist()
        ));
        // Wrong port is a distinct origin and must not slip through.
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:9999"),
            &allowlist()
        ));
    }

    #[test]
    fn missing_origin_is_rejected() {
        assert!(!loopback_ws_origin_allowed(None, &allowlist()));
    }

    #[test]
    fn empty_allowlist_rejects_every_origin() {
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:7777"),
            &[]
        ));
        assert!(!loopback_ws_origin_allowed(None, &[]));
    }
}
