//! Issue #731 / slice F of PRD #718 — RedWire transport smoke for
//! `QUEUE READ … WAIT <duration>`.
//!
//! The runtime acceptance is pinned by `queue_read_wait_runtime.rs`
//! and `queue_read_wait_cap_and_txn.rs` (both at the
//! `RedDBRuntime::execute_query` level). This file re-verifies the
//! four canonical cases when the WAIT is dispatched over RedWire,
//! i.e. with the engine listener bound on an ephemeral port and
//! the request shipped through the published
//! `RedWireClient`.
//!
//! Cases pinned here:
//!
//!   1. Empty queue + `WAIT 1s` returns an empty projection after
//!      ~the budget — the timeout path travels through RedWire as a
//!      normal `Result` frame and the runtime accounts the outcome
//!      as `wait_timed_out`.
//!   2. A second client enqueues during the wait — the parked
//!      waiter wakes well before the budget and the runtime accounts
//!      the outcome as `wait_woken`.
//!   3. `WAIT` above the server cap is rejected with a clear error
//!      frame (no parking, no fake empty timeout).
//!   4. Server-side cancellation (`QueueWaitRegistry::cancel_all`)
//!      releases the parked waiter through the `wait_cancelled`
//!      outcome rather than a timeout. This is the cancellation
//!      surface that connection-close drives in transports where
//!      the session can signal it; per-connection close detection
//!      in the redwire session loop itself remains a follow-up. The
//!      contract this slice pins is the wire-level cancellation
//!      *outcome*: the runtime's parked WAIT terminates through the
//!      `Cancelled` branch (counter increments by exactly one) and
//!      not through `Timeout`, even when the request originated on
//!      a RedWire `Query` frame.
//!
//! These run on a single Tokio thread so a synchronous WAIT in the
//! session task cannot hide behind additional I/O workers. Both the
//! legacy summary frame and the full result envelope must wake and
//! surface cancellation without starving the producer or cancellation task.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb::api::RedDBOptions;
use reddb::health::HealthProvider;
use reddb::wire::redwire::start_redwire_listener_on;
use reddb::RedDBRuntime;
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::ErrorCode;
use reddb_wire::redwire::{
    encode_execute_prepared_payload, encode_frame, encode_prepare_payload, read_frame_async, Frame,
    MessageKind, REDWIRE_MAGIC,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Bind the listener on :0, hand the chosen `addr` back so tests
/// can connect, and return the runtime handle so the test can poke
/// runtime-internal state (registry, telemetry) the wire path does
/// not surface yet.
async fn start_server() -> (SocketAddr, Arc<RedDBRuntime>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().expect("ephemeral address");

    let runtime =
        Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"));
    let rt_for_listener = runtime.clone();
    let handle = tokio::spawn(async move {
        start_redwire_listener_on(listener, rt_for_listener)
            .await
            .expect("RedWire listener");
    });
    (addr, runtime, handle)
}

async fn connect(addr: SocketAddr) -> RedWireClient {
    RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("connect")
}

/// Snapshot one (scope, queue) row out of the per-queue wait
/// counters. The runtime keys these by `(scope, queue)`; for
/// in-memory anonymous sessions the scope is the empty string.
fn wait_counts(runtime: &RedDBRuntime, queue: &str) -> (u64, u64, u64, u64) {
    let snap = runtime.queue_telemetry_snapshot();
    let pick = |rows: &Vec<((String, String), u64)>| -> u64 {
        let m: BTreeMap<_, _> = rows
            .iter()
            .map(|((s, q), n)| ((s.clone(), q.clone()), *n))
            .collect();
        m.get(&(String::new(), queue.to_string()))
            .copied()
            .unwrap_or(0)
    };
    (
        pick(&snap.wait_started),
        pick(&snap.wait_woken),
        pick(&snap.wait_timed_out),
        pick(&snap.wait_cancelled),
    )
}

/// Poll `runtime`'s `wait_started` count for `queue` until it
/// reaches at least `target`, or the deadline elapses. Returns
/// `true` if the threshold was observed in time. The poll cadence
/// is short (5 ms) so the cancel/notify follow-up still lands
/// inside the WAIT budget under heavy test-parallel CPU load.
async fn wait_for_started(
    runtime: &RedDBRuntime,
    queue: &str,
    target: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let (started, _, _, _) = wait_counts(runtime, queue);
        if started >= target {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (started, _, _, _) = wait_counts(runtime, queue);
    started >= target
}

#[tokio::test]
async fn wait_returns_empty_after_budget_over_redwire() {
    let (addr, runtime, _server) = start_server().await;
    let mut client = connect(addr).await;

    client
        .query("CREATE QUEUE qrw_empty")
        .await
        .expect("create queue");
    client
        .query("QUEUE GROUP CREATE qrw_empty workers")
        .await
        .expect("create group");

    let started = Instant::now();
    let result = client
        .query("QUEUE READ qrw_empty GROUP workers CONSUMER c1 COUNT 1 WAIT 1s")
        .await
        .expect("WAIT over RedWire should succeed with an empty projection on timeout");
    let elapsed = started.elapsed();

    assert!(
        result.affected == 0,
        "timeout must surface affected=0, got {}",
        result.affected
    );
    assert!(
        elapsed >= Duration::from_millis(900),
        "should park ~the WAIT budget, elapsed={elapsed:?}"
    );
    // 3s gives slow CI plenty of slack while still rejecting any
    // path that ignores the budget entirely.
    assert!(
        elapsed < Duration::from_secs(3),
        "should not stall past the budget, elapsed={elapsed:?}"
    );

    let (started_n, woken, timed_out, cancelled) = wait_counts(&runtime, "qrw_empty");
    assert_eq!(started_n, 1, "exactly one WAIT lifecycle started");
    assert_eq!(timed_out, 1, "outcome must be Timeout");
    assert_eq!(woken, 0, "no wake fired on a quiet queue");
    assert_eq!(cancelled, 0, "no cancellation on a quiet queue");

    client.close().await.ok();
}

#[tokio::test]
async fn enqueue_from_second_client_wakes_waiter_over_redwire() {
    assert_enqueue_wakes_waiter(false).await;
}

#[tokio::test]
async fn enqueue_wakes_legacy_query_waiter_over_redwire() {
    assert_enqueue_wakes_waiter(true).await;
}

async fn assert_enqueue_wakes_waiter(legacy_query: bool) {
    let (addr, runtime, _server) = start_server().await;

    // Setup over a throwaway client so the waiter's connection
    // starts fresh and stays parked exclusively in QUEUE READ.
    let mut setup = connect(addr).await;
    setup
        .query("CREATE QUEUE qrw_wake")
        .await
        .expect("create queue");
    setup
        .query("QUEUE GROUP CREATE qrw_wake workers")
        .await
        .expect("create group");
    setup.close().await.ok();

    let mut waiter = connect(addr).await;
    let mut producer = connect(addr).await;

    // Producer pushes only after telemetry confirms the waiter is
    // parked. Polling on `wait_started` is more robust than a fixed
    // sleep — under heavy `cargo test` parallelism the waiter may
    // need >50 ms to reach the park loop on the server.
    let rt_for_producer = runtime.clone();
    let producer_task = tokio::spawn(async move {
        let parked =
            wait_for_started(&rt_for_producer, "qrw_wake", 1, Duration::from_secs(3)).await;
        assert!(parked, "waiter never registered with the wait registry");
        producer
            .query("QUEUE PUSH qrw_wake 'live'")
            .await
            .expect("push from second client");
    });

    let started = Instant::now();
    let sql = "QUEUE READ qrw_wake GROUP workers CONSUMER c1 COUNT 1 WAIT 5s";
    if legacy_query {
        waiter.query_raw(sql).await.expect("legacy WAIT wakes");
    } else {
        let result = waiter.query(sql).await.expect("WAIT wakes with records");
        assert_eq!(result.rows.len(), 1, "the committed item must be delivered");
    }
    let elapsed = started.elapsed();
    producer_task.await.expect("producer task joined");

    assert!(
        elapsed < Duration::from_secs(4),
        "commit on a second client must wake the waiter before the budget, elapsed={elapsed:?}"
    );
    let (started_n, woken, timed_out, _cancelled) = wait_counts(&runtime, "qrw_wake");
    assert_eq!(started_n, 1, "exactly one WAIT lifecycle started");
    assert_eq!(
        woken, 1,
        "outcome must be Woken (got woken={woken}, timed_out={timed_out})"
    );
    assert_eq!(
        timed_out, 0,
        "wake must not be misclassified as Timeout, got timed_out={timed_out}"
    );

    waiter.close().await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_above_server_cap_is_rejected_over_redwire() {
    let (addr, runtime, _server) = start_server().await;
    let mut client = connect(addr).await;

    client
        .query("CREATE QUEUE qrw_cap")
        .await
        .expect("create queue");
    client
        .query("QUEUE GROUP CREATE qrw_cap workers")
        .await
        .expect("create group");

    // Default cap is 60_000 ms. 999h is far above any sane cap.
    let started = Instant::now();
    let err = client
        .query("QUEUE READ qrw_cap GROUP workers CONSUMER c1 COUNT 1 WAIT 999h")
        .await
        .expect_err("WAIT above the cap must surface as an Error frame");
    let elapsed = started.elapsed();

    assert_eq!(
        err.code,
        ErrorCode::Engine,
        "cap rejection must arrive as an engine Error frame, got {:?}",
        err.code
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("red.config.queue.max_wait_ms"),
        "error frame should name the cap key for operators, got: {msg:?}"
    );
    assert!(
        msg.contains("60000"),
        "error frame should name the active cap value, got: {msg:?}"
    );
    // No parking — the cap check fires before the waiter is
    // registered, so the round-trip stays trivially short even
    // accounting for network overhead.
    assert!(
        elapsed < Duration::from_millis(500),
        "cap rejection must not park, elapsed={elapsed:?}"
    );
    let (started_n, _, _, _) = wait_counts(&runtime, "qrw_cap");
    assert_eq!(
        started_n, 0,
        "rejected WAIT must not register a wait lifecycle"
    );

    client.close().await.ok();
}

#[tokio::test]
async fn server_cancellation_surfaces_explicit_outcome_over_redwire() {
    assert_server_cancellation(false).await;
}

#[tokio::test]
async fn server_cancellation_reaches_legacy_query_waiter_over_redwire() {
    assert_server_cancellation(true).await;
}

async fn assert_server_cancellation(legacy_query: bool) {
    let (addr, runtime, _server) = start_server().await;

    let mut setup = connect(addr).await;
    setup
        .query("CREATE QUEUE qrw_cancel")
        .await
        .expect("create queue");
    setup
        .query("QUEUE GROUP CREATE qrw_cancel workers")
        .await
        .expect("create group");
    setup.close().await.ok();

    let mut waiter = connect(addr).await;

    // Drive the cancellation server-side once the waiter is parked.
    // In transports that detect connection-close mid-WAIT this same
    // path fires on disconnect; redwire today exposes the surface
    // via the registry hook (per-connection close detection in the
    // redwire session loop is a follow-up). The point of this smoke
    // is that the *cancellation outcome* travels through the
    // runtime distinctly from a timeout — even when the request
    // originated on the wire, the parked WAIT terminates through
    // `wait_cancelled`, not `wait_timed_out`.
    let cancel_rt = runtime.clone();
    let cancel_task = tokio::spawn(async move {
        let parked = wait_for_started(&cancel_rt, "qrw_cancel", 1, Duration::from_secs(3)).await;
        assert!(parked, "waiter never registered before cancel");
        cancel_rt.queue_wait_registry().cancel_all();
    });

    let started = Instant::now();
    let sql = "QUEUE READ qrw_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s";
    let error = if legacy_query {
        waiter
            .query_raw(sql)
            .await
            .expect_err("legacy WAIT cancelled")
    } else {
        waiter.query(sql).await.expect_err("WAIT cancelled")
    };
    assert_eq!(error.code, ErrorCode::Engine);
    assert!(
        error.message.contains("QUEUE READ WAIT cancelled"),
        "{error}"
    );
    let elapsed = started.elapsed();
    cancel_task.await.expect("cancel task joined");

    assert!(
        elapsed < Duration::from_secs(4),
        "cancellation must release the waiter before the WAIT budget, elapsed={elapsed:?}"
    );
    let (started_n, woken, timed_out, cancelled) = wait_counts(&runtime, "qrw_cancel");
    assert_eq!(started_n, 1, "exactly one WAIT lifecycle started");
    assert_eq!(
        cancelled, 1,
        "outcome must be Cancelled (got cancelled={cancelled}, timed_out={timed_out}, woken={woken})"
    );
    assert_eq!(
        timed_out, 0,
        "cancellation must not be misclassified as Timeout"
    );
    assert_eq!(woken, 0, "cancellation must not be misclassified as Woken");

    // Per-test isolation: reset the registry flag so any test
    // running after this one in the same process does not inherit
    // a sticky cancellation. The runtime is per-test so the slot
    // map and telemetry are already isolated; only the flag is
    // process-visible.
    runtime.queue_wait_registry().reset_cancelled();
}

async fn connect_raw(addr: SocketAddr) -> TcpStream {
    let mut socket = TcpStream::connect(addr).await.expect("raw connection");
    socket
        .write_all(&[REDWIRE_MAGIC, 1])
        .await
        .expect("startup");
    let hello = Frame::new(
        MessageKind::Hello,
        1,
        br#"{"versions":[1],"auth_methods":["anonymous"],"features":0,"client_name":"wait-regression"}"#.to_vec(),
    );
    socket
        .write_all(&encode_frame(&hello))
        .await
        .expect("hello");
    assert_eq!(
        read_frame_async(&mut socket).await.expect("hello ack").kind,
        MessageKind::HelloAck
    );
    let auth = Frame::new(MessageKind::AuthResponse, 2, b"{}".to_vec());
    socket
        .write_all(&encode_frame(&auth))
        .await
        .expect("anonymous auth");
    assert_eq!(
        read_frame_async(&mut socket).await.expect("auth ack").kind,
        MessageKind::AuthOk
    );
    socket
}

#[tokio::test]
async fn enqueue_wakes_binary_query_waiter_over_redwire() {
    assert_binary_enqueue_wakes_waiter(false).await;
}

#[tokio::test]
async fn enqueue_wakes_prepared_query_waiter_over_redwire() {
    assert_binary_enqueue_wakes_waiter(true).await;
}

async fn assert_binary_enqueue_wakes_waiter(prepared: bool) {
    let (addr, runtime, _server) = start_server().await;
    let mut producer = connect(addr).await;
    producer
        .query("CREATE QUEUE qrw_binary")
        .await
        .expect("queue");
    producer
        .query("QUEUE GROUP CREATE qrw_binary workers")
        .await
        .expect("group");
    let mut waiter = connect_raw(addr).await;
    let sql = "QUEUE READ qrw_binary GROUP workers CONSUMER c1 COUNT 1 WAIT 5s";
    let frame = if prepared {
        let prepare = Frame::new(
            MessageKind::Prepare,
            3,
            encode_prepare_payload(41, sql).expect("prepare payload"),
        );
        waiter
            .write_all(&encode_frame(&prepare))
            .await
            .expect("prepare");
        let reply = read_frame_async(&mut waiter).await.expect("prepared ack");
        assert_eq!(reply.kind, MessageKind::PreparedOk, "{reply:?}");
        assert_eq!(u16::from_le_bytes([reply.payload[4], reply.payload[5]]), 0);
        // Run an intervening query through the worker before executing the
        // prepared ID: its connection-owned registry must survive the handoff.
        let query = Frame::new(MessageKind::Query, 4, b"SELECT 1".to_vec());
        waiter
            .write_all(&encode_frame(&query))
            .await
            .expect("intervening query");
        assert_eq!(
            read_frame_async(&mut waiter)
                .await
                .expect("query reply")
                .kind,
            MessageKind::Result
        );
        Frame::new(
            MessageKind::ExecutePrepared,
            5,
            encode_execute_prepared_payload(41, &[]).expect("execute payload"),
        )
    } else {
        Frame::new(MessageKind::QueryBinary, 5, sql.as_bytes().to_vec())
    };
    let rt_for_producer = Arc::clone(&runtime);
    let producer_task = tokio::spawn(async move {
        assert!(wait_for_started(&rt_for_producer, "qrw_binary", 1, Duration::from_secs(3)).await);
        producer
            .query("QUEUE PUSH qrw_binary 'live'")
            .await
            .expect("push");
        producer.close().await.expect("producer close");
    });
    let started = Instant::now();
    waiter
        .write_all(&encode_frame(&frame))
        .await
        .expect("wait query");
    let reply = read_frame_async(&mut waiter).await.expect("wait result");
    assert_eq!(reply.kind, MessageKind::Result, "{reply:?}");
    assert_eq!(reply.correlation_id, 5);
    assert!(started.elapsed() < Duration::from_secs(4));
    producer_task.await.expect("producer joined");
    assert_eq!(wait_counts(&runtime, "qrw_binary"), (1, 1, 0, 0));
}

#[tokio::test]
async fn cancelled_session_retains_lease_until_blocking_query_finishes() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let runtime = Arc::new(RedDBRuntime::in_memory().expect("runtime"));
        runtime
            .execute_query("CREATE QUEUE qrw_lease")
            .expect("queue");
        runtime
            .execute_query("QUEUE GROUP CREATE qrw_lease workers")
            .expect("group");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("address");
        let rt_for_session = Arc::clone(&runtime);
        let session = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            assert_eq!(socket.read_u8().await.expect("magic"), REDWIRE_MAGIC);
            reddb::wire::redwire::session::handle_session(socket, rt_for_session, None, None)
                .await
                .expect("session");
        });
        let mut socket = connect_raw(addr).await;
        let frame = Frame::new(
            MessageKind::Query,
            3,
            b"QUEUE READ qrw_lease GROUP workers CONSUMER c1 COUNT 1 WAIT 5s".to_vec(),
        );
        socket.write_all(&encode_frame(&frame)).await.expect("WAIT");
        assert!(wait_for_started(&runtime, "qrw_lease", 1, Duration::from_secs(3)).await);
        session.abort();
        assert!(session.await.expect_err("session aborted").is_cancelled());
        assert_eq!(
            runtime
                .health()
                .diagnostics
                .get("runtime.active_connections"),
            Some(&"1".to_string()),
            "the running query must still own its lease"
        );
        runtime.queue_wait_registry().cancel_all();
        while runtime
            .health()
            .diagnostics
            .get("runtime.active_connections")
            != Some(&"0".to_string())
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(wait_counts(&runtime, "qrw_lease"), (1, 0, 0, 1));
    })
    .await
    .expect("query releases lease after cancellation");
}
