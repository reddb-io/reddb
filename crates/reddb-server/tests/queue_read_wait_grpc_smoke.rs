//! Issue #732 / slice G of PRD #718 — gRPC smoke for
//! `QUEUE READ … WAIT <duration>`.
//!
//! The runtime path (#728), telemetry (#729), and HTTP transport
//! (#730) are already pinned by their own runtime-level / smoke
//! tests. This file exists only to verify that the four canonical
//! WAIT outcomes survive a real gRPC round-trip through
//! `RedDBGrpcServer`:
//!
//!   1. Empty queue + WAIT 800ms → reply with `record_count == 0`,
//!      and the round-trip blocks for ~the budget.
//!   2. Producer enqueues from a separate gRPC client during the
//!      waiter's WAIT → the message is delivered to the waiter.
//!   3. WAIT > server cap → tonic `Status` error whose message
//!      names the cap key (`red.config.queue.max_wait_ms`) and the
//!      active cap value (60000ms default).
//!   4. Registry cancellation while a WAIT is parked → tonic
//!      `Status` error with the explicit `QUEUE READ WAIT cancelled`
//!      message — not an empty 0-record reply.
//!
//! The brief frames cancellation as "tonic request cancellation".
//! The Query RPC handler delegates to a synchronous
//! `RedDBRuntime::execute_query` call which blocks the tokio task;
//! tonic-side request cancellation cannot unwind a sync blocked
//! call, so the only cancellation primitive that reaches a parked
//! waiter today is `QueueWaitRegistry::cancel_all()`. The smoke
//! pins the gRPC propagation path the same way the #728 runtime
//! test pins the runtime path and the #730 HTTP smoke pins the
//! HTTP path. A follow-up slice can wire per-request cancellation
//! into the gRPC handler once the runtime grows a per-waiter
//! cancel handle reachable from the async handler; the assertion
//! here (tonic error + explicit message) is the contract that
//! wiring must continue to satisfy.

use reddb_server::grpc::{proto, GrpcServerOptions, RedDBGrpcServer};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tonic::transport::Channel;
use tonic::Request;

use proto::red_db_client::RedDbClient;
use proto::QueryRequest;

struct ServerHandle {
    addr: std::net::SocketAddr,
    runtime: RedDBRuntime,
    _server_task: JoinHandle<()>,
}

