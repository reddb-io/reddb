#![cfg(all(feature = "http", feature = "redwire-websocket"))]

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use reddb_client::{QueryOptions, Reddb};
use reddb_wire::redwire::{self, Frame, MessageKind};
use reddb_wire::topology::{Endpoint, ReplicaInfo, Topology};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

type Membership = Arc<RwLock<Topology>>;

fn reply(frame: Frame, node: &str, topology: &Topology) -> Frame {
    let features =
        reddb_wire::query_with_params::FEATURE_PARAMS | reddb_wire::routing::FEATURE_TOPOLOGY;
    let (kind, payload) = match frame.kind {
        MessageKind::Hello => (MessageKind::HelloAck, redwire::build_hello_ack(1, "bearer", features, None)),
        MessageKind::AuthResponse => {
            let auth: serde_json::Value = serde_json::from_slice(&frame.payload).expect("auth JSON");
            assert_eq!(auth["token"], "test-token");
            (MessageKind::AuthOk, redwire::build_auth_ok_payload("test-session", "reader", "read", features))
        }
        MessageKind::Topology => (MessageKind::Result, reddb_wire::encode_topology(topology)),
        MessageKind::Query | MessageKind::QueryWithParams => (MessageKind::Result, serde_json::to_vec(&serde_json::json!({
            "statement": "select", "columns": ["node"], "records": [{ "node": node }], "affected": 0,
        })).expect("query result JSON")),
        MessageKind::Bye => (MessageKind::Bye, Vec::new()),
        _ => panic!("unexpected mock request {:?}", frame.kind),
    };
    redwire::build_reply_frame(frame.correlation_id, kind, payload).expect("mock frame")
}

async fn serve_wire<S>(mut stream: S, node: &str, topology: Membership)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut preface = [0; 2];
    if stream.read_exact(&mut preface).await.is_err() {
        return;
    }
    while let Ok(frame) = redwire::read_frame_async(&mut stream).await {
        let bye = frame.kind == MessageKind::Bye;
        let response = reply(frame, node, &*topology.read().await);
        if redwire::write_frame_async(&mut stream, &response)
            .await
            .is_err()
            || bye
        {
            break;
        }
    }
}

async fn serve_websocket<S>(stream: S, node: &str, topology: Membership)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(
        stream,
        |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
         mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            response.headers_mut().insert(
                "sec-websocket-protocol",
                redwire::REDWIRE_WS_SUBPROTOCOL
                    .parse()
                    .expect("test subprotocol"),
            );
            Ok(response)
        },
    )
    .await
    else {
        return;
    };
    let mut bytes = Vec::new();
    let mut preface = false;
    while let Some(Ok(Message::Binary(chunk))) = socket.next().await {
        bytes.extend_from_slice(&chunk);
        if !preface {
            if bytes.len() < 2 {
                continue;
            }
            bytes.drain(..2);
            preface = true;
        }
        while bytes.len() >= 16 {
            let length = u32::from_le_bytes(bytes[..4].try_into().expect("frame length")) as usize;
            if bytes.len() < length {
                break;
            }
            let (frame, _) = redwire::decode_frame(&bytes[..length]).expect("mock request frame");
            let bye = frame.kind == MessageKind::Bye;
            let response = redwire::encode_frame(&reply(frame, node, &*topology.read().await));
            bytes.drain(..length);
            if socket.send(Message::Binary(response.into())).await.is_err() || bye {
                return;
            }
        }
    }
}

