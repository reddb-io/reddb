//! Async HTTP edge on axum/hyper (issue #931, PRD #930, ADR 0035).
//!
//! Replaces the thread-per-connection HTTP backend with an async
//! axum/hyper service running on a tokio runtime. The execution engine
//! stays 100% synchronous and disk-backed; the transport edge bridges to
//! it via `spawn_blocking`, exactly as the RedWire session bridges
//! async-transport ↔ sync-engine.
//!
//! What is re-homed here, unchanged in behaviour:
//!   * Routing and handlers — reached through the existing
//!     [`RedDBServer::route`] (buffered) and
//!     [`RedDBServer::try_route_streaming`] (NDJSON/SSE) entry points.
//!   * The single CORS choke point — buffered responses get the
//!     [`super::transport::CORS_HEADER_PAIRS`] set applied here; streaming
//!     heads carry it inline (written by the sync emitters) and are
//!     forwarded verbatim.
//!   * The `HeaderEscapeGuard` — guard-validated `extra_headers` ride
//!     through onto the hyper response.
//!
//! What is retired: the `serve_on` accept loop that spawned one OS thread
//! per connection and the `(2*num_cpus).clamp(8,256)` thread cap. Idle
//! keep-alive connections are now cheap tokio tasks that hold neither a
//! thread nor an admission slot — the connection limiter is applied per
//! in-flight request, not per connection.

use std::io::{self, Write};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoConnBuilder;
use hyper_util::service::TowerToHyperService;

use super::http_connection_limiter::HttpConnectionPermit;
use super::http_handler_metrics::{HttpRejectReason, HttpTransport};
use super::per_principal_conns::PrincipalConnPermit;
use super::routing::{
    is_health_probe_request, principal_connection_refusal_response, principal_for,
};
use super::transport::{
    find_header_end, json_error, parse_query_string, HttpRequest, HttpResponse, CORS_HEADER_PAIRS,
};
use super::RedDBServer;

/// Both admission permits a request holds for its lifetime: the global
/// in-flight limiter slot (async backpressure — bounds total in-flight
/// work without a thread cap) and the per-principal in-flight slot
/// (fairness — one caller cannot monopolise the global budget). Bundled so
/// they move into the `spawn_blocking` closure together and release in
/// lock-step when the handler returns (issue #934).
struct EdgeAdmission {
    _global: HttpConnectionPermit,
    _principal: PrincipalConnPermit,
}

/// Bounded buffer between the sync streaming producer (running on the
/// `spawn_blocking` pool) and the async hyper body. Small enough to apply
/// backpressure to a slow producer, large enough to avoid ping-ponging
/// the blocking thread on every frame.
const STREAM_CHANNEL_DEPTH: usize = 16;

/// Build the per-connection hyper server with HTTP/1 half-close support.
///
/// Many RedDB HTTP clients (and the e2e tests) send a request, then
/// `shutdown(Write)` their socket while waiting for the response — the
/// classic `Connection: close` request/response pattern the retired
/// thread-per-connection loop served fine. hyper defaults to aborting a
/// connection with "closed before message completed" the moment it sees
/// EOF on the read half, so `half_close(true)` is required to let the
/// server finish writing the response after the client closes its write
/// side.
fn connection_builder() -> AutoConnBuilder<TokioExecutor> {
    let mut builder = AutoConnBuilder::new(TokioExecutor::new());
    builder.http1().half_close(true);
    builder
}

/// State threaded into the axum fallback handler. Carries a cheap clone of
/// the server plus which transport this listener represents (so reject /
/// duration metrics land in the right bucket).
#[derive(Clone)]
struct EdgeState {
    server: RedDBServer,
    transport: HttpTransport,
}

/// axum catch-all: every method + path funnels through the existing
/// router. We deliberately do not declare per-route handlers — the
/// canonical routing table lives in [`RedDBServer::route`] and must stay
/// the single source of truth.
async fn edge_fallback(State(state): State<EdgeState>, req: axum::extract::Request) -> Response {
    state.server.handle_edge_request(req, state.transport).await
}

