//! In-process protocol demux (issue #933, PRD #930, ADR 0035).
//!
//! A single async acceptor owns the shared public port. For each inbound
//! connection it peeks the first bytes, classifies the protocol (gRPC
//! HTTP/2 preface / HTTP/1.x / RedWire `0xFE` magic), and dispatches to the
//! matching handler **in-process** — there is no loopback backend socket
//! and no `copy_bidirectional` proxy hop. All transports share the one
//! tokio runtime the acceptor runs on.
//!
//! Peeking (not reading) is the load-bearing trick: the classified bytes
//! stay in the socket buffer, so the chosen handler re-reads the
//! connection from byte zero exactly as if it had accepted it directly —
//! the HTTP edge reads its request line, hyper reads the HTTP/2 preface,
//! and the RedWire session reads its magic byte.
//!
//! Detection is pluggable: each protocol is a `ProtocolDetector` impl in
//! the `detector` submodule, and the `Router` composes them in order.

pub(crate) mod detector;
pub(crate) mod router;

use std::sync::Arc;

use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub(crate) use detector::Protocol;
use router::Router;

use crate::grpc::RedDBGrpcServer;
use crate::runtime::RedDBRuntime;
use crate::server::RedDBServer;

/// Depth of the bounded channel feeding classified gRPC connections into
/// the in-process tonic server. A connection sits here only for the brief
/// window between classification and tonic accepting it; the bound keeps a
/// burst of gRPC dials from growing the queue without limit.
const GRPC_DEMUX_CHANNEL_DEPTH: usize = 128;

/// Handler dependencies the demux dispatches into. Each transport is the
/// same server object the standalone single-transport runners use, so the
/// served surface on the shared port matches the dedicated ports exactly.
pub(crate) struct InProcessRouterConfig {
    pub bind_addr: String,
    pub http_server: RedDBServer,
    pub grpc_server: RedDBGrpcServer,
    pub wire_runtime: Arc<RedDBRuntime>,
}

pub(crate) async fn serve_tcp_router(
    config: InProcessRouterConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let InProcessRouterConfig {
        bind_addr,
        http_server,
        grpc_server,
        wire_runtime,
    } = config;

    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!(
        transport = "router",
        bind = %bind_addr,
        protocols = "grpc/http/wire",
        "in-process demux online"
    );

    // One long-lived tonic server, fed classified connections over a
    // channel instead of its own listener. Dropping every `grpc_tx` (only
    // on acceptor exit) closes the stream and drains the server.
    let (grpc_tx, grpc_rx) = mpsc::channel::<TcpStream>(GRPC_DEMUX_CHANNEL_DEPTH);
    tokio::spawn(async move {
        if let Err(err) = grpc_server.serve_router_demux(grpc_rx).await {
            tracing::error!(transport = "router", err = %err, "in-process gRPC server exited");
        }
    });

    let router = Arc::new(Router::default_tcp());
    loop {
        let (stream, peer) = listener.accept().await?;
        let router = router.clone();
        let http_server = http_server.clone();
        let grpc_tx = grpc_tx.clone();
        let wire_runtime = wire_runtime.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(err) =
                dispatch_connection(stream, &router, http_server, grpc_tx, wire_runtime).await
            {
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

/// Classify one accepted connection and hand it to the matching handler
/// in-process. The peeked bytes remain buffered on the socket, so each
/// handler reads the connection from the start.
async fn dispatch_connection(
    stream: TcpStream,
    router: &Router,
    http_server: RedDBServer,
    grpc_tx: mpsc::Sender<TcpStream>,
    wire_runtime: Arc<RedDBRuntime>,
) -> io::Result<()> {
    match router.detect(&stream).await? {
        Protocol::Http => http_server.serve_edge_one(stream).await,
        Protocol::Grpc => {
            // The only failure here is the tonic server task having exited;
            // surface it rather than dropping the connection silently.
            if grpc_tx.send(stream).await.is_err() {
                return Err(io::Error::other("in-process gRPC server unavailable"));
            }
        }
        Protocol::Wire => {
            crate::wire::redwire::listener::handle_router_connection(stream, wire_runtime).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::detector::HTTP_2_PREFACE;
    use super::router::Router;
    use super::Protocol;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};

    /// Drive the real peek path the demux uses: a client writes a
    /// protocol's opening bytes, the server accepts and classifies. This
    /// proves classification works end-to-end over a socket (peek, not
    /// read) for each protocol the shared port serves.
    async fn classify_opening_bytes(opening: &[u8]) -> Protocol {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let opening = opening.to_vec();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.expect("connect");
            stream.write_all(&opening).await.expect("write");
            stream.flush().await.expect("flush");
            // Hold the connection open so the peeked bytes are not lost to
            // a half-close racing the server's classification.
            stream
        });

        let (server_stream, _peer) = listener.accept().await.expect("accept");
        let protocol = Router::default_tcp()
            .detect(&server_stream)
            .await
            .expect("detect");
        let _client = client.await.expect("client task");
        protocol
    }

    #[tokio::test]
    async fn demux_classifies_h2_preface_as_grpc() {
        assert_eq!(classify_opening_bytes(HTTP_2_PREFACE).await, Protocol::Grpc);
    }

    #[tokio::test]
    async fn demux_classifies_http_request_line_as_http() {
        assert_eq!(
            classify_opening_bytes(b"POST /query HTTP/1.1\r\n").await,
            Protocol::Http
        );
        assert_eq!(
            classify_opening_bytes(b"GET /health HTTP/1.1\r\n").await,
            Protocol::Http
        );
    }

    #[tokio::test]
    async fn demux_classifies_redwire_magic_as_wire() {
        // 0xFE magic followed by a handshake frame prefix.
        assert_eq!(
            classify_opening_bytes(&[0xFE, 0x00, 0x01, 0x02]).await,
            Protocol::Wire
        );
    }

    #[tokio::test]
    async fn demux_falls_back_to_wire_for_unknown_binary() {
        // No detector matches a bare binary frame → Wire fallback, matching
        // the previous router's reachability for native RedWire clients.
        assert_eq!(
            classify_opening_bytes(&[0x10, 0x00, 0x00, 0x00, 0x01, b'S', b'E', b'L']).await,
            Protocol::Wire
        );
    }
}
