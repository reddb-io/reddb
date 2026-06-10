//! Local RedWire-over-WebSocket bridge for `red ui --server file://<path>`
//! (issue #1042, PRD #1041; ADR 0047 bridge, ADR 0049 RedWire-over-WS
//! transport, ADR 0036 WS edge).
//!
//! Binds a loopback (`127.0.0.1`) axum server that does two things over one
//! port:
//!   * serves a UI bundle — a `--ui-dir` directory, or the checked-in
//!     [`fixture`](ui_bridge_fixture/index.html) page — over plain HTTP, and
//!   * mounts `/redwire`, the same RedWire-over-WebSocket endpoint as the
//!     internet edge (ADR 0036), over the **embedded engine** opened from the
//!     file. The async-transport ↔ sync-engine seam is reused verbatim via
//!     [`super::ws_edge::run_ws_session`]: for `file://` this serves RedWire
//!     over the embedded engine, *not* a proxy of the HTTP surface.
//!
//! ## Origin gate (loopback variant)
//!
//! ADR 0036's WS endpoint is default-deny on an `Origin` allowlist, and the
//! internet edge additionally demands TLS (`wss://`). The loopback bridge
//! relaxes the TLS rule **only** — it is bound to `127.0.0.1`, so a plain
//! `ws://` upgrade from the page it just served is accepted — while keeping
//! the allowlist default-deny, seeded with the bridge's own served origins
//! (`http://127.0.0.1:<port>` and `http://localhost:<port>`). The gate is
//! [`loopback_ws_origin_allowed`], kept distinct from
//! `ws_edge::ws_upgrade_decision` (the internet edge, HTTPS-required).
//!
//! The bridge is session-scoped: dropping the returned [`UiBridge`] (or
//! calling [`UiBridge::shutdown`]) triggers a graceful shutdown and releases
//! the port, so an interrupted `red ui` leaves no orphaned listener.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::oneshot;

use super::ws_edge::{run_ws_session, REDWIRE_WS_PATH, REDWIRE_WS_SUBPROTOCOL};
use super::RedDBServer;

/// The single checked-in fixture page, embedded so the bridge can serve a
/// working UI with no on-disk bundle.
const FIXTURE_INDEX: &str = include_str!("ui_bridge_fixture/index.html");

/// Where the bridge reads the UI bundle from.
#[derive(Clone, Debug)]
enum UiSource {
    /// Serve files from a directory on disk (`--ui-dir`).
    Dir(PathBuf),
    /// Serve the single checked-in fixture page (no `--ui-dir`).
    Fixture,
}

/// Configuration for [`spawn_ui_bridge`].
#[derive(Clone, Debug, Default)]
pub struct UiBridgeConfig {
    /// TCP port to bind on `127.0.0.1`. `0` selects an ephemeral port.
    pub port: u16,
    /// Optional directory whose files are served as the UI bundle. When
    /// `None`, the checked-in fixture page is served at `/`.
    pub ui_dir: Option<PathBuf>,
}

/// A running loopback bridge. Dropping it (or calling [`UiBridge::shutdown`])
/// stops the server and releases the port — the bridge is session-scoped.
pub struct UiBridge {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl UiBridge {
    /// The bound loopback address (with the resolved port, even when `0` was
    /// requested).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// URL the default browser is pointed at (the UI bundle root).
    pub fn ui_url(&self) -> String {
        format!("http://{}/", self.local_addr)
    }

    /// URL of the RedWire-over-WebSocket endpoint the served page connects to.
    pub fn ws_url(&self) -> String {
        format!("ws://{}{}", self.local_addr, REDWIRE_WS_PATH)
    }