impl RedDBServer {
    /// Build the axum router for one listener surface.
    fn build_edge_router(&self, transport: HttpTransport) -> axum::Router {
        axum::Router::new()
            .fallback(edge_fallback)
            .with_state(EdgeState {
                server: self.clone(),
                transport,
            })
    }

    /// Serve the async HTTP edge on an already-bound tokio listener until
    /// the listener errors fatally. One cheap tokio task per accepted
    /// connection; hyper owns keep-alive so an idle connection is a parked
    /// future, not a thread.
    pub(crate) async fn serve_edge(
        self,
        listener: tokio::net::TcpListener,
        transport: HttpTransport,
    ) -> io::Result<()> {
        let router = self.build_edge_router(transport);
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    // Transient per-accept errors (e.g. EMFILE) should not
                    // tear the whole listener down; log and keep serving.
                    tracing::warn!(target: "reddb::http", error = %err, "accept failed");
                    continue;
                }
            };
            let service = TowerToHyperService::new(router.clone());
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                if let Err(err) = connection_builder().serve_connection(io, service).await {
                    tracing::debug!(target: "reddb::http", error = %err, "connection closed with error");
                }
            });
        }
    }

    /// TLS variant of [`Self::serve_edge`]. Terminates rustls per
    /// connection on the tokio runtime, then serves the same router.
    pub(crate) async fn serve_edge_tls(
        self,
        listener: tokio::net::TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
        transport: HttpTransport,
    ) -> io::Result<()> {
        let router = self.build_edge_router(transport);
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!(target: "reddb::http_tls", error = %err, "accept failed");
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            let service = TowerToHyperService::new(router.clone());
            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        let io = TokioIo::new(tls_stream);
                        if let Err(err) = connection_builder().serve_connection(io, service).await {
                            tracing::debug!(target: "reddb::http_tls", error = %err, "connection closed with error");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(target: "reddb::http_tls", error = %err, "TLS handshake failed");
                    }
                }
            });
        }
    }

    /// Adapt a std listener (as the CLI and tests hand us) into a tokio
    /// listener and serve the clear-text edge.
    pub(crate) async fn serve_edge_on_std(
        self,
        listener: std::net::TcpListener,
        transport: HttpTransport,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        self.serve_edge(listener, transport).await
    }

    /// Adapt a std listener and serve the TLS edge.
    pub(crate) async fn serve_edge_tls_on_std(
        self,
        listener: std::net::TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
        transport: HttpTransport,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        self.serve_edge_tls(listener, acceptor, transport).await
    }

    /// Serve a single accepted connection to completion (used by
    /// `serve_one_on`). The connection may carry multiple keep-alive
    /// requests; it returns when the peer closes.
    pub(crate) async fn serve_edge_one(self, stream: tokio::net::TcpStream) {
        let service = TowerToHyperService::new(self.build_edge_router(HttpTransport::Http));
        let io = TokioIo::new(stream);
        if let Err(err) = connection_builder().serve_connection(io, service).await {
            tracing::debug!(target: "reddb::http", error = %err, "connection closed with error");
        }
    }

    /// Per-request entry: parse → admit → dispatch (buffered or
    /// streaming) → record duration. Mirrors the cross-cutting concerns
    /// the old `handle_connection` thread carried (limiter, handler
    /// deadline, metrics), minus the per-connection OS thread.
    async fn handle_edge_request(
        &self,
        req: axum::extract::Request,
        transport: HttpTransport,
    ) -> Response {
        let started = std::time::Instant::now();
        let request = match read_edge_request(req, self.options.max_body_bytes).await {
            Ok(request) => request,
            Err(response) => return response,
        };

        // Per-principal fairness cap (issue #934). Checked *before* the
        // global slot so an over-cap caller is shed without ever consuming
        // global admission — that is the point of the fairness bound.
        // Health probes are exempt: a refused `/health/*` reads as the
        // instance being down to a load balancer. A cap of `0` (the
        // default) disables enforcement and the acquire is infallible.
        let principal = principal_for(&request.headers);
        let principal_cap = if is_health_probe_request(&request.method, &request.path) {
            0
        } else {
            self.max_conns_per_principal
        };
        let principal_permit = match self.principal_conns.try_acquire(&principal, principal_cap) {
            Ok(permit) => permit,
            Err(err) => {
                self.http_metrics
                    .record_reject(transport, HttpRejectReason::PrincipalCapExhausted);
                return buffered_response_to_axum(principal_connection_refusal_response(
                    &err,
                    self.retry_after_secs,
                ));
            }
        };

        // Global admission is per in-flight request, not per connection:
        // an idle keep-alive connection holds no slot, so the cap no
        // longer bounds connection count (issue #931 / AC #4). This is the
        // async-backpressure bound that replaces the retired thread cap.
        let global_permit = match self.http_limiter.try_acquire() {
            Some(permit) => permit,
            None => {
                self.http_metrics
                    .record_reject(transport, HttpRejectReason::CapExhausted);
                return self.reject_capacity_response();
            }
        };

        let admission = EdgeAdmission {
            _global: global_permit,
            _principal: principal_permit,
        };

        let response = if self.is_streaming_request(&request) {
            self.serve_streaming_request(request, admission).await
        } else {
            self.serve_buffered_request(request, admission, transport)
                .await
        };
        self.http_metrics
            .record_duration(transport, started.elapsed().as_secs_f64());
        response
    }

    /// Buffered (non-streaming) path: run the synchronous router on the
    /// blocking pool, bounded by the per-handler deadline.
    async fn serve_buffered_request(
        &self,
        request: HttpRequest,
        admission: EdgeAdmission,
        transport: HttpTransport,
    ) -> Response {
        let server = self.clone();
        let join = tokio::task::spawn_blocking(move || {
            // Both admission permits are held for the duration of the
            // engine call and released together when this closure returns.
            let _admission = admission;
            let response = server.route(request);
            // Test-only injected slow downstream (doc-hidden hook). In
            // production this is 0, so it is a single relaxed atomic load
            // on the hot path. Sleeping here — while the admission permits
            // are still held — lets a test deterministically hold a
            // per-principal slot to exercise the cap (issue #934).
            let inject_ms = server.test_slow_inject_ms();
            if inject_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(inject_ms));
            }
            response
        });
        match tokio::time::timeout(self.handler_timeout, join).await {
            Ok(Ok(response)) => buffered_response_to_axum(response),
            Ok(Err(_join_err)) => internal_error_response(),
            Err(_elapsed) => {
                self.http_metrics
                    .record_reject(transport, HttpRejectReason::HandlerTimeout);
                handler_timeout_response()
            }
        }
    }

    /// Streaming path (NDJSON / SSE): the synchronous streaming handler
    /// writes a complete raw HTTP/1.1 response into a [`StreamSink`] on
    /// the blocking pool; the sink parses the head and de-frames the body
    /// into an async stream that hyper re-frames onto the wire. A buffered
    /// refusal (auth/quota/capacity/unsupported-statement) is detected by
    /// its `Content-Length` framing and served as a non-streamed response,
    /// preserving the existing wire contract.
    async fn serve_streaming_request(
        &self,
        request: HttpRequest,
        admission: EdgeAdmission,
    ) -> Response {
        let (head_tx, head_rx) = oneshot::channel::<EdgeStreamResponse>();
        let server = self.clone();
        tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let mut sink = StreamSink::new(head_tx);
            // Errors here are connection-level (client gone / broken
            // pipe); the sync handler already wrote what it could.
            let _ = server.try_route_streaming(&request, &mut sink);
            sink.finish();
        });
        match head_rx.await {
            Ok(response) => stream_response_to_axum(response),
            // The producer dropped the head sender without emitting a
            // complete response head — surface a 500 rather than hang.
            Err(_) => internal_error_response(),
        }
    }

    /// 503 emitted when the in-flight-request admission cap is exhausted.
    /// Carries `Retry-After` and the canonical CORS posture.
    fn reject_capacity_response(&self) -> Response {
        let body = format!(
            "{{\"error\":\"server at capacity\",\"retry_after_secs\":{}}}",
            self.retry_after_secs
        );
        let mut builder = Response::builder()
            .status(503)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::RETRY_AFTER, self.retry_after_secs.to_string());
        for (name, value) in CORS_HEADER_PAIRS {
            builder = builder.header(name, value);
        }
        builder
            .body(Body::from(body))
            .unwrap_or_else(|_| internal_error_response())
    }
}

