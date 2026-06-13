//! Issue #731 / slice F of PRD #718 — RedWire transport smoke for
//! `QUEUE READ … WAIT <duration>`.
//!
//! The runtime acceptance is pinned by `queue_read_wait_runtime.rs`
//! and `queue_read_wait_cap_and_txn.rs` (both at the
//! `RedDBRuntime::execute_query` level). This file re-verifies the
//! four canonical cases when the WAIT is dispatched over RedWire,
//! i.e. with the engine listener bound on an ephemeral port and
//! the request shipped as a `Query` frame through the published
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
//! Telemetry, not record count, is the assertion of record in #2
//! and #4: the JSON `Query` envelope today carries `affected` /
//! `statement` only, so a parked WAIT that wakes still appears to
//! the client as a `Result` frame with zero rows. The behaviour
//! the brief calls out — *wake* vs *timeout* vs *cancel* — lives
//! in the runtime's `wait_*` counters, which `queue_telemetry_snapshot`
//! exposes process-locally per (scope, queue). We assert there to
//! avoid coupling the smoke to envelope shape that lives in the
//! presentation layer.

#![cfg(all(feature = "redwire", feature = "embedded"))]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb::api::RedDBOptions;
use reddb::wire::redwire::{start_redwire_listener, RedWireConfig};
use reddb::RedDBRuntime;
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::ErrorCode;
use tokio::net::TcpListener;

/// Bind the listener on :0, hand the chosen `addr` back so tests
/// can connect, and return the runtime handle so the test can poke
/// runtime-internal state (registry, telemetry) the wire path does
/// not surface yet.
async fn start_server() -> (SocketAddr, Arc<RedDBRuntime>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: None,
        oauth: None,
    };
    let rt_for_listener = runtime.clone();
    let handle = tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, rt_for_listener).await;
    });
    // Give the listener time to bind. 50 ms matches the rest of the
    // RedWire smoke suite.
    tokio::time::sleep(Duration::from_millis(50)).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enqueue_from_second_client_wakes_waiter_over_redwire() {
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
    let _result = waiter
        .query("QUEUE READ qrw_wake GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .await
        .expect("WAIT should return through the wire once the producer commits");
    let elapsed = started.elapsed();
    producer_task.await.expect("producer task joined");

    // The wire envelope strips records (the JSON `Query` reply
    // carries `affected` only — see notes at the top of the file).
    // The signal that matters is *when* the waiter unblocked: the
    // wake fired well before the 5s budget, and the runtime
    // accounted it under `wait_woken`, not `wait_timed_out`.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_cancellation_surfaces_explicit_outcome_over_redwire() {
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
    // Either the client sees an explicit engine Error frame (the
    // usual happy path — runtime returns Err, run_query maps to an
    // Error frame), or an empty Result frame if the cancellation
    // races with the wire response. Either way the *runtime* must
    // have terminated the parked WAIT through the Cancelled branch,
    // which is what the telemetry assertion below pins.
    let _ = waiter
        .query("QUEUE READ qrw_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .await;
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