async fn start_grpc_server() -> ServerHandle {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let server = RedDBGrpcServer::with_options(
        runtime.clone(),
        GrpcServerOptions::default(),
        Arc::new(reddb_server::auth::store::AuthStore::new(
            reddb_server::auth::AuthConfig::default(),
        )),
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("server addr");
    let server_for_task = server.clone();
    let task = tokio::spawn(async move {
        let _ = server_for_task.serve_on(listener).await;
    });

    // Tonic spins up lazily; give the listener a tick to start
    // accepting before the client connects.
    sleep(Duration::from_millis(50)).await;

    ServerHandle {
        addr,
        runtime,
        _server_task: task,
    }
}

async fn connect_client(addr: std::net::SocketAddr) -> RedDbClient<Channel> {
    let endpoint = format!("http://{addr}");
    RedDbClient::connect(endpoint)
        .await
        .expect("grpc client connects")
}

fn query(sql: &str) -> QueryRequest {
    QueryRequest {
        query: sql.to_string(),
        entity_types: Vec::new(),
        capabilities: Vec::new(),
        params: Vec::new(),
    }
}

async fn exec_ok(client: &mut RedDbClient<Channel>, sql: &str) -> proto::QueryReply {
    client
        .query(Request::new(query(sql)))
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_wait_returns_empty_after_budget_when_queue_stays_empty() {
    let h = start_grpc_server().await;
    let mut client = connect_client(h.addr).await;

    exec_ok(&mut client, "CREATE QUEUE qgrpc_empty").await;
    exec_ok(&mut client, "QUEUE GROUP CREATE qgrpc_empty workers").await;

    let started = Instant::now();
    let reply = exec_ok(
        &mut client,
        "QUEUE READ qgrpc_empty GROUP workers CONSUMER c1 COUNT 1 WAIT 800ms",
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        reply.record_count, 0,
        "WAIT timeout should deliver an empty projection over gRPC, json={}",
        reply.result_json
    );
    assert!(
        elapsed >= Duration::from_millis(750),
        "round-trip should park at least ~the WAIT budget, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "round-trip should not stall past the budget, elapsed={elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_enqueue_during_wait_delivers_message_to_waiter() {
    let h = start_grpc_server().await;
    let mut waiter = connect_client(h.addr).await;
    let mut producer = connect_client(h.addr).await;

    exec_ok(&mut waiter, "CREATE QUEUE qgrpc_wake").await;
    exec_ok(&mut waiter, "QUEUE GROUP CREATE qgrpc_wake workers").await;

    let producer_task = tokio::spawn(async move {
        sleep(Duration::from_millis(150)).await;
        exec_ok(&mut producer, "QUEUE PUSH qgrpc_wake 'wakeup'").await
    });

    let started = Instant::now();
    let reply = exec_ok(
        &mut waiter,
        "QUEUE READ qgrpc_wake GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
    )
    .await;
    let elapsed = started.elapsed();
    producer_task.await.expect("producer joined");

    assert_eq!(
        reply.record_count, 1,
        "committed enqueue must wake the waiter and deliver over gRPC, json={}",
        reply.result_json
    );
    assert!(
        reply.result_json.contains("wakeup"),
        "delivered payload should round-trip in result_json, json={}",
        reply.result_json
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "post-commit notify should wake well before the 5s budget (elapsed={elapsed:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_wait_above_cap_is_rejected_with_explicit_error() {
    let h = start_grpc_server().await;
    let mut client = connect_client(h.addr).await;

    exec_ok(&mut client, "CREATE QUEUE qgrpc_cap").await;
    exec_ok(&mut client, "QUEUE GROUP CREATE qgrpc_cap workers").await;

    let started = Instant::now();
    let err = client
        .query(Request::new(query(
            "QUEUE READ qgrpc_cap GROUP workers CONSUMER c1 COUNT 1 WAIT 999h",
        )))
        .await
        .expect_err("WAIT > cap should surface as a tonic error");
    let elapsed = started.elapsed();

    let msg = err.message();
    assert!(
        msg.contains("red.config.queue.max_wait_ms"),
        "rejection should name the cap key over gRPC, status={err:?}"
    );
    assert!(
        msg.contains("60000"),
        "rejection should name the active cap value (default 60000ms), status={err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "cap rejection must not park before refusing, elapsed={elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_wait_cancellation_returns_explicit_error_not_empty_timeout() {
    let h = start_grpc_server().await;
    let mut client = connect_client(h.addr).await;

    exec_ok(&mut client, "CREATE QUEUE qgrpc_cancel").await;
    exec_ok(&mut client, "QUEUE GROUP CREATE qgrpc_cancel workers").await;

    // Trigger cancellation mid-WAIT through the runtime registry —
    // see the module doc-comment for why per-request tonic
    // cancellation cannot unwind the parked sync call today.
    let registry = h.runtime.queue_wait_registry();
    let canceler = tokio::spawn(async move {
        sleep(Duration::from_millis(150)).await;
        registry.cancel_all();
    });

    let err = client
        .query(Request::new(query(
            "QUEUE READ qgrpc_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
        )))
        .await
        .expect_err("cancellation should surface as a tonic error, not a 0-record reply");
    canceler.await.expect("canceler joined");

    let msg = err.message();
    assert!(
        msg.contains("WAIT cancelled") || msg.contains("shutting down"),
        "cancellation error should be explicit over gRPC, status={err:?}"
    );

    // Leave the shared registry in a known state in case future cases
    // share a process-wide counter (none today, cheap insurance).
    h.runtime.queue_wait_registry().reset_cancelled();
}
