//! Credential handoff for `red ui` (issue #1048, PRD #1041; ADR 0051 auth
//! model, ADR 0036 handshake auth).
//!
//! When the user supplies a token, `red` owns it and the UI never sees it:
//!
//!   * **Served-UI path** (`red ui <uri> --token <X>`): the token is held in
//!     this process and presented in the RedWire handshake (ADR 0036 bearer)
//!     by the loopback bridge — the UI runs in *injected-auth* mode and never
//!     sees or persists the secret. The injection itself lives in
//!     [`super::ui_bridge`]; this module owns the *mode decision* and the
//!     credential-free config snippet served into the page.
//!   * **Deep-link / desktop path**: the token crosses via a *local secret
//!     channel* — a one-time loopback fetch keyed by a single-use nonce
//!     ([`OneTimeSecret`] + [`spawn_handoff_server`]). The dispatched deep-link
//!     URL carries only the nonce/handoff URL, never the secret, so nothing
//!     lands in `ps`, shell history, or URL logs.
//!
//! The database's auth configuration is the source of truth for whether the
//! UI prompts ([`UiAuthMode::resolve`]): an authenticated DB with no supplied
//! token → the UI prompts via its own connect flow; an unauthenticated DB →
//! no prompt.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::oneshot;

/// What the served UI should do about authentication, decided by `red` from
/// the database's auth configuration and whether a token was supplied. The
/// mode is injected into the page (credential-free) so the UI knows whether
/// to prompt without ever holding the secret itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAuthMode {
    /// A token was supplied: `red` holds it and presents it in the RedWire
    /// handshake. The UI must **not** prompt and never sees the secret.
    Injected,
    /// No token, but the database requires auth: the UI prompts for
    /// credentials through its own connect flow.
    Prompt,
    /// No token and the database is unauthenticated: the UI connects
    /// anonymously with no prompt.
    Open,
}

impl UiAuthMode {
    /// Resolve the mode from whether a credential was supplied and whether the
    /// target database requires authentication. A supplied token always wins
    /// (injected-auth); otherwise the DB's auth config decides prompt vs open.
    pub fn resolve(token_supplied: bool, db_auth_required: bool) -> Self {
        match (token_supplied, db_auth_required) {
            (true, _) => UiAuthMode::Injected,
            (false, true) => UiAuthMode::Prompt,
            (false, false) => UiAuthMode::Open,
        }
    }

    /// The stable wire string injected as `window.REDDB_AUTH_MODE`. Kept
    /// lowercase and hyphen-free so it is a clean JS string literal.
    pub fn as_str(self) -> &'static str {
        match self {
            UiAuthMode::Injected => "injected",
            UiAuthMode::Prompt => "prompt",
            UiAuthMode::Open => "open",
        }
    }
}

/// Build the `<script>` snippet that tells the served page which auth mode to
/// run in. **Never carries the token** — only the mode hint. Injected before
/// `</head>` by the bridge's static-file handler.
pub fn auth_mode_config_snippet(mode: UiAuthMode) -> String {
    format!(
        "<script>window.REDDB_AUTH_MODE=\"{}\";</script>",
        mode.as_str()
    )
}

/// Insert [`auth_mode_config_snippet`] just before `</head>` in an HTML
/// document. Returns the original bytes unchanged when `</head>` is absent.
/// The mode string is a fixed enum rendering (no `"`/`\`), so plain
/// interpolation is safe.
pub fn inject_auth_mode_config(html: Vec<u8>, mode: UiAuthMode) -> Vec<u8> {
    let snippet = auth_mode_config_snippet(mode);
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

// ---------------------------------------------------------------------------
// One-time secret channel for the deep-link / desktop path.
// ---------------------------------------------------------------------------

/// A single-use nonce, hex-encoded from 16 bytes of OS CSPRNG. Used to key the
/// one-time loopback handoff so the deep-link URL can carry the nonce (a
/// throwaway lookup key) instead of the secret.
pub fn new_handoff_nonce() -> String {
    let mut bytes = [0u8; 16];
    // CSPRNG; on the astronomically unlikely fill error, fall back to a
    // process/address-derived value — still single-use and never the secret.
    if crate::crypto::os_random::fill_bytes(&mut bytes).is_err() {
        let seed = (&bytes as *const _ as usize) as u64;
        bytes[..8].copy_from_slice(&seed.to_le_bytes());
    }
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push(nibble_hex(b >> 4));
        out.push(nibble_hex(b & 0x0f));
    }
    out
}

fn nibble_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + (n - 10)) as char,
    }
}

/// A credential that can be fetched **exactly once**. The desktop app fetches
/// it from the loopback handoff endpoint; a second fetch (a replay, or a
/// stray request) gets nothing. Thread-safe so the handoff server's handler
/// and the waiting CLI can share it.
#[derive(Debug)]
pub struct OneTimeSecret {
    inner: Mutex<Option<String>>,
}

impl OneTimeSecret {
    /// Wrap a secret for single-use retrieval.
    pub fn new(secret: String) -> Self {
        Self {
            inner: Mutex::new(Some(secret)),
        }
    }

    /// Take the secret, leaving the channel empty. Returns `None` if already
    /// taken (replay) — the first caller wins and no one else sees it.
    pub fn take(&self) -> Option<String> {
        self.inner.lock().expect("one-time secret lock").take()
    }

    /// Whether the secret has already been consumed.
    pub fn is_consumed(&self) -> bool {
        self.inner.lock().expect("one-time secret lock").is_none()
    }
}