async fn serve_http<S>(mut stream: S, node: &str, topology: Membership)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).await.is_err() {
            return;
        }
        request.push(byte[0]);
    }
    let headers = String::from_utf8(request).expect("HTTP headers");
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer test-token")
            || headers.starts_with("GET /health")
    );
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    if stream.read_exact(&mut body).await.is_err() {
        return;
    }
    let value = if headers.starts_with("GET /v1/topology") {
        reddb_wire::routing::topology_json(&*topology.read().await)
    } else if headers.starts_with("GET /health") {
        serde_json::json!({"ok": true})
    } else {
        serde_json::json!({"statement": "select", "columns": ["node"], "records": [{"node": node}]})
    };
    let body = value.to_string();
    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn start_node(
    protocol: &str,
    node: &'static str,
    topology: Membership,
    tls: Arc<rustls::ServerConfig>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = format!(
        "{protocol}://{}",
        listener.local_addr().expect("listener address")
    );
    let protocol = protocol.to_string();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let protocol = protocol.clone();
            let topology = Arc::clone(&topology);
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&tls));
            tokio::spawn(async move {
                let stream: std::pin::Pin<Box<dyn TestStream + Send>> =
                    if matches!(protocol.as_str(), "reds" | "https" | "wss") {
                        let Ok(stream) = acceptor.accept(stream).await else {
                            return;
                        };
                        Box::pin(stream)
                    } else {
                        Box::pin(stream)
                    };
                match protocol.as_str() {
                    "http" | "https" => serve_http(stream, node, topology).await,
                    "ws" | "wss" => serve_websocket(stream, node, topology).await,
                    _ => serve_wire(stream, node, topology).await,
                }
            });
        }
    });
    (address, task)
}

trait TestStream: AsyncRead + AsyncWrite + Unpin {}
impl<T: AsyncRead + AsyncWrite + Unpin> TestStream for T {}

#[tokio::test]
async fn high_level_client_routes_over_tcp_http_websocket_and_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).expect("test certificate");
    let tls = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
            )
            .expect("TLS server"),
    );
    let directory = tempfile::tempdir().expect("certificate directory");
    let certificate = directory.path().join("ca.pem");
    std::fs::write(&certificate, cert.pem()).expect("write test CA");
    for protocol in ["red", "reds", "http", "https", "ws", "wss"] {
        let topology = Arc::new(RwLock::new(Topology {
            epoch: 1,
            primary: Endpoint {
                addr: String::new(),
                region: String::new(),
            },
            replicas: Vec::new(),
        }));
        let (primary, primary_task) =
            start_node(protocol, "primary", Arc::clone(&topology), Arc::clone(&tls)).await;
        let (replica, replica_task) =
            start_node(protocol, "replica", Arc::clone(&topology), Arc::clone(&tls)).await;
        {
            let mut membership = topology.write().await;
            membership.primary.addr = primary.clone();
            membership.replicas.push(ReplicaInfo {
                addr: replica,
                region: String::new(),
                healthy: true,
                lag_ms: 0,
                last_applied_lsn: 100,
                rebootstrapping: false,
            });
        }
        let mut uri = url::Url::parse(&primary).expect("test URI");
        uri.query_pairs_mut()
            .append_pair("ca", &certificate.to_string_lossy());
        uri.query_pairs_mut().append_pair("token", "test-token");
        let db = Reddb::connect(uri.as_str()).await.expect(protocol);
        let read = db
            .query("SELECT node FROM users")
            .await
            .expect("replica read");
        assert_eq!(
            read.rows[0][0].1,
            reddb_client::ValueOut::String("replica".into()),
            "{protocol}"
        );
        let strong = db
            .query_with_options("SELECT node FROM users", QueryOptions::strong())
            .await
            .expect("strong read");
        assert_eq!(
            strong.rows[0][0].1,
            reddb_client::ValueOut::String("primary".into()),
            "{protocol}"
        );
        db.query("/* transaction */ BEGIN").await.expect("begin");
        let transaction = db
            .query("SELECT node FROM users")
            .await
            .expect("transaction read");
        assert_eq!(
            transaction.rows[0][0].1,
            reddb_client::ValueOut::String("primary".into()),
            "{protocol}"
        );
        db.query("ROLLBACK").await.expect("rollback");
        db.close().await.expect("close");
        let closed = db.query_with_options(
            "SELECT node FROM users",
            QueryOptions::causal(
                reddb_wire::replication::CausalBookmark::new(1, 101),
                reddb_client::CausalReadOptions::with_deadline(std::time::Duration::from_secs(1)),
            ),
        );
        let error = tokio::time::timeout(std::time::Duration::from_millis(100), closed)
            .await
            .expect("closed client must not wait for replication")
            .expect_err("closed client");
        assert_eq!(error.code, reddb_client::ErrorCode::ClientClosed);
        primary_task.abort();
        replica_task.abort();
    }
}
