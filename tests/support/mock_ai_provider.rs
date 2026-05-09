use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockAiErrorKind {
    RateLimit,
    Server,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct MockAiProviderConfig {
    pub latency_ms: u64,
    pub error_rate: f64,
    pub error_kind: MockAiErrorKind,
    pub dup_text_count: usize,
}

impl Default for MockAiProviderConfig {
    fn default() -> Self {
        Self {
            latency_ms: 0,
            error_rate: 0.0,
            error_kind: MockAiErrorKind::Server,
            dup_text_count: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockAiProviderCounters {
    pub total_requests: u64,
    pub total_inputs: u64,
    pub unique_inputs: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MockAiProviderLatency {
    pub p50_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Default)]
struct MockAiProviderState {
    total_requests: AtomicU64,
    total_inputs: AtomicU64,
    unique_inputs: Mutex<HashSet<String>>,
    request_latencies_us: Mutex<Vec<u64>>,
}

pub struct MockAiProvider {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    state: Arc<MockAiProviderState>,
    handle: Option<JoinHandle<()>>,
}

impl MockAiProvider {
    pub fn start(config: MockAiProviderConfig) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(MockAiProviderState::default());

        let server_shutdown = Arc::clone(&shutdown);
        let server_state = Arc::clone(&state);
        let handle = thread::spawn(move || {
            while !server_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let start = Instant::now();
                        handle_stream(stream, &config, &server_state);
                        server_state
                            .request_latencies_us
                            .lock()
                            .expect("latency lock")
                            .push(start.elapsed().as_micros() as u64);
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            addr,
            shutdown,
            state,
            handle: Some(handle),
        })
    }

    pub fn api_base(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn counters(&self) -> MockAiProviderCounters {
        MockAiProviderCounters {
            total_requests: self.state.total_requests.load(Ordering::Relaxed),
            total_inputs: self.state.total_inputs.load(Ordering::Relaxed),
            unique_inputs: self
                .state
                .unique_inputs
                .lock()
                .expect("unique input lock")
                .len() as u64,
        }
    }

    pub fn latency(&self) -> MockAiProviderLatency {
        let mut samples = self
            .state
            .request_latencies_us
            .lock()
            .expect("latency lock")
            .clone();
        samples.sort_unstable();
        MockAiProviderLatency {
            p50_ms: percentile_ms(&samples, 0.50),
            p99_ms: percentile_ms(&samples, 0.99),
        }
    }

    pub fn duplicate_bench_inputs(total: usize, dup_text_count: usize) -> Vec<String> {
        let duplicate_prefix = dup_text_count.min(total);
        (0..total)
            .map(|index| {
                if index < duplicate_prefix {
                    "duplicate benchmark text".to_string()
                } else {
                    format!("benchmark text row {index:04}")
                }
            })
            .collect()
    }
}

impl Drop for MockAiProvider {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn percentile_ms(samples: &[u64], quantile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = ((samples.len() - 1) as f64 * quantile).ceil() as usize;
    samples[index] as f64 / 1000.0
}

fn handle_stream(
    mut stream: TcpStream,
    config: &MockAiProviderConfig,
    state: &MockAiProviderState,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let request_no = state.total_requests.fetch_add(1, Ordering::Relaxed) + 1;

    if config.latency_ms > 0 {
        thread::sleep(Duration::from_millis(config.latency_ms));
    }

    if should_error(config.error_rate, request_no) {
        match config.error_kind {
            MockAiErrorKind::RateLimit => {
                write_json(
                    &mut stream,
                    429,
                    json!({"error":{"message":"mock rate limit"}}),
                );
            }
            MockAiErrorKind::Server => {
                write_json(
                    &mut stream,
                    500,
                    json!({"error":{"message":"mock server error"}}),
                );
            }
            MockAiErrorKind::Timeout => {
                thread::sleep(Duration::from_secs(2));
            }
        }
        return;
    }

    if request.path.ends_with("/embeddings") {
        let body = embedding_response(&request.body, config.dup_text_count, state);
        write_json(&mut stream, 200, body);
    } else if request.path.ends_with("/chat/completions") {
        write_json(
            &mut stream,
            200,
            json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "mock-chat",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "mock response"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }),
        );
    } else {
        write_json(
            &mut stream,
            404,
            json!({"error":{"message":"mock endpoint not found"}}),
        );
    }
}

#[derive(Debug)]
struct HttpRequest {
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(pos) = find_header_end(&buffer) {
            break pos;
        }
    };

    let header = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header.lines();
    let request_line = lines.next()?;
    let path = request_line.split_whitespace().nth(1)?.to_string();
    let content_len = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let body_start = header_end + 4;
    while buffer.len().saturating_sub(body_start) < content_len {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let available = buffer.len().saturating_sub(body_start).min(content_len);
    Some(HttpRequest {
        path,
        body: buffer[body_start..body_start + available].to_vec(),
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn should_error(error_rate: f64, request_no: u64) -> bool {
    if error_rate <= 0.0 {
        return false;
    }
    if error_rate >= 1.0 {
        return true;
    }
    let threshold = (error_rate.clamp(0.0, 1.0) * 1000.0).round() as u64;
    ((request_no - 1) % 1000) < threshold
}

fn embedding_response(body: &[u8], dup_text_count: usize, state: &MockAiProviderState) -> Value {
    let parsed: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("mock-embedding")
        .to_string();
    let inputs = collect_inputs(parsed.get("input"));

    state
        .total_inputs
        .fetch_add(inputs.len() as u64, Ordering::Relaxed);
    {
        let mut unique = state.unique_inputs.lock().expect("unique input lock");
        for input in &inputs {
            unique.insert(input.clone());
        }
    }

    let data = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let source = if index < dup_text_count {
                "duplicate mock text"
            } else {
                input
            };
            json!({
                "object": "embedding",
                "index": index,
                "embedding": deterministic_embedding(source),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "object": "list",
        "data": data,
        "model": model,
        "usage": {
            "prompt_tokens": inputs.len(),
            "total_tokens": inputs.len()
        }
    })
}

fn collect_inputs(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(input)) => vec![input.clone()],
        Some(Value::Array(inputs)) => inputs
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn deterministic_embedding(input: &str) -> Vec<f32> {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (0..8)
        .map(|slot| {
            let shifted = hash.rotate_left((slot * 8) as u32);
            ((shifted & 0xffff) as f32) / 65535.0
        })
        .collect()
}

fn write_json(stream: &mut TcpStream, status: u16, body: Value) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
