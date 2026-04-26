//! TCP listener for RedWire v2. Mirrors the v1 listener pattern
//! at `src/wire/listener.rs` so operators see consistent shape.
//!
//! Each accepted connection spawns a `handle_session` task. The
//! first byte off the wire (the v2 magic, `0xFE`) is consumed by
//! the service-router detector before reaching this listener;
//! when the listener runs standalone it reads the magic itself.

use std::io;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;

use super::session::handle_session;
use super::REDWIRE_V2_MAGIC;

#[derive(Clone)]
pub struct RedWireConfig {
    pub bind_addr: String,
    pub auth_store: Option<Arc<AuthStore>>,
}

impl std::fmt::Debug for RedWireConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedWireConfig")
            .field("bind_addr", &self.bind_addr)
            .field("auth_store_present", &self.auth_store.is_some())
            .finish()
    }
}

pub async fn start_redwire_listener(
    config: RedWireConfig,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(
        transport = "redwire-v2",
        bind = %config.bind_addr,
        "listener online"
    );
    loop {
        let (stream, peer) = listener.accept().await?;
        let rt = runtime.clone();
        let auth = config.auth_store.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(err) = handle_standalone(stream, rt, auth).await {
                tracing::warn!(
                    transport = "redwire-v2",
                    peer = %peer_str,
                    err = %err,
                    "session ended with error"
                );
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
) -> io::Result<()> {
    let mut magic = [0u8; 1];
    stream.read_exact(&mut magic).await?;
    if magic[0] != REDWIRE_V2_MAGIC {
        return Err(io::Error::other(format!(
            "redwire: client did not present v2 magic (got 0x{:02x})",
            magic[0]
        )));
    }
    handle_session(stream, runtime, auth_store).await
}
