//! TCP / TLS / Unix listeners for the RedWire protocol.
//!
//! Each accepted connection spawns a `handle_session` task. The
//! first byte off the wire (the RedWire magic, `0xFE`) is consumed
//! by the service-router detector before reaching this listener;
//! when the listener runs standalone it reads the magic itself.

use std::io;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;

use super::session::handle_session;
use super::REDWIRE_MAGIC;

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

/// Start a RedWire listener on an already-bound TCP listener (used
/// by the service router which owns the public socket and proxies
/// to a loopback redwire backend).
pub async fn start_redwire_listener_on(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    serve_redwire_tcp(listener, runtime, None, None).await
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

/// Standalone entry: consume the magic byte ourselves before the
/// session loop. The router-multiplexed entry skips this — the
/// detector already consumed the magic.
async fn handle_standalone(
    mut stream: TcpStream,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
    oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
) -> io::Result<()> {
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    if magic[0] != REDWIRE_MAGIC {
        return Err(io::Error::other(format!(
            "redwire: client did not present magic byte (got 0x{:02x})",
            magic[0]
        )));
    }
    handle_session(stream, runtime, auth_store, oauth).await
}

#[cfg(unix)]
async fn handle_standalone_unix(
    mut stream: tokio::net::UnixStream,
    runtime: Arc<RedDBRuntime>,
) -> io::Result<()> {
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    if magic[0] != REDWIRE_MAGIC {
        return Err(io::Error::other(format!(
            "redwire: client did not present magic byte (got 0x{:02x})",
            magic[0]
        )));
    }
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
    if magic[0] != REDWIRE_MAGIC {
        return Err(io::Error::other(format!(
            "redwire: client did not present magic byte (got 0x{:02x})",
            magic[0]
        )));
    }
    handle_session(stream, runtime, None, None).await
}