/// Convert an incoming axum request into the internal [`HttpRequest`] the
/// router understands, enforcing the configured body cap (413 on
/// overflow). Header names are lower-cased and values decoded lossily to
/// match the legacy `HttpRequest::read_from` parser.
async fn read_edge_request(
    req: axum::extract::Request,
    max_body_bytes: usize,
) -> Result<HttpRequest, Response> {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let path = parts.uri.path().to_string();
    let query = parts
        .uri
        .query()
        .map(parse_query_string)
        .unwrap_or_default();

    let mut headers = std::collections::BTreeMap::new();
    for (name, value) in parts.headers.iter() {
        headers.insert(
            name.as_str().to_ascii_lowercase(),
            String::from_utf8_lossy(value.as_bytes()).trim().to_string(),
        );
    }

    let body = match axum::body::to_bytes(body, max_body_bytes).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => {
            return Err(buffered_response_to_axum(json_error(
                413,
                "request body exceeds configured limit",
            )))
        }
    };

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Build a hyper response from a buffered [`HttpResponse`], applying the
/// single CORS choke point plus any guard-validated `extra_headers`.
fn buffered_response_to_axum(response: HttpResponse) -> Response {
    let mut builder = Response::builder()
        .status(response.status)
        .header(http::header::CONTENT_TYPE, response.content_type);
    for (name, value) in CORS_HEADER_PAIRS {
        builder = builder.header(name, value);
    }
    for (name, value) in response.extra_headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(response.body))
        .unwrap_or_else(|_| internal_error_response())
}

