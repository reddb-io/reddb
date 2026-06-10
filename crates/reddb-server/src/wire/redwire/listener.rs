//! TCP / TLS / Unix listeners for the RedWire protocol.
//!
//! Each accepted connection spawns a `handle_session` task. The
//! RedWire magic (`0xFE`) is always read here off the wire: the
//! standalone listeners read it before the session loop, and the
//! in-process demux (issue #933) only *peeks* to classify, so the
//! magic is still buffered when `handle_router_connection` reads it.

use std::io;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;

use super::session::handle_session;
use super::validate_startup_magic;

#[derive(Clone)]
pub struct RedWireConfig {
    pub bind_addr: String,
    pub auth_store: Option<Arc<AuthStore>>,
    /// Optional OAuth/OIDC validator. When set, clients can
    /// negotiate `oauth-jwt` in their `Hello.auth_methods` and
    /// the handshake will validate the JWT against this issuer
    /// + JWKS.
    pub oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
}

impl std::fmt::Debug for RedWireConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedWireConfig")
            .field("bind_addr", &self.bind_addr)
            .field("auth_store_present", &self.auth_store.is_some())
            .field("oauth_present", &self.oauth.is_some())
            .finish()
    }
}

/// Start a plain-TCP RedWire listener on the configured bind addr.
pub async fn start_redwire_listener(
    config: RedWireConfig,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(transport = "redwire", bind = %config.bind_addr, "listener online");
    serve_redwire_tcp(listener, runtime, config.auth_store, config.oauth).await
}

/// Start a RedWire listener on an already-bound TCP listener (used by
/// service_cli's spawn_wire_listeners when binding the user-facing wire
/// port directly; the shared-port demux instead dispatches per connection
/// through [`handle_router_connection`]).
///
/// Pulls the auth store off the runtime so bearer tokens issued by the
/// HTTP `/auth/login` endpoint are honoured on the wire transport too —
/// otherwise every authenticated client gets "bearer auth refused — server
/// has no auth store configured" even when `--vault true` is set.
pub async fn start_redwire_listener_on(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let auth_store = runtime.auth_store();
    serve_redwire_tcp(listener, runtime, auth_store, None).await
}

async fn serve_redwire_tcp(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
    oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let rt = runtime.clone();
        let auth = auth_store.clone();
        let oauth = oauth.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(err) = handle_standalone(stream, rt, auth, oauth).await {
                tracing::warn!(
                    transport = "redwire",
                    peer = %peer_str,
                    err = %err,
                    "session ended with error"
                );
            }
        });
    }
}

/// Start a RedWire listener on a Unix domain socket.
///
/// Accepts connections from `unix://path` URLs or plain filesystem paths.
/// Existing socket files are removed before bind.
#[cfg(unix)]
pub async fn start_redwire_unix_listener(
    socket_path: &str,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::net::UnixListener;

    let path: &str = socket_path.strip_prefix("unix://").unwrap_or(socket_path);
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    tracing::info!(transport = "redwire-unix", bind = %path, "listener online");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let rt = runtime.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_standalone_unix(stream, rt).await {
                tracing::warn!(transport = "redwire-unix", err = %err, "connection failed");
            }
        });
    }
}

/// Start a RedWire listener wrapped in TLS.
pub async fn start_redwire_tls_listener(
    bind_addr: &str,
    runtime: Arc<RedDBRuntime>,
    tls_config: &crate::wire::tls::WireTlsConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let acceptor = crate::wire::tls::build_tls_acceptor(tls_config)?;
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(transport = "redwire+tls", bind = %bind_addr, "listener online");
    serve_redwire_tls(listener, acceptor, runtime).await
}

/// Start a TLS RedWire listener on an already-bound TCP listener. Mirror
/// of [`start_redwire_listener_on`] for the TLS edge — lets a caller pick
/// the bound address (e.g. an ephemeral `127.0.0.1:0` port) before serving.
pub async fn start_redwire_tls_listener_on(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
    tls_config: &crate::wire::tls::WireTlsConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let acceptor = crate::wire::tls::build_tls_acceptor(tls_config)?;
    serve_redwire_tls(listener, acceptor, runtime).await
}

async fn serve_redwire_tls(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (tcp_stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let rt = runtime.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            match acceptor.accept(tcp_stream).await {
                Ok(tls_stream) => {
                    if let Err(err) = handle_standalone_tls(tls_stream, rt).await {
                        tracing::warn!(
                            transport = "redwire+tls",
                            peer = %peer_str,
                            err = %err,
                            "session ended with error"
                        );
                    }
                }
                Err(err) => tracing::warn!(
                    transport = "redwire+tls",
                    peer = %peer_str,
                    err = %err,
                    "TLS handshake failed"
                ),
            }
        });
    }
}

/// In-process demux entry (issue #933): serve one RedWire connection the
/// protocol router classified on the shared port. The router peeks — it
/// does not consume — so the RedWire startup magic is still on the wire and is
/// read here exactly as in the standalone path. The auth store is pulled
/// off the runtime so bearer tokens minted over HTTP are honoured here too.
pub async fn handle_router_connection(
    stream: TcpStream,
    runtime: Arc<RedDBRuntime>,
) -> io::Result<()> {
    let auth_store = runtime.auth_store();
    handle_standalone(stream, runtime, auth_store, None).await
}

/// Consume the RedWire magic byte off an arbitrary async byte stream,
/// then run the session. This is the seam (issue #932, ADR 0036) that
/// lets a non-socket transport — the WebSocket data channel (#935) —
/// reuse the exact standalone preamble: the browser sends the same
/// RedWire startup magic as native drivers, and the WS edge peels it here before
/// handing the stream to the transport-agnostic session.
pub(crate) async fn handle_session_consume_magic<S>(
    mut stream: S,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
    oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    validate_startup_magic(magic[0]).map_err(io::Error::other)?;
    handle_session(stream, runtime, auth_store, oauth).await
}

/// Standalone entry: consume the magic byte ourselves before the
/// session loop. The router-multiplexed entry skips this — the
/// detector already consumed the magic.
async fn handle_standalone(
    stream: TcpStream,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
    oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
) -> io::Result<()> {
    handle_session_consume_magic(stream, runtime, auth_store, oauth).await
}

#[cfg(unix)]
async fn handle_standalone_unix(
    mut stream: tokio::net::UnixStream,
    runtime: Arc<RedDBRuntime>,
) -> io::Result<()> {
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    validate_startup_magic(magic[0]).map_err(io::Error::other)?;
    handle_session(stream, runtime, None, None).await
}

async fn handle_standalone_tls<S>(
    mut stream: tokio_rustls::server::TlsStream<S>,
    runtime: Arc<RedDBRuntime>,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    validate_startup_magic(magic[0]).map_err(io::Error::other)?;
    handle_session(stream, runtime, None, None).await
}
