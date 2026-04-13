use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

const HTTP_2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const PROTOCOL_PROBE_RETRY: Duration = Duration::from_millis(10);
const HTTP_METHOD_PREFIXES: [&[u8]; 9] = [
    b"GET ",
    b"POST ",
    b"PUT ",
    b"PATCH ",
    b"DELETE ",
    b"HEAD ",
    b"OPTIONS ",
    b"TRACE ",
    b"CONNECT ",
];

#[derive(Debug, Clone)]
pub(crate) struct TcpProtocolRouterConfig {
    pub bind_addr: String,
    pub grpc_backend: SocketAddr,
    pub http_backend: SocketAddr,
    pub wire_backend: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutedTcpProtocol {
    Grpc,
    Http,
    Wire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutedTcpProtocolProbe {
    Ready(RoutedTcpProtocol),
    Pending,
}

pub(crate) async fn serve_tcp_router(
    config: TcpProtocolRouterConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    eprintln!(
        "red server (router) listening on {} [grpc/http/wire]",
        config.bind_addr
    );

    loop {
        let (stream, _) = listener.accept().await?;
        let router = config.clone();
        tokio::spawn(async move {
            if let Err(err) = proxy_routed_connection(stream, router).await {
                eprintln!("router connection error: {err}");
            }
        });
    }
}

async fn proxy_routed_connection(
    mut inbound: TcpStream,
    config: TcpProtocolRouterConfig,
) -> io::Result<()> {
    let backend = match detect_tcp_protocol(&inbound).await? {
        RoutedTcpProtocol::Grpc => config.grpc_backend,
        RoutedTcpProtocol::Http => config.http_backend,
        RoutedTcpProtocol::Wire => config.wire_backend,
    };

    let mut outbound = TcpStream::connect(backend).await?;
    tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await?;
    Ok(())
}

async fn detect_tcp_protocol(stream: &TcpStream) -> io::Result<RoutedTcpProtocol> {
    let started_at = Instant::now();
    let mut peek_buf = [0_u8; HTTP_2_PREFACE.len()];

    loop {
        let read = stream.peek(&mut peek_buf).await?;
        if read == 0 {
            return Ok(RoutedTcpProtocol::Wire);
        }

        let bytes = &peek_buf[..read];
        match probe_tcp_protocol(bytes) {
            RoutedTcpProtocolProbe::Ready(protocol) => return Ok(protocol),
            RoutedTcpProtocolProbe::Pending => {
                if read == peek_buf.len() || started_at.elapsed() >= PROTOCOL_PROBE_TIMEOUT {
                    return Ok(resolve_pending_protocol(bytes));
                }
                sleep(PROTOCOL_PROBE_RETRY).await;
            }
        }
    }
}

fn classify_tcp_protocol(bytes: &[u8]) -> RoutedTcpProtocol {
    match probe_tcp_protocol(bytes) {
        RoutedTcpProtocolProbe::Ready(protocol) => protocol,
        RoutedTcpProtocolProbe::Pending => resolve_pending_protocol(bytes),
    }
}

fn probe_tcp_protocol(bytes: &[u8]) -> RoutedTcpProtocolProbe {
    if bytes.starts_with(HTTP_2_PREFACE) {
        return RoutedTcpProtocolProbe::Ready(RoutedTcpProtocol::Grpc);
    }

    if HTTP_METHOD_PREFIXES
        .iter()
        .any(|prefix| bytes.starts_with(prefix))
    {
        return RoutedTcpProtocolProbe::Ready(RoutedTcpProtocol::Http);
    }

    if grpc_candidate(bytes) || http_candidate(bytes) {
        return RoutedTcpProtocolProbe::Pending;
    }

    RoutedTcpProtocolProbe::Ready(RoutedTcpProtocol::Wire)
}

fn resolve_pending_protocol(bytes: &[u8]) -> RoutedTcpProtocol {
    match (grpc_candidate(bytes), http_candidate(bytes)) {
        (true, false) => RoutedTcpProtocol::Grpc,
        (false, true) => RoutedTcpProtocol::Http,
        _ => RoutedTcpProtocol::Wire,
    }
}

fn grpc_candidate(bytes: &[u8]) -> bool {
    HTTP_2_PREFACE.starts_with(bytes)
}

fn http_candidate(bytes: &[u8]) -> bool {
    HTTP_METHOD_PREFIXES
        .iter()
        .any(|prefix| prefix.starts_with(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_detects_http_2_preface_as_grpc() {
        assert_eq!(
            classify_tcp_protocol(HTTP_2_PREFACE),
            RoutedTcpProtocol::Grpc
        );
    }

    #[test]
    fn classify_detects_http_1_methods() {
        assert_eq!(
            classify_tcp_protocol(b"POST /query HTTP/1.1\r\nhost: localhost\r\n\r\n"),
            RoutedTcpProtocol::Http
        );
        assert_eq!(
            classify_tcp_protocol(b"GET /health HTTP/1.1\r\nhost: localhost\r\n\r\n"),
            RoutedTcpProtocol::Http
        );
    }

    #[test]
    fn classify_falls_back_to_wire_for_binary_frames() {
        assert_eq!(
            classify_tcp_protocol(&[0x10, 0x00, 0x00, 0x00, 0x01, b'S', b'E', b'L']),
            RoutedTcpProtocol::Wire
        );
    }

    #[test]
    fn probe_keeps_partial_http_2_preface_pending() {
        assert_eq!(
            probe_tcp_protocol(b"PRI * HTTP/2.0\r\n"),
            RoutedTcpProtocolProbe::Pending
        );
    }

    #[test]
    fn probe_keeps_partial_http_method_pending() {
        assert_eq!(probe_tcp_protocol(b"PO"), RoutedTcpProtocolProbe::Pending);
    }
}