/// Build a hyper response from a parsed streaming/buffered edge response.
fn stream_response_to_axum(response: EdgeStreamResponse) -> Response {
    match response {
        EdgeStreamResponse::Buffered {
            status,
            headers,
            body,
        } => build_response(status, headers, Body::from(body)),
        EdgeStreamResponse::Streaming {
            status,
            headers,
            body,
        } => build_response(
            status,
            headers,
            Body::from_stream(ReceiverStream::new(body)),
        ),
    }
}

/// Assemble a response from a status, a forwarded header set, and a body.
/// The header set already excludes hop-by-hop headers (Connection,
/// Transfer-Encoding, Content-Length) so hyper re-derives framing itself.
fn build_response(status: u16, headers: Vec<(String, String)>, body: Body) -> Response {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| internal_error_response())
}

fn internal_error_response() -> Response {
    Response::builder()
        .status(500)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"ok\":false,\"error\":\"internal error\"}"))
        .expect("static 500 response is well-formed")
}

fn handler_timeout_response() -> Response {
    let mut builder = Response::builder()
        .status(503)
        .header(http::header::CONTENT_TYPE, "application/json");
    for (name, value) in CORS_HEADER_PAIRS {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(
            "{\"ok\":false,\"error\":\"handler deadline exceeded\"}",
        ))
        .unwrap_or_else(|_| internal_error_response())
}