    /// Trigger a graceful shutdown and wait for the server task to finish.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for UiBridge {
    fn drop(&mut self) {
        // Best-effort teardown if the caller never awaited `shutdown()`:
        // signal graceful stop and abort the task so the port is released
        // even on an interrupted / panicking path.
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The default-deny `Origin` allowlist for the loopback bridge: only the two
/// origins the bridge itself serves the bundle from.
fn loopback_origin_allowlist(port: u16) -> Vec<String> {
    vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ]
}

/// Loopback WS origin gate (ADR 0036, loopback variant). Default-deny: a
/// missing `Origin` (no browser context) or one outside the allowlist is
/// refused. TLS is **not** required here — the only relaxation from
/// `ws_edge::ws_upgrade_decision` — because the bridge is `127.0.0.1`-bound.
pub(crate) fn loopback_ws_origin_allowed(origin: Option<&str>, allowlist: &[String]) -> bool {
    match origin {
        Some(o) => allowlist.iter().any(|allowed| allowed == o),
        None => false,
    }
}

/// State threaded into the bridge's axum handlers.
#[derive(Clone)]
struct BridgeState {
    server: RedDBServer,
    allowed_origins: Arc<Vec<String>>,
    ui: UiSource,
}

/// `GET /redwire` — validate the loopback origin gate, then upgrade to a
/// binary WebSocket and run a RedWire session over the embedded engine.
async fn redwire_upgrade(
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

/// Catch-all static handler for the UI bundle.
async fn serve_ui(State(state): State<BridgeState>, uri: Uri) -> Response {
    serve_bundle_path(&state.ui, uri.path())
}

/// Resolve one request path against the configured [`UiSource`]. Pure so the
/// path mapping (default-to-index, traversal rejection, content type) is
/// unit-tested without binding a socket.
fn serve_bundle_path(source: &UiSource, req_path: &str) -> Response {
    let trimmed = req_path.trim_start_matches('/');
    let rel = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    match source {
        UiSource::Fixture => {
            if rel == "index.html" {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    FIXTURE_INDEX,
                )
                    .into_response()
            } else {
                (StatusCode::NOT_FOUND, "not found").into_response()
            }
        }
        UiSource::Dir(dir) => {
            // Reject path traversal before touching the filesystem: any `..`
            // segment (or an absolute component) escapes the bundle root.
            if rel
                .split('/')
                .any(|seg| seg == ".." || seg == "." || seg.is_empty())
            {
                return (StatusCode::FORBIDDEN, "invalid path").into_response();
            }
            let full = dir.join(rel);
            match std::fs::read(&full) {
                Ok(bytes) => {
                    let content_type = content_type_for(&full);
                    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
                }
                Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
            }
        }
    }
}

/// Minimal extension → MIME map for the static bundle. Unknown extensions
/// fall back to `application/octet-stream`.
fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Bind the loopback bridge and start serving the UI bundle + `/redwire`
/// endpoint over `server`'s embedded engine. The returned [`UiBridge`] owns
/// the server task and tears it down on `shutdown()`/drop.
pub async fn spawn_ui_bridge(
    server: RedDBServer,
    config: UiBridgeConfig,
) -> std::io::Result<UiBridge> {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config.port);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;

    let allowed_origins = Arc::new(loopback_origin_allowlist(local_addr.port()));
    let ui = match config.ui_dir {
        Some(dir) => UiSource::Dir(dir),
        None => UiSource::Fixture,
    };

    let state = BridgeState {
        server,
        allowed_origins,
        ui,
    };

    let router = axum::Router::new()
        .route(REDWIRE_WS_PATH, get(redwire_upgrade))
        .fallback(serve_ui)
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(UiBridge {
        local_addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist() -> Vec<String> {
        loopback_origin_allowlist(8080)
    }

    #[test]
    fn served_origins_are_on_the_allowlist() {
        let list = allowlist();
        assert!(loopback_ws_origin_allowed(
            Some("http://127.0.0.1:8080"),
            &list
        ));
        assert!(loopback_ws_origin_allowed(
            Some("http://localhost:8080"),
            &list
        ));
    }

    #[test]
    fn missing_origin_is_default_denied() {
        assert!(!loopback_ws_origin_allowed(None, &allowlist()));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        // A page on another site (CSWSH attempt) is refused even though the
        // bridge speaks plain ws://.
        assert!(!loopback_ws_origin_allowed(
            Some("http://evil.example.com"),
            &allowlist()
        ));
        // Different port than the one we served from is also rejected.
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:9999"),
            &allowlist()
        ));
    }

    #[test]
    fn empty_allowlist_rejects_everything() {
        assert!(!loopback_ws_origin_allowed(
            Some("http://127.0.0.1:8080"),
            &[]
        ));
    }

    #[test]
    fn fixture_serves_index_at_root_and_explicit_path() {
        let root = serve_bundle_path(&UiSource::Fixture, "/");
        assert_eq!(root.status(), StatusCode::OK);
        let index = serve_bundle_path(&UiSource::Fixture, "/index.html");
        assert_eq!(index.status(), StatusCode::OK);
    }

    #[test]
    fn fixture_404s_unknown_paths() {
        let resp = serve_bundle_path(&UiSource::Fixture, "/nope.js");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn dir_source_rejects_traversal() {
        let resp = serve_bundle_path(
            &UiSource::Dir(PathBuf::from("/tmp/bundle")),
            "/../etc/passwd",
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn content_type_mapping() {
        assert_eq!(
            content_type_for(Path::new("a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("a/app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("a/x.bin")),
            "application/octet-stream"
        );
    }
}
