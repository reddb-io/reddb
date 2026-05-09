use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reddb_server::runtime::ai::transport::{
    AiHttpRequest, AiRetryConfig, AiTransport, AiTransportConfig,
};

#[tokio::test]
async fn ai_transport_retries_429_and_5xx_then_returns_success() {
    let stub = SequencedStub::start(vec![500, 429, 200]);
    let transport = test_transport(3);

    let response = transport
        .request(
            AiHttpRequest::post_json(
                "mock",
                format!("http://{}/v1/embeddings", stub.addr()),
                r#"{"input":"hello"}"#.to_string(),
            )
            .model("mock-embed"),
        )
        .await
        .expect("request should succeed after retries");

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body, r#"{"ok":true}"#);
    assert_eq!(response.attempt_count, 3);
    assert_eq!(stub.request_count(), 3);

    let mut body = String::new();
    reddb_server::runtime::ai::metrics::render_ai_metrics(&mut body);
    assert!(
        body.contains("reddb_ai_provider_retries_total{provider=\"mock\",reason=\"http_5xx\"}")
            || body
                .contains("reddb_ai_provider_retries_total{provider=\"mock\",reason=\"http_429\"}"),
        "{body}"
    );
    assert!(
        body.contains(
            "reddb_ai_provider_requests_total{provider=\"mock\",model=\"mock-embed\",status=\"ok\"}"
        ),
        "{body}"
    );
}

#[tokio::test]
async fn ai_transport_error_includes_context_after_retry_exhaustion() {
    let stub = SequencedStub::start(vec![500, 500]);
    let transport = test_transport(2);

    let err = transport
        .request(
            AiHttpRequest::post_json(
                "openai",
                format!("http://{}/v1/embeddings", stub.addr()),
                "{}".to_string(),
            )
            .model("mock-error-model"),
        )
        .await
        .expect_err("request should fail after retry exhaustion");

    assert_eq!(err.provider, "openai");
    assert_eq!(err.status_code, Some(500));
    assert_eq!(err.attempt_count, 2);
    assert_eq!(err.total_wait_ms, 1);
    assert!(err.to_string().contains("provider=openai"));
    assert_eq!(stub.request_count(), 2);

    let mut body = String::new();
    reddb_server::runtime::ai::metrics::render_ai_metrics(&mut body);
    assert!(
        body.contains(
            "reddb_ai_provider_requests_total{provider=\"openai\",model=\"mock-error-model\",status=\"http_5xx\"}"
        ),
        "{body}"
    );
}

#[tokio::test]
async fn ai_transport_retries_connection_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused local port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    let transport = test_transport(2);

    let err = transport
        .request(AiHttpRequest::post_json(
            "anthropic",
            format!("http://{addr}/v1/messages"),
            "{}".to_string(),
        ))
        .await
        .expect_err("connection refused should fail after retries");

    assert_eq!(err.provider, "anthropic");
    assert_eq!(err.status_code, None);
    assert_eq!(err.attempt_count, 2);
    assert_eq!(err.total_wait_ms, 1);
}

fn test_transport(max_attempts: u32) -> AiTransport {
    AiTransport::new(AiTransportConfig {
        pool_size: 2,
        timeout: Duration::from_millis(250),
        retry: AiRetryConfig {
            max_attempts,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        },
    })
}

struct SequencedStub {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    request_count: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

impl SequencedStub {
    fn start(statuses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("local addr");
        let shutdown = Arc::new(AtomicBool::new(false));
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_shutdown = Arc::clone(&shutdown);
        let server_count = Arc::clone(&request_count);
        let handle = thread::spawn(move || {
            while !server_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let index = server_count.fetch_add(1, Ordering::Relaxed);
                        read_http_request(&mut stream);
                        let status = statuses
                            .get(index)
                            .copied()
                            .unwrap_or_else(|| *statuses.last().expect("statuses"));
                        write_http_response(&mut stream, status);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            shutdown,
            request_count,
            handle: Some(handle),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::Relaxed)
    }
}

impl Drop for SequencedStub {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_http_request(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut buffer = [0_u8; 1024];
    let _ = stream.read(&mut buffer);
}

fn write_http_response(stream: &mut TcpStream, status: u16) {
    let reason = match status {
        200 => "OK",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let body = if status == 200 {
        r#"{"ok":true}"#
    } else {
        r#"{"error":"retry"}"#
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write stub response");
}