/// Outcome the [`StreamSink`] hands back to the async edge once it has
/// parsed the response head produced by the synchronous streaming handler.
enum EdgeStreamResponse {
    /// A `Content-Length`-framed response (refusal / error) collected in
    /// full — served without chunking so a refusal stays a non-streaming
    /// response.
    Buffered {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// A chunked or close-delimited (SSE) response — head parsed, body
    /// payloads streamed through the channel.
    Streaming {
        status: u16,
        headers: Vec<(String, String)>,
        body: mpsc::Receiver<Result<Bytes, io::Error>>,
    },
}

/// How the synchronous handler framed its response body.
enum BodyFraming {
    /// `Transfer-Encoding: chunked` — de-chunk payloads and stream them.
    Chunked(ChunkDecoder),
    /// No length framing (SSE, `Connection: close` delimited) — forward
    /// raw bytes until the producer finishes.
    CloseDelimited,
}

/// Sink state machine. The synchronous streaming handlers write a complete
/// raw HTTP/1.1 response (status line, headers, body) into this `Write`;
/// the sink parses the head once, then routes the body either into a
/// collected buffer (Content-Length) or an async stream (chunked / SSE).
enum SinkState {
    /// Accumulating the response head until `\r\n\r\n`.
    Head(Vec<u8>),
    /// A `Content-Length` body being collected in full.
    Buffering {
        status: u16,
        headers: Vec<(String, String)>,
        remaining: usize,
        body: Vec<u8>,
    },
    /// A streaming body being forwarded frame-by-frame.
    Streaming {
        sender: mpsc::Sender<Result<Bytes, io::Error>>,
        framing: BodyFraming,
    },
    /// Terminal: response already dispatched, further writes ignored.
    Done,
}

/// A `std::io::Write` bridge from the synchronous streaming engine to the
/// async hyper body. See [`SinkState`] for the protocol.
struct StreamSink {
    head: Option<oneshot::Sender<EdgeStreamResponse>>,
    state: SinkState,
}

impl StreamSink {
    fn new(head: oneshot::Sender<EdgeStreamResponse>) -> Self {
        Self {
            head: Some(head),
            state: SinkState::Head(Vec::with_capacity(512)),
        }
    }

    /// Drive the state machine with a fresh slice of producer bytes.
    fn consume(&mut self, data: &[u8]) -> io::Result<()> {
        // Head phase is handled first so the borrow of `self.state` is
        // released before we transition and recurse into the body phase.
        let (head_bytes, leftover) = match &mut self.state {
            SinkState::Head(buffer) => {
                buffer.extend_from_slice(data);
                match find_header_end(buffer) {
                    Some(pos) => (buffer[..pos].to_vec(), buffer[pos + 4..].to_vec()),
                    None => return Ok(()),
                }
            }
            _ => return self.consume_body(data),
        };
        self.begin_body(&head_bytes)?;
        self.consume_body(&leftover)
    }

    /// Parse the response head and pick the body strategy.
    fn begin_body(&mut self, head_bytes: &[u8]) -> io::Result<()> {
        let (status, headers, framing) = parse_response_head(head_bytes)?;
        match framing {
            HeadFraming::ContentLength(0) => {
                if let Some(head) = self.head.take() {
                    let _ = head.send(EdgeStreamResponse::Buffered {
                        status,
                        headers,
                        body: Vec::new(),
                    });
                }
                self.state = SinkState::Done;
            }
            HeadFraming::ContentLength(remaining) => {
                self.state = SinkState::Buffering {
                    status,
                    headers,
                    remaining,
                    body: Vec::with_capacity(remaining),
                };
            }
            HeadFraming::Chunked => {
                let (sender, body) = mpsc::channel(STREAM_CHANNEL_DEPTH);
                if let Some(head) = self.head.take() {
                    let _ = head.send(EdgeStreamResponse::Streaming {
                        status,
                        headers,
                        body,
                    });
                }
                self.state = SinkState::Streaming {
                    sender,
                    framing: BodyFraming::Chunked(ChunkDecoder::new()),
                };
            }
            HeadFraming::CloseDelimited => {
                let (sender, body) = mpsc::channel(STREAM_CHANNEL_DEPTH);
                if let Some(head) = self.head.take() {
                    let _ = head.send(EdgeStreamResponse::Streaming {
                        status,
                        headers,
                        body,
                    });
                }
                self.state = SinkState::Streaming {
                    sender,
                    framing: BodyFraming::CloseDelimited,
                };
            }
        }
        Ok(())
    }

