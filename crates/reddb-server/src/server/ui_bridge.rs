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

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::oneshot;

use super::ws_edge::{REDWIRE_WS_PATH, REDWIRE_WS_SUBPROTOCOL};
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
}

/// State threaded into the bridge's axum handlers. Cheap to clone (the
/// server clone shares its inner `Arc`s; the origin list and bundle dir
/// are shared via `Arc`).
#[derive(Clone)]
struct BridgeState {
    server: RedDBServer,
    allowed_origins: Arc<Vec<String>>,
    ui_dir: Option<Arc<PathBuf>>,
}

/// A running loopback UI bridge. Holds the bound address plus the handles
/// needed to shut the serve task down cleanly. Dropping it without
/// calling [`Self::shutdown`] aborts the serve task on drop of the join
/// handle's runtime; prefer `shutdown().await` for an orderly teardown.
pub struct UiBridge {
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
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

    /// URL of the RedWire-over-WebSocket endpoint the served page opens a
    /// session against.
    pub fn ws_url(&self) -> String {
        format!("ws://{}{}", self.local_addr, REDWIRE_WS_PATH)
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
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port)).await?;
    let local_addr = listener.local_addr()?;

    let state = BridgeState {
        server,
        allowed_origins: Arc::new(seed_loopback_origins(local_addr.port())),
        ui_dir: config.ui_dir.map(Arc::new),
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

    let server = state.server.clone();
    ws.protocols([REDWIRE_WS_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            super::ws_edge::run_ws_session(socket, server).await;
        })
}

/// Static-file fallback: serve the UI bundle. With a `--ui-dir`, files are
/// read from that directory (`/` → `index.html`), guarded against path
/// traversal; without one, the embedded fixture answers `/` and
/// `/index.html` and everything else is 404.
async fn serve_ui(State(state): State<BridgeState>, uri: Uri) -> Response {
    let raw = uri.path();
    let rel = raw.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    match &state.ui_dir {
        None => {
            if rel == "index.html" {
                html_response(FIXTURE_INDEX.as_bytes().to_vec())
            } else {
                not_found()
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
                Ok(Ok(bytes)) => content_type_response(rel, bytes),
                _ => not_found(),
            }
        }
    }
}

/// Guess a content type from a file extension. Minimal map covering the
/// asset kinds a UI bundle ships; anything unknown is served as opaque
/// bytes.
fn content_type_for(path: &str) -> &'static str {
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

    #[test]
    fn content_types_cover_bundle_assets() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("data.bin"), "application/octet-stream");
    }
}
