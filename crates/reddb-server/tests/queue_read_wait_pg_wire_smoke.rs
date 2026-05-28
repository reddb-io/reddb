//! Issue #733 / slice H of PRD #718 — PostgreSQL-wire smoke for
//! `QUEUE READ … WAIT <duration>`.
//!
//! The runtime path (#728), telemetry (#729), and the HTTP / gRPC /
//! RedWire transports (#730 / #732 / #731) are already pinned by their
//! own runtime-level and smoke tests. This file exists only to verify
//! that the four canonical WAIT outcomes survive a real PG-wire
//! round-trip through `start_pg_wire_listener` — the same simple-query
//! path PG-compatible clients drive:
//!
//!   1. Empty queue + WAIT 1s → a `SELECT 0` CommandComplete with zero
//!      DataRow frames, and the round-trip blocks for ~the budget.
//!   2. Producer enqueues from a separate PG-wire connection during the
//!      waiter's WAIT → the message is delivered to the waiter (one
//!      DataRow frame carrying the payload).
//!   3. WAIT > server cap → an ErrorResponse frame whose message names
//!      the cap key (`red.config.queue.max_wait_ms`) and the active cap
//!      value (60000ms default), with no park.
//!   4. Registry cancellation while a WAIT is parked → an ErrorResponse
//!      frame with the explicit `QUEUE READ WAIT cancelled` message —
//!      not a `SELECT 0` empty-timeout reply.
//!
//! LISTEN/NOTIFY is explicitly out of scope (PG-wire carries the RedDB
//! queue-wait behaviour only).
//!
//! On cancellation: the brief frames it as "Terminate / connection
//! drop". The pg-wire session loop drives `RedDBRuntime::execute_query`
//! synchronously inside the connection's tokio task, so a Terminate
//! frame queued behind the parked read cannot be processed until the
//! read returns, and a TCP drop is not observed until the next frame
//! read. The only cancellation primitive that reaches a parked waiter
//! today is `QueueWaitRegistry::cancel_all()` — the same sanctioned
//! pattern the landed HTTP (#730) and gRPC (#732) smokes use. The
//! load-bearing contract pinned here is the *outcome* (explicit
//! cancellation error, not an empty timeout) over a PG-wire-originated
//! WAIT. Per-connection disconnect→cancel wiring in the session loop is
//! a transport follow-up, identical to the gap noted in those smokes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb_server::wire::postgres::{start_pg_wire_listener, PgWireConfig};
use reddb_server::{RedDBOptions, RedDBRuntime};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// Generous ceiling on any single backend exchange. The WAIT budgets
/// used below are <=5s; a read that outlives this means the waiter
/// never returned (a regression), and the bound turns that hang into a
/// fast, legible failure instead of a watchdog-reaped iteration.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(20);

struct ServerHandle {
    addr: SocketAddr,
    runtime: Arc<RedDBRuntime>,
    _join: tokio::task::JoinHandle<()>,
}

async fn start_server() -> ServerHandle {
    // Probe an ephemeral port, then hand the address to the listener.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let cfg = PgWireConfig {
        bind_addr: addr.to_string(),
        ..PgWireConfig::default()
    };

    // The listener runs on this test's own multi-thread runtime — the
    // same one that fires the cancel/producer timers and pumps the
    // client socket. That is deliberate: the pg-wire session loop drives
    // the synchronous, possibly-parking `execute_query` via
    // `block_in_place`, so a parked `QUEUE READ … WAIT` frees its worker
    // for the other connections and the timer driver instead of stalling
    // the runtime. The enqueue/cancel cases below would deadlock against
    // the parked waiter if that offload regressed, so sharing one
    // runtime is the regression guard, not an accident.
    let runtime_for_task = Arc::clone(&runtime);
    let join = tokio::spawn(async move {
        let _ = start_pg_wire_listener(cfg, runtime_for_task).await;
    });
    // Listener binds inside the task; give it a tick before connecting.
    sleep(Duration::from_millis(50)).await;
    ServerHandle {
        addr,
        runtime,
        _join: join,
    }
}

/// Open a fresh PG-wire connection and complete the startup handshake,
/// leaving the stream parked at ReadyForQuery.
async fn connect(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("connect pg wire");
    write_startup(&mut stream).await;
    read_until_ready(&mut stream).await;
    stream
}