    /// Feed body bytes according to the active state.
    fn consume_body(&mut self, data: &[u8]) -> io::Result<()> {
        match std::mem::replace(&mut self.state, SinkState::Done) {
            SinkState::Buffering {
                status,
                headers,
                mut remaining,
                mut body,
            } => {
                let take = remaining.min(data.len());
                body.extend_from_slice(&data[..take]);
                remaining -= take;
                if remaining == 0 {
                    if let Some(head) = self.head.take() {
                        let _ = head.send(EdgeStreamResponse::Buffered {
                            status,
                            headers,
                            body,
                        });
                    }
                    // state remains Done
                } else {
                    self.state = SinkState::Buffering {
                        status,
                        headers,
                        remaining,
                        body,
                    };
                }
                Ok(())
            }
            SinkState::Streaming {
                sender,
                mut framing,
            } => {
                let result = forward_stream(&sender, &mut framing, data);
                if result.is_ok() {
                    self.state = SinkState::Streaming { sender, framing };
                }
                // On error the state stays Done so subsequent writes are
                // dropped; the broken-pipe error returned here unwinds the
                // synchronous handler.
                result
            }
            SinkState::Head(buffer) => {
                // Reached only if begin_body left us in Head (it never
                // does); restore and wait for more bytes.
                self.state = SinkState::Head(buffer);
                Ok(())
            }
            SinkState::Done => Ok(()),
        }
    }

    /// Called once the synchronous handler returns. Flushes a partially
    /// collected Content-Length body and drops the streaming sender so the
    /// async body terminates cleanly.
    fn finish(mut self) {
        if let SinkState::Buffering {
            status,
            headers,
            body,
            ..
        } = std::mem::replace(&mut self.state, SinkState::Done)
        {
            if let Some(head) = self.head.take() {
                let _ = head.send(EdgeStreamResponse::Buffered {
                    status,
                    headers,
                    body,
                });
            }
        }
        // Any remaining streaming sender / unsent head drops here: the
        // receiver observes end-of-stream, or (for an unsent head) the
        // oneshot resolves to Err → the edge returns a 500.
    }
}

impl Write for StreamSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.consume(data)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Forward body bytes for a streaming response.
fn forward_stream(
    sender: &mpsc::Sender<Result<Bytes, io::Error>>,
    framing: &mut BodyFraming,
    data: &[u8],
) -> io::Result<()> {
    match framing {
        BodyFraming::CloseDelimited => {
            if !data.is_empty() {
                send_frame(sender, Bytes::copy_from_slice(data))?;
            }
            Ok(())
        }
        BodyFraming::Chunked(decoder) => {
            let mut frames = Vec::new();
            decoder.feed(data, &mut frames);
            for frame in frames {
                send_frame(sender, frame)?;
            }
            Ok(())
        }
    }
}

fn send_frame(sender: &mpsc::Sender<Result<Bytes, io::Error>>, frame: Bytes) -> io::Result<()> {
    sender
        .blocking_send(Ok(frame))
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "streaming client disconnected"))
}

/// Framing of a parsed response head.
enum HeadFraming {
    ContentLength(usize),
    Chunked,
    CloseDelimited,
}

/// Parse a raw HTTP/1.1 response head (status line + headers, without the
/// trailing `\r\n\r\n`). Hop-by-hop headers (`Connection`,
/// `Transfer-Encoding`, `Content-Length`) are stripped from the forwarded
/// set — hyper re-derives framing — while their values drive the framing
/// decision returned alongside.
fn parse_response_head(head: &[u8]) -> io::Result<(u16, Vec<(String, String)>, HeadFraming)> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status line"))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|token| token.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status code"))?;

    let mut headers = Vec::new();
    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "transfer-encoding" => {
                if value.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
            }
            "content-length" => content_length = value.parse().ok(),
            "connection" => {}
            _ => headers.push((name.to_string(), value.to_string())),
        }
    }

    let framing = if chunked {
        HeadFraming::Chunked
    } else if let Some(length) = content_length {
        HeadFraming::ContentLength(length)
    } else {
        HeadFraming::CloseDelimited
    };
    Ok((status, headers, framing))
}