// ---------------------------------------------------------------------------
// Loopback handoff server — serves the one-time secret to the desktop app.
// ---------------------------------------------------------------------------

/// Shared state for the handoff server's single route.
#[derive(Clone)]
struct HandoffState {
    /// The single-use nonce the path must match (constant-time compared).
    nonce: Arc<String>,
    /// The credential, retrievable exactly once.
    secret: Arc<OneTimeSecret>,
}

/// A running loopback server that hands the held credential to the desktop
/// app exactly once, keyed by a single-use nonce. The deep-link URL carries
/// only [`Self::handoff_url`] (host/port + nonce) — never the secret — so
/// nothing sensitive lands in `ps`, shell history, or URL logs.
pub struct HandoffServer {
    local_addr: SocketAddr,
    nonce: String,
    secret: Arc<OneTimeSecret>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl HandoffServer {
    /// The loopback URL the desktop app fetches the credential from. Carries
    /// the nonce (a throwaway lookup key), never the secret.
    pub fn handoff_url(&self) -> String {
        format!("http://{}/handoff/{}", self.local_addr, self.nonce)
    }

    /// The bound loopback address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Whether the credential has been fetched (the handoff completed).
    pub fn is_consumed(&self) -> bool {
        self.secret.is_consumed()
    }

    /// Signal graceful shutdown and wait for the serve task to wind down.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// Bind a loopback handoff server holding `token`, retrievable once via the
/// nonce-keyed path. Returns once the listener is bound. Must be called from
/// within a tokio runtime.
pub async fn spawn_handoff_server(token: String) -> std::io::Result<HandoffServer> {
    let nonce = new_handoff_nonce();
    let secret = Arc::new(OneTimeSecret::new(token));

    let state = HandoffState {
        nonce: Arc::new(nonce.clone()),
        secret: Arc::clone(&secret),
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_addr = listener.local_addr()?;

    let router = axum::Router::new()
        .route("/handoff/{nonce}", get(serve_handoff))
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(HandoffServer {
        local_addr,
        nonce,
        secret,
        shutdown_tx: Some(shutdown_tx),
        join,
    })
}

/// `GET /handoff/{nonce}` — return the credential once when the nonce matches
/// (constant-time), 404 otherwise or after it has been consumed. A
/// `Cache-Control: no-store` header keeps the secret out of any intermediary
/// (there are none on loopback, but defence in depth).
async fn serve_handoff(State(state): State<HandoffState>, Path(nonce): Path<String>) -> Response {
    if !crate::crypto::constant_time_eq(nonce.as_bytes(), state.nonce.as_bytes()) {
        return not_found();
    }
    match state.secret.take() {
        Some(token) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            token,
        )
            .into_response(),
        None => not_found(),
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_supplied_token_is_always_injected() {
        assert_eq!(UiAuthMode::resolve(true, true), UiAuthMode::Injected);
        assert_eq!(UiAuthMode::resolve(true, false), UiAuthMode::Injected);
    }

    #[test]
    fn resolve_no_token_follows_db_auth_config() {
        // Authenticated DB, no token → the UI prompts.
        assert_eq!(UiAuthMode::resolve(false, true), UiAuthMode::Prompt);
        // Unauthenticated DB, no token → no prompt.
        assert_eq!(UiAuthMode::resolve(false, false), UiAuthMode::Open);
    }

    #[test]
    fn auth_mode_strings_are_stable() {
        assert_eq!(UiAuthMode::Injected.as_str(), "injected");
        assert_eq!(UiAuthMode::Prompt.as_str(), "prompt");
        assert_eq!(UiAuthMode::Open.as_str(), "open");
    }

    #[test]
    fn config_snippet_never_carries_a_token() {
        // The snippet is mode-only; no token argument exists to leak.
        for mode in [UiAuthMode::Injected, UiAuthMode::Prompt, UiAuthMode::Open] {
            let snippet = auth_mode_config_snippet(mode);
            assert!(snippet.contains(mode.as_str()));
            assert!(!snippet.to_ascii_lowercase().contains("token"));
            assert!(!snippet.to_ascii_lowercase().contains("bearer"));
        }
    }

    #[test]
    fn inject_auth_mode_inserts_before_head_close() {
        let html = b"<html><head></head><body></body></html>".to_vec();
        let out = inject_auth_mode_config(html, UiAuthMode::Injected);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("<script>window.REDDB_AUTH_MODE=\"injected\";</script></head>"),
            "snippet must appear before </head>: {s}"
        );
    }

    #[test]
    fn inject_auth_mode_noop_without_head_close() {
        let html = b"<html><body>no head</body></html>".to_vec();
        let orig = html.clone();
        assert_eq!(inject_auth_mode_config(html, UiAuthMode::Prompt), orig);
    }

    #[test]
    fn handoff_nonce_is_32_hex_chars_and_varies() {
        let a = new_handoff_nonce();
        let b = new_handoff_nonce();
        assert_eq!(a.len(), 32, "nonce is 16 bytes hex-encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Two CSPRNG draws collide with probability 2^-128 — treat equality
        // as a generator fault.
        assert_ne!(a, b, "nonces must be unique per draw");
    }

    #[test]
    fn one_time_secret_yields_once_then_empty() {
        let secret = OneTimeSecret::new("rk_supersecret".to_string());
        assert!(!secret.is_consumed());
        assert_eq!(secret.take().as_deref(), Some("rk_supersecret"));
        assert!(secret.is_consumed());
        // A replay gets nothing — the channel is single-use.
        assert_eq!(secret.take(), None);
    }
}