/// Run one simple-query exchange and return every backend frame up to
/// and including the terminating ReadyForQuery. Bounded by
/// `EXCHANGE_TIMEOUT` so a never-returning WAIT fails fast.
async fn simple_query(stream: &mut TcpStream, sql: &str) -> Vec<(u8, Vec<u8>)> {
    write_frontend_frame(stream, b'Q', query_body(sql)).await;
    timeout(EXCHANGE_TIMEOUT, read_until_ready(stream))
        .await
        .unwrap_or_else(|_| panic!("pg-wire exchange timed out for: {sql}"))
}

/// Convenience: run `sql` and assert it produced no ErrorResponse.
async fn exec_ok(stream: &mut TcpStream, sql: &str) -> Vec<(u8, Vec<u8>)> {
    let frames = simple_query(stream, sql).await;
    if let Some((_, body)) = frames.iter().find(|(tag, _)| *tag == b'E') {
        panic!(
            "{sql}: unexpected error frame: {:?}",
            decode_error_message(body)
        );
    }
    frames
}

fn data_row_count(frames: &[(u8, Vec<u8>)]) -> usize {
    frames.iter().filter(|(tag, _)| *tag == b'D').count()
}

fn error_message(frames: &[(u8, Vec<u8>)]) -> Option<String> {
    frames
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, body)| decode_error_message(body))
}