/// Incremental HTTP/1.1 chunked-transfer decoder. The producer writes
/// well-formed chunks (`<hex>\r\n<payload>\r\n` … `0\r\n\r\n`) but
/// `Write` boundaries are arbitrary, so the decoder tolerates a chunk
/// split across any number of `write` calls. Each complete chunk payload
/// is emitted as one [`Bytes`] frame, preserving the producer's flush
/// boundaries (one engine flush → one wire chunk downstream).
struct ChunkDecoder {
    state: ChunkState,
    size_line: Vec<u8>,
    remaining: usize,
    payload: Vec<u8>,
}

enum ChunkState {
    Size,
    Data,
    TrailingCrlf(usize),
    Done,
}

impl ChunkDecoder {
    fn new() -> Self {
        Self {
            state: ChunkState::Size,
            size_line: Vec::new(),
            remaining: 0,
            payload: Vec::new(),
        }
    }

    fn feed(&mut self, mut data: &[u8], frames: &mut Vec<Bytes>) {
        while !data.is_empty() {
            match self.state {
                ChunkState::Size => {
                    if let Some(idx) = data.iter().position(|&b| b == b'\n') {
                        self.size_line.extend_from_slice(&data[..idx]);
                        data = &data[idx + 1..];
                        let line = String::from_utf8_lossy(&self.size_line);
                        let hex = line.trim().split(';').next().unwrap_or("").trim();
                        let size = usize::from_str_radix(hex, 16).unwrap_or(0);
                        self.size_line.clear();
                        if size == 0 {
                            self.state = ChunkState::Done;
                        } else {
                            self.remaining = size;
                            self.payload.clear();
                            self.payload.reserve(size);
                            self.state = ChunkState::Data;
                        }
                    } else {
                        self.size_line.extend_from_slice(data);
                        data = &[];
                    }
                }
                ChunkState::Data => {
                    let take = self.remaining.min(data.len());
                    self.payload.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    self.remaining -= take;
                    if self.remaining == 0 {
                        frames.push(Bytes::from(std::mem::take(&mut self.payload)));
                        self.state = ChunkState::TrailingCrlf(2);
                    }
                }
                ChunkState::TrailingCrlf(rem) => {
                    let take = rem.min(data.len());
                    data = &data[take..];
                    let left = rem - take;
                    self.state = if left == 0 {
                        ChunkState::Size
                    } else {
                        ChunkState::TrailingCrlf(left)
                    };
                }
                ChunkState::Done => data = &[],
            }
        }
    }
}

