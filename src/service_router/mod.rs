//! TCP protocol router.
//!
//! Accepts an inbound TCP connection, peeks the first bytes to decide whether
//! it's gRPC (HTTP/2), HTTP/1.x, or the proprietary Wire protocol, and proxies
//! to the matching backend. Detection is pluggable: each protocol is a
//! `ProtocolDetector` impl in the `detector` submodule, and the `Router`
//! composes them in order.

pub(crate) mod detector;
pub(crate) mod router;

use std::net::SocketAddr;

use tokio::io;
use tokio::net::{TcpListener, TcpStream};

pub(crate) use detector::Protocol;
use router::Router;

#[derive(Debug, Clone)]
pub(crate) struct TcpProtocolRouterConfig {
    pub bind_addr: String,
    pub grpc_backend: SocketAddr,
    pub http_backend: SocketAddr,
    pub wire_backend: SocketAddr,
}

pub(crate) async fn serve_tcp_router(
    config: TcpProtocolRouterConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(
        transport = "router",
        bind = %config.bind_addr,
        protocols = "grpc/http/wire",
        "listener online"
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        let router = config.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(err) = proxy_routed_connection(stream, router).await {
                tracing::warn!(
                    transport = "router",
                    peer = %peer_str,
                    err = %err,
                    "connection failed"
                );
            }
        });
    }
}

async fn proxy_routed_connection(
    mut inbound: TcpStream,
    config: TcpProtocolRouterConfig,
) -> io::Result<()> {
    let router = Router::default_tcp();
    let backend = match router.detect(&inbound).await? {
        Protocol::Grpc => config.grpc_backend,
        Protocol::Http => config.http_backend,
        Protocol::Wire => config.wire_backend,
    };

    let mut outbound = TcpStream::connect(backend).await?;
    tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await?;
    Ok(())
}