fn command_complete_tags(frames: &[(u8, Vec<u8>)]) -> Vec<String> {
    frames
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, body)| decode_command_complete(body).to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_wire_wait_returns_empty_after_budget_when_queue_stays_empty() {
    let h = start_server().await;
    let mut stream = connect(h.addr).await;

    exec_ok(&mut stream, "CREATE QUEUE qpg_empty").await;
    exec_ok(&mut stream, "QUEUE GROUP CREATE qpg_empty workers").await;

    let started = Instant::now();
    let frames = exec_ok(
        &mut stream,
        "QUEUE READ qpg_empty GROUP workers CONSUMER c1 COUNT 1 WAIT 1s",
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        data_row_count(&frames),
        0,
        "WAIT timeout should deliver an empty projection over PG-wire, frames={:?}",
        frame_tags(&frames)
    );
    assert!(
        command_complete_tags(&frames)
            .iter()
            .any(|tag| tag == "SELECT 0"),
        "empty WAIT should complete as SELECT 0, tags={:?}",
        command_complete_tags(&frames)
    );
    assert!(
        elapsed >= Duration::from_millis(900),
        "round-trip should park at least ~the WAIT budget, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "round-trip should not stall past the budget, elapsed={elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_wire_enqueue_during_wait_delivers_message_to_waiter() {
    let h = start_server().await;
    let mut waiter = connect(h.addr).await;

    exec_ok(&mut waiter, "CREATE QUEUE qpg_wake").await;
    exec_ok(&mut waiter, "QUEUE GROUP CREATE qpg_wake workers").await;

    let producer_addr = h.addr;
    let producer = tokio::spawn(async move {
        let mut producer = connect(producer_addr).await;
        sleep(Duration::from_millis(150)).await;
        exec_ok(&mut producer, "QUEUE PUSH qpg_wake 'wakeup'").await;
    });

    let started = Instant::now();
    let frames = exec_ok(
        &mut waiter,
        "QUEUE READ qpg_wake GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
    )
    .await;
    let elapsed = started.elapsed();
    producer.await.expect("producer joined");

    assert_eq!(
        data_row_count(&frames),
        1,
        "committed enqueue must wake the waiter and deliver over PG-wire, frames={:?}",
        frame_tags(&frames)
    );
    let delivered = frames
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, body)| body.clone())
        .expect("a DataRow frame");
    assert!(
        delivered.windows(b"wakeup".len()).any(|w| w == b"wakeup"),
        "delivered payload should round-trip in the DataRow bytes"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "post-commit notify should wake well before the 5s budget (elapsed={elapsed:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_wire_wait_above_cap_is_rejected_with_explicit_error() {
    let h = start_server().await;
    let mut stream = connect(h.addr).await;

    exec_ok(&mut stream, "CREATE QUEUE qpg_cap").await;
    exec_ok(&mut stream, "QUEUE GROUP CREATE qpg_cap workers").await;

    let started = Instant::now();
    let frames = simple_query(
        &mut stream,
        "QUEUE READ qpg_cap GROUP workers CONSUMER c1 COUNT 1 WAIT 999h",
    )
    .await;
    let elapsed = started.elapsed();

    let msg = error_message(&frames).unwrap_or_else(|| {
        panic!(
            "WAIT > cap should surface an ErrorResponse, frames={:?}",
            frame_tags(&frames)
        )
    });
    assert!(
        msg.contains("red.config.queue.max_wait_ms"),
        "rejection should name the cap key over PG-wire, msg={msg}"
    );
    assert!(
        msg.contains("60000"),
        "rejection should name the active cap value (default 60000ms), msg={msg}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "cap rejection must not park before refusing, elapsed={elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_wire_wait_cancellation_returns_explicit_error_not_empty_timeout() {
    let h = start_server().await;
    let mut stream = connect(h.addr).await;

    exec_ok(&mut stream, "CREATE QUEUE qpg_cancel").await;
    exec_ok(&mut stream, "QUEUE GROUP CREATE qpg_cancel workers").await;

    // Drive cancellation mid-WAIT through the runtime registry — see the
    // module doc-comment for why a Terminate frame / TCP drop cannot
    // unwind the parked synchronous read in today's session loop. The
    // waiter parks under `block_in_place`, so this canceler timer still
    // fires on schedule on the shared runtime and `cancel_all`'s condvar
    // wake reaches the parked waiter.
    let registry = Arc::clone(&h.runtime.queue_wait_registry());
    let canceler = tokio::spawn(async move {
        sleep(Duration::from_millis(150)).await;
        registry.cancel_all();
    });

    let frames = simple_query(
        &mut stream,
        "QUEUE READ qpg_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
    )
    .await;
    canceler.await.expect("canceler joined");

    let msg = error_message(&frames).unwrap_or_else(|| {
        panic!(
            "cancellation should surface an ErrorResponse, not a SELECT 0 reply, frames={:?}",
            frame_tags(&frames)
        )
    });
    assert!(
        msg.contains("WAIT cancelled") || msg.contains("shutting down"),
        "cancellation error should be explicit over PG-wire, msg={msg}"
    );
    assert_eq!(
        data_row_count(&frames),
        0,
        "cancellation must not deliver rows, frames={:?}",
        frame_tags(&frames)
    );

    // Leave the shared registry in a known state in case future cases
    // share a process-wide counter (none today, cheap insurance).
    h.runtime.queue_wait_registry().reset_cancelled();
}

// --- PG-wire frame helpers (subset of tests/postgres_wire_extended.rs) ---

fn frame_tags(frames: &[(u8, Vec<u8>)]) -> Vec<char> {
    frames.iter().map(|(tag, _)| *tag as char).collect()
}

async fn write_startup<W: AsyncWrite + Unpin>(stream: &mut W) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&((3u32) << 16).to_be_bytes());
    payload.extend_from_slice(b"user\0reddb\0");
    payload.push(0);
    let len = (payload.len() + 4) as u32;
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
}

async fn write_frontend_frame<W: AsyncWrite + Unpin>(stream: &mut W, tag: u8, payload: Vec<u8>) {
    stream.write_all(&[tag]).await.unwrap();
    stream
        .write_all(&((payload.len() + 4) as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
}

async fn read_backend_frame<R: AsyncRead + Unpin>(stream: &mut R) -> (u8, Vec<u8>) {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).await.unwrap();
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await.unwrap();
    let len = u32::from_be_bytes(len) as usize;
    let mut body = vec![0u8; len - 4];
    stream.read_exact(&mut body).await.unwrap();
    (tag[0], body)
}

async fn read_until_ready<R: AsyncRead + Unpin>(stream: &mut R) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    loop {
        let frame = read_backend_frame(stream).await;
        let done = frame.0 == b'Z';
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

fn query_body(query: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(query.as_bytes());
    out.push(0);
    out
}

fn decode_command_complete(body: &[u8]) -> &str {
    let nul = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    std::str::from_utf8(&body[..nul]).unwrap()
}

fn decode_error_message(body: &[u8]) -> Option<String> {
    let mut pos = 0;
    while pos < body.len() {
        let field = body[pos];
        pos += 1;
        if field == 0 {
            break;
        }
        let end = body[pos..].iter().position(|&b| b == 0)? + pos;
        let value = std::str::from_utf8(&body[pos..end]).ok()?.to_string();
        pos = end + 1;
        if field == b'M' {
            return Some(value);
        }
    }
    None
}