/// Build a multi-threaded tokio runtime for the primary HTTP listener.
/// The engine work runs on the shared `spawn_blocking` pool, so the edge
/// itself wants only enough workers to drive I/O.
pub(crate) fn build_edge_runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Build a lighter runtime for background / secondary listeners (admin /
/// metrics ports, the dual-server HTTP side, and test servers). A bounded
/// worker count keeps the thread footprint small when many listeners run
/// in one process; the synchronous engine still parallelises through
/// `spawn_blocking`.
pub(crate) fn build_background_edge_runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

/// Convenience: convert an `Arc<rustls::ServerConfig>` into the tokio-rustls
/// acceptor the edge serves with.
pub(crate) fn tls_acceptor(config: Arc<rustls::ServerConfig>) -> tokio_rustls::TlsAcceptor {
    tokio_rustls::TlsAcceptor::from(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(rx: &mut mpsc::Receiver<Result<Bytes, io::Error>>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(item) = rx.try_recv() {
            out.push(item.expect("frame ok").to_vec());
        }
        out
    }

    #[test]
    fn chunk_decoder_emits_one_frame_per_chunk() {
        let mut decoder = ChunkDecoder::new();
        let mut frames = Vec::new();
        // "a\r\n{\"row\":1}\n\r\n" = 0xa (10) byte payload.
        decoder.feed(b"a\r\n{\"row\":1}\n\r\n", &mut frames);
        decoder.feed(b"3\r\nend\r\n0\r\n\r\n", &mut frames);
        let decoded: Vec<String> = frames
            .iter()
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        assert_eq!(
            decoded,
            vec!["{\"row\":1}\n".to_string(), "end".to_string()]
        );
    }

    #[test]
    fn chunk_decoder_tolerates_split_writes() {
        let mut decoder = ChunkDecoder::new();
        let mut frames = Vec::new();
        // Same two chunks as above, but fed one byte at a time.
        let raw = b"a\r\n{\"row\":1}\n\r\n3\r\nend\r\n0\r\n\r\n";
        for byte in raw {
            decoder.feed(&[*byte], &mut frames);
        }
        let decoded: Vec<String> = frames
            .iter()
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        assert_eq!(
            decoded,
            vec!["{\"row\":1}\n".to_string(), "end".to_string()]
        );
    }

    #[test]
    fn parse_head_strips_hop_by_hop_and_detects_chunked() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nAccess-Control-Allow-Origin: *";
        let (status, headers, framing) = parse_response_head(head).expect("parse");
        assert_eq!(status, 200);
        assert!(matches!(framing, HeadFraming::Chunked));
        // Connection + Transfer-Encoding stripped; CORS + Content-Type kept.
        assert!(headers
            .iter()
            .any(|(n, v)| n == "Content-Type" && v == "application/x-ndjson"));
        assert!(headers
            .iter()
            .any(|(n, _)| n == "Access-Control-Allow-Origin"));
        assert!(!headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("connection")));
        assert!(!headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("transfer-encoding")));
    }

    #[tokio::test]
    async fn sink_routes_content_length_refusal_as_buffered() {
        let (tx, rx) = oneshot::channel();
        let mut sink = StreamSink::new(tx);
        // A typical refusal: HttpResponse::to_http_bytes output shape.
        let body = b"{\"ok\":false,\"code\":\"x\"}";
        let head = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        sink.write_all(head.as_bytes()).unwrap();
        sink.write_all(body).unwrap();
        sink.finish();
        match rx.await.expect("head") {
            EdgeStreamResponse::Buffered {
                status,
                body: collected,
                ..
            } => {
                assert_eq!(status, 400);
                assert_eq!(collected, body);
            }
            EdgeStreamResponse::Streaming { .. } => {
                panic!("refusal must be buffered, not streamed")
            }
        }
    }

    #[tokio::test]
    async fn sink_streams_chunked_body_frames() {
        let (tx, rx) = oneshot::channel();
        // The sink uses `blocking_send`, which panics inside a tokio
        // worker — in production it runs on the `spawn_blocking` pool, so
        // drive it from a plain std thread here.
        let writer = std::thread::spawn(move || {
            let mut sink = StreamSink::new(tx);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            sink.write_all(head.as_bytes()).unwrap();
            sink.write_all(b"5\r\nhello\r\n").unwrap();
            sink.write_all(b"5\r\nworld\r\n").unwrap();
            sink.write_all(b"0\r\n\r\n").unwrap();
            sink.finish();
        });
        let response = rx.await.expect("head");
        writer.join().unwrap();
        match response {
            EdgeStreamResponse::Streaming {
                status, mut body, ..
            } => {
                assert_eq!(status, 200);
                let frames = drain(&mut body);
                assert_eq!(frames, vec![b"hello".to_vec(), b"world".to_vec()]);
            }
            EdgeStreamResponse::Buffered { .. } => panic!("chunked body must stream"),
        }
    }

    #[tokio::test]
    async fn sink_streams_close_delimited_sse_body() {
        let (tx, rx) = oneshot::channel();
        let writer = std::thread::spawn(move || {
            let mut sink = StreamSink::new(tx);
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            sink.write_all(head.as_bytes()).unwrap();
            sink.write_all(b"data: one\n\n").unwrap();
            sink.write_all(b"data: two\n\n").unwrap();
            sink.finish();
        });
        let response = rx.await.expect("head");
        writer.join().unwrap();
        match response {
            EdgeStreamResponse::Streaming { mut body, .. } => {
                let frames = drain(&mut body);
                let joined: Vec<u8> = frames.concat();
                assert_eq!(joined, b"data: one\n\ndata: two\n\n");
            }
            EdgeStreamResponse::Buffered { .. } => panic!("SSE must stream"),
        }
    }
}
