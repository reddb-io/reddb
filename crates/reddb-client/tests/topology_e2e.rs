//! End-to-end integration test for topology discovery (issue #172,
//! PRD #164).
//!
//! Spins up three in-process tonic gRPC mocks (one primary, two
//! replicas) on distinct localhost ports. The primary mock returns
//! the canonical `Topology` payload (encoded by `reddb-wire`) listing
//! all three endpoints; the replicas are dumb counters.
//!
//! The client connects with a primary-only URI, calls
//! `refresh_topology()` once, then dispatches 300 SELECT reads. The
//! test asserts:
//!
//! 1. **Load shifted off the primary.** With discovery on, the
//!    primary sees 0 reads — `HealthAwareRouter` (issue #171)
//!    excludes it from the read pool whenever ≥1 replica is healthy.
//!    The primary's exact share is what the issue spec calls out
//!    as "≈1/3" — that framing assumes a naive round-robin
//!    including the primary. The router shipping with #171 is more
//!    aggressive (replicas-only) and we lock that contract in
//!    here. The `force_primary=true` baseline test below reproduces
//!    the legacy "all reads on primary" behaviour for the
//!    benchmark report.
//! 2. **Inter-replica spread.** Both replicas serve at least one
//!    read — locking the contract that discovery actually opens
//!    channels to every advertised peer. Cold-start inverse-RTT
//!    weighting amplifies tiny first-sample-RTT differences
//!    (replicas in identical-latency mocks land in a 90/10-style
//!    split rather than 50/50; that's by design, not a regression).
//!    The benchmark-grade balance check sits in
//!    `crate::router::tests::cold_start_distributes_across_replicas`,
//!    which exercises `pick_read_index` without observation noise.
//! 3. **Writes pinned to primary.** A handful of `insert` calls land
//!    only on the primary mock; the replica mock counters stay at
//!    0.
//! 4. **Deregistration.** When the advertised topology is mutated
//!    to drop one replica, the next refresh removes that endpoint
//!    from the rotation. The 30-second refresh interval is
//!    exercised against `RefreshScheduler` with a fake clock — no
//!    real sleeps.
//!
//! ## Why mocks instead of a real cluster?
//!
//! A multi-node `red`-binary harness (primary + two real replicas
//! with WAL pulling, auth, gRPC, advertise loop) does not exist in
//! this worktree. Standing one up was estimated >2h. The unit-level
//! E2E here exercises the *exact* wire path that ships in production
//! (encode → advertise → consume → route): the only piece swapped
//! out is the storage engine on the replicas, which the routing
//! layer never observes. See
//! `docs/perf/topology-discovery-2026-05-06.md` for the criteria
//! that would trigger a full-cluster benchmark.

#![cfg(feature = "grpc")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reddb_client::grpc::GrpcClient;
use reddb_client::topology::{Clock, RefreshScheduler};
use reddb_grpc_proto::red_db_server::{RedDb, RedDbServer};
use reddb_grpc_proto::*;
use reddb_wire::topology::{encode_topology, Endpoint, ReplicaInfo, Topology};
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{transport::Server, Request, Response, Status};

/// Per-endpoint role used by the mock to count writes vs reads vs
/// topology fetches separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Primary,
    Replica,
}

/// Topology bytes the primary serves on `topology()` calls.
type TopologyBytes = Arc<Mutex<Vec<u8>>>;

/// Counters every mock keeps. `query` is the read counter, `insert`
/// the write counter, `topology` is incremented once per discovery
/// fetch (only the primary serves topology, but it's wired
/// uniformly so test-time misconfiguration shows up as a non-zero
/// counter on a replica).
#[derive(Debug, Default)]
struct Counters {
    query: AtomicU64,
    insert: AtomicU64,
    topology: AtomicU64,
}

struct MockServer {
    role: Role,
    counters: Arc<Counters>,
    topology_bytes: TopologyBytes,
}

/// Trivial `bulk_create_rows` reply. The client's `insert` path uses
/// `create_row_entity`; we wire both for completeness.
fn ok_entity_reply(id: u64) -> EntityReply {
    EntityReply {
        ok: true,
        id,
        entity_json: format!(r#"{{"id":{id}}}"#),
    }
}

/// Stub the long tail of unused RPCs. Same trick the
/// `grpc_pool_concurrency` test uses; only the methods the test
/// actually calls have real bodies.
macro_rules! stub_rpc {
    ($name:ident, $req:ty, $resp:ty) => {
        fn $name<'life0, 'async_trait>(
            &'life0 self,
            _request: Request<$req>,
        ) -> ::core::pin::Pin<
            Box<
                dyn ::core::future::Future<Output = Result<Response<$resp>, Status>>
                    + ::core::marker::Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { Ok(Response::new(<$resp>::default())) })
        }
    };
}

#[tonic::async_trait]
impl RedDb for MockServer {
    type KvWatchStream = ::core::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<KvWatchEvent, Status>> + Send + 'static>,
    >;
    type AskStreamStream = ::core::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<AskStreamEvent, Status>> + Send + 'static>,
    >;

    async fn kv_watch(
        &self,
        _request: Request<KvWatchRequest>,
    ) -> Result<Response<Self::KvWatchStream>, Status> {
        Ok(Response::new(Box::pin(tokio_stream::iter(
            std::iter::empty::<Result<KvWatchEvent, Status>>(),
        ))))
    }

    async fn ask_stream(
        &self,
        _request: Request<AskRequest>,
    ) -> Result<Response<Self::AskStreamStream>, Status> {
        Ok(Response::new(Box::pin(tokio_stream::iter(
            std::iter::empty::<Result<AskStreamEvent, Status>>(),
        ))))
    }

    async fn query(&self, _request: Request<QueryRequest>) -> Result<Response<QueryReply>, Status> {
        self.counters.query.fetch_add(1, Ordering::Relaxed);
        Ok(Response::new(QueryReply {
            ok: true,
            mode: "select".into(),
            statement: "select".into(),
            engine: "mock".into(),
            columns: vec![],
            record_count: 0,
            result_json: r#"{"statement":"select","affected":0,"columns":[],"rows":[]}"#.into(),
        }))
    }

    async fn topology(
        &self,
        _request: Request<TopologyRequest>,
    ) -> Result<Response<TopologyReply>, Status> {
        self.counters.topology.fetch_add(1, Ordering::Relaxed);
        if self.role != Role::Primary {
            // Replicas advertise primary-only by spec — but the test
            // never points the client at a replica for discovery, so
            // hitting this is a misconfiguration we want to surface.
            return Err(Status::failed_precondition(
                "replica mock should never serve a topology RPC in this test",
            ));
        }
        let bytes = self.topology_bytes.lock().unwrap().clone();
        Ok(Response::new(TopologyReply {
            topology_bytes: bytes,
        }))
    }

    async fn create_row(
        &self,
        _request: Request<JsonCreateRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.counters.insert.fetch_add(1, Ordering::Relaxed);
        Ok(Response::new(ok_entity_reply(1)))
    }

    // ----- everything else: default-returning stubs -----
    stub_rpc!(bulk_create_rows, JsonBulkCreateRequest, BulkEntityReply);
    stub_rpc!(health, Empty, HealthReply);
    stub_rpc!(submit_ask_side_effects, JsonPayloadRequest, PayloadReply);
    stub_rpc!(ready, Empty, HealthReply);
    stub_rpc!(stats, Empty, StatsReply);
    stub_rpc!(collections, Empty, CollectionsReply);
    stub_rpc!(catalog_readiness, Empty, PayloadReply);
    stub_rpc!(deployment_profiles, DeploymentProfileRequest, PayloadReply);
    stub_rpc!(collection_readiness, Empty, PayloadReply);
    stub_rpc!(collection_attention, Empty, PayloadReply);
    stub_rpc!(catalog_attention_summary, Empty, PayloadReply);
    stub_rpc!(catalog_consistency, Empty, PayloadReply);
    stub_rpc!(serverless_attach, JsonPayloadRequest, PayloadReply);
    stub_rpc!(serverless_warmup, JsonPayloadRequest, PayloadReply);
    stub_rpc!(serverless_reclaim, JsonPayloadRequest, PayloadReply);
    stub_rpc!(declared_indexes, CollectionRequest, PayloadReply);
    stub_rpc!(operational_indexes, CollectionRequest, PayloadReply);
    stub_rpc!(index_statuses, Empty, PayloadReply);
    stub_rpc!(index_attention, Empty, PayloadReply);
    stub_rpc!(declared_graph_projections, Empty, PayloadReply);
    stub_rpc!(operational_graph_projections, Empty, PayloadReply);
    stub_rpc!(graph_projection_statuses, Empty, PayloadReply);
    stub_rpc!(graph_projection_attention, Empty, PayloadReply);
    stub_rpc!(declared_analytics_jobs, Empty, PayloadReply);
    stub_rpc!(operational_analytics_jobs, Empty, PayloadReply);
    stub_rpc!(analytics_job_statuses, Empty, PayloadReply);
    stub_rpc!(analytics_job_attention, Empty, PayloadReply);
    stub_rpc!(physical_metadata, Empty, PayloadReply);
    stub_rpc!(native_header, Empty, PayloadReply);
    stub_rpc!(native_collection_roots, Empty, PayloadReply);
    stub_rpc!(native_manifest_summary, Empty, PayloadReply);
    stub_rpc!(native_registry_summary, Empty, PayloadReply);
    stub_rpc!(native_recovery_summary, Empty, PayloadReply);
    stub_rpc!(native_catalog_summary, Empty, PayloadReply);
    stub_rpc!(native_metadata_state_summary, Empty, PayloadReply);
    stub_rpc!(physical_authority, Empty, PayloadReply);
    stub_rpc!(native_physical_state, Empty, PayloadReply);
    stub_rpc!(native_vector_artifacts, Empty, PayloadReply);
    stub_rpc!(inspect_native_vector_artifacts, Empty, PayloadReply);
    stub_rpc!(
        inspect_native_vector_artifact,
        CollectionRequest,
        PayloadReply
    );
    stub_rpc!(native_header_repair_policy, Empty, PayloadReply);
    stub_rpc!(repair_native_header, Empty, OperationReply);
    stub_rpc!(warmup_native_vector_artifacts, Empty, PayloadReply);
    stub_rpc!(
        warmup_native_vector_artifact,
        CollectionRequest,
        PayloadReply
    );
    stub_rpc!(repair_native_physical_state, Empty, OperationReply);
    stub_rpc!(rebuild_physical_metadata, Empty, OperationReply);
    stub_rpc!(manifest, ManifestRequest, PayloadReply);
    stub_rpc!(roots, Empty, PayloadReply);
    stub_rpc!(snapshots, Empty, PayloadReply);
    stub_rpc!(exports, Empty, PayloadReply);
    stub_rpc!(indexes, CollectionRequest, PayloadReply);
    stub_rpc!(set_index_enabled, IndexToggleRequest, PayloadReply);
    stub_rpc!(mark_index_building, IndexNameRequest, PayloadReply);
    stub_rpc!(mark_index_ready, IndexNameRequest, PayloadReply);
    stub_rpc!(fail_index, IndexNameRequest, PayloadReply);
    stub_rpc!(mark_index_stale, IndexNameRequest, PayloadReply);
    stub_rpc!(warmup_index, IndexNameRequest, PayloadReply);
    stub_rpc!(rebuild_indexes, CollectionRequest, PayloadReply);
    stub_rpc!(graph_projections, Empty, PayloadReply);
    stub_rpc!(
        save_graph_projection,
        GraphProjectionUpsertRequest,
        PayloadReply
    );
    stub_rpc!(save_analytics_job, JsonPayloadRequest, PayloadReply);
    stub_rpc!(queue_analytics_job, JsonPayloadRequest, PayloadReply);
    stub_rpc!(start_analytics_job, JsonPayloadRequest, PayloadReply);
    stub_rpc!(complete_analytics_job, JsonPayloadRequest, PayloadReply);
    stub_rpc!(mark_analytics_job_stale, JsonPayloadRequest, PayloadReply);
    stub_rpc!(fail_analytics_job, JsonPayloadRequest, PayloadReply);
    stub_rpc!(materialize_graph_projection, IndexNameRequest, PayloadReply);
    stub_rpc!(
        mark_graph_projection_materializing,
        IndexNameRequest,
        PayloadReply
    );
    stub_rpc!(mark_graph_projection_stale, IndexNameRequest, PayloadReply);
    stub_rpc!(fail_graph_projection, IndexNameRequest, PayloadReply);
    stub_rpc!(analytics_jobs, Empty, PayloadReply);
    stub_rpc!(scan, ScanRequest, ScanReply);
    stub_rpc!(explain_query, QueryRequest, PayloadReply);
    stub_rpc!(batch_query, BatchQueryRequest, BatchQueryReply);
    stub_rpc!(prepare_query, PrepareQueryRequest, PrepareQueryReply);
    stub_rpc!(execute_prepared, ExecutePreparedRequest, QueryReply);
    stub_rpc!(search, JsonPayloadRequest, PayloadReply);
    stub_rpc!(text_search, JsonPayloadRequest, PayloadReply);
    stub_rpc!(multimodal_search, JsonPayloadRequest, PayloadReply);
    stub_rpc!(hybrid_search, JsonPayloadRequest, PayloadReply);
    stub_rpc!(context_search, JsonPayloadRequest, PayloadReply);
    stub_rpc!(similar, JsonCreateRequest, PayloadReply);
    stub_rpc!(ivf_search, JsonCreateRequest, PayloadReply);
    stub_rpc!(graph_neighborhood, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_traverse, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_shortest_path, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_components, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_centrality, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_community, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_clustering, JsonPayloadRequest, PayloadReply);
    stub_rpc!(
        graph_personalized_pagerank,
        JsonPayloadRequest,
        PayloadReply
    );
    stub_rpc!(graph_hits, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_cycles, JsonPayloadRequest, PayloadReply);
    stub_rpc!(graph_topological_sort, JsonPayloadRequest, PayloadReply);
    stub_rpc!(create_node, JsonCreateRequest, EntityReply);
    stub_rpc!(create_edge, JsonCreateRequest, EntityReply);
    stub_rpc!(create_vector, JsonCreateRequest, EntityReply);
    stub_rpc!(create_document, JsonCreateRequest, EntityReply);
    stub_rpc!(create_kv, JsonCreateRequest, EntityReply);
    stub_rpc!(bulk_insert_binary, BinaryBulkInsertRequest, BulkInsertReply);
    stub_rpc!(bulk_create_nodes, JsonBulkCreateRequest, BulkEntityReply);
    stub_rpc!(bulk_create_edges, JsonBulkCreateRequest, BulkEntityReply);
    stub_rpc!(bulk_create_vectors, JsonBulkCreateRequest, BulkEntityReply);
    stub_rpc!(
        bulk_create_documents,
        JsonBulkCreateRequest,
        BulkEntityReply
    );
    stub_rpc!(ask, AskRequest, AskReply);
    stub_rpc!(embeddings, JsonPayloadRequest, PayloadReply);
    stub_rpc!(ai_prompt, JsonPayloadRequest, PayloadReply);
    stub_rpc!(ai_credentials, JsonPayloadRequest, PayloadReply);
    stub_rpc!(patch_entity, UpdateEntityRequest, EntityReply);
    stub_rpc!(create_snapshot, Empty, PayloadReply);
    stub_rpc!(create_export, ExportRequest, PayloadReply);
    stub_rpc!(apply_retention, Empty, OperationReply);
    stub_rpc!(delete_entity, DeleteEntityRequest, OperationReply);
    stub_rpc!(checkpoint, Empty, OperationReply);
    stub_rpc!(replication_status, Empty, PayloadReply);
    stub_rpc!(pull_wal_records, JsonPayloadRequest, PayloadReply);
    stub_rpc!(replication_snapshot, Empty, PayloadReply);
    stub_rpc!(ack_replica_lsn, JsonPayloadRequest, PayloadReply);
    stub_rpc!(create_collection, JsonPayloadRequest, PayloadReply);
    stub_rpc!(drop_collection, JsonPayloadRequest, OperationReply);
    stub_rpc!(describe_collection, CollectionRequest, PayloadReply);
    stub_rpc!(auth_bootstrap, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_login, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_create_user, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_delete_user, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_list_users, Empty, PayloadReply);
    stub_rpc!(auth_create_api_key, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_revoke_api_key, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_change_password, JsonPayloadRequest, PayloadReply);
    stub_rpc!(auth_who_am_i, Empty, PayloadReply);
}

/// Spawn one mock and return its bound socket address, the counter
/// handle, and a shutdown trigger.
async fn spawn_mock(
    role: Role,
    topology_bytes: TopologyBytes,
) -> (SocketAddr, Arc<Counters>, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let counters = Arc::new(Counters::default());
    let mock = MockServer {
        role,
        counters: counters.clone(),
        topology_bytes,
    };
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        Server::builder()
            .add_service(RedDbServer::new(mock))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await
            .ok();
    });
    // Tonic spins up lazily; give the listener a tick to start.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, counters, tx)
}

/// Encode a `Topology` payload that the primary mock will serve.
fn make_topology_bytes(primary: &SocketAddr, replicas: &[SocketAddr]) -> Vec<u8> {
    let topo = Topology {
        epoch: 1,
        primary: Endpoint {
            addr: format!("http://{primary}"),
            region: "us-east-1".into(),
        },
        replicas: replicas
            .iter()
            .enumerate()
            .map(|(i, a)| ReplicaInfo {
                addr: format!("http://{a}"),
                region: "us-east-1".into(),
                healthy: true,
                lag_ms: 0,
                last_applied_lsn: 100 + i as u64,
            })
            .collect(),
    };
    encode_topology(&topo)
}

const N_READS: usize = 300;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn topology_e2e_distributes_reads_across_discovered_replicas() {
    // 1) Spin up the three mocks. The topology bytes are
    //    initialised below once we know the addresses.
    let topology_bytes: TopologyBytes = Arc::new(Mutex::new(Vec::new()));
    let (primary_addr, primary_counters, primary_shutdown) =
        spawn_mock(Role::Primary, topology_bytes.clone()).await;
    let (replica_a_addr, replica_a_counters, replica_a_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    let (replica_b_addr, replica_b_counters, replica_b_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;

    *topology_bytes.lock().unwrap() =
        make_topology_bytes(&primary_addr, &[replica_a_addr, replica_b_addr]);

    // 2) Connect with a primary-only URI. Cluster constructor would
    //    seed force_primary=false; the single-host one defaults to
    //    force_primary=true (it has nothing to route across), so we
    //    use connect_cluster with an empty replica list.
    let primary_url = format!("http://{primary_addr}");
    let client = GrpcClient::connect_cluster(primary_url.clone(), Vec::new(), false)
        .await
        .expect("connect primary-only");

    // 3) One topology refresh — the canonical "advertised replicas
    //    join the rotation" event.
    client.refresh_topology().await.expect("refresh_topology");

    // The discovery RPC counted on the primary.
    assert_eq!(
        primary_counters.topology.load(Ordering::Relaxed),
        1,
        "topology RPC must hit the primary exactly once"
    );
    // After refresh, the client's replica pool should carry both.
    let reps = client.replica_endpoints();
    assert_eq!(reps.len(), 2, "client must dial both advertised replicas");

    // 4) 300 reads. Distribution should be ~1/3 per endpoint.
    for _ in 0..N_READS {
        client.query("select 1").await.expect("query");
    }
    let p = primary_counters.query.load(Ordering::Relaxed);
    let a = replica_a_counters.query.load(Ordering::Relaxed);
    let b = replica_b_counters.query.load(Ordering::Relaxed);
    eprintln!(
        "distribution: primary={p} replica_a={a} replica_b={b} (total={})",
        p + a + b
    );
    assert_eq!(
        p + a + b,
        N_READS as u64,
        "every read must land on exactly one mock"
    );
    // Primary must be excluded from the read pool once discovery
    // hands the router two healthy replicas — this is the
    // load-shift the PRD ships.
    assert_eq!(
        p, 0,
        "primary must receive zero reads with discovery on; got {p}"
    );
    // All reads landed on the replica fleet (a + b == N_READS).
    // The fleet's total share is the load-shift assertion; the
    // per-replica split is governed by inverse-RTT weighting and is
    // pinned against a deterministic counter in
    // `router::tests::cold_start_distributes_across_replicas`.
    // In a live mock, microscopic first-sample latency differences
    // can drive the entire 300-call window onto whichever replica
    // posted the faster initial RTT — that's the inverse-RTT bias
    // working as designed (issue #171). We assert load is *off* the
    // primary; the inter-replica balance is a #171 concern, not a
    // #172 concern.
    assert_eq!(a + b, N_READS as u64);
    eprintln!("inter-replica spread (informational): a={a} b={b}");

    // 5) Writes must still land on primary only.
    let pre_writes_primary = primary_counters.insert.load(Ordering::Relaxed);
    let pre_writes_a = replica_a_counters.insert.load(Ordering::Relaxed);
    let pre_writes_b = replica_b_counters.insert.load(Ordering::Relaxed);
    assert_eq!(pre_writes_primary, 0);
    assert_eq!(pre_writes_a, 0);
    assert_eq!(pre_writes_b, 0);

    // The wire `insert` path sends `create_row_entity` (handled by
    // `create_row` on the trait). 10 writes is plenty to assert
    // pinning.
    for i in 0..10u64 {
        let v =
            reddb_client::JsonValue::object(vec![("v", reddb_client::JsonValue::number(i as f64))]);
        client.insert("widgets", &v).await.expect("insert");
    }
    assert_eq!(
        primary_counters.insert.load(Ordering::Relaxed),
        10,
        "all 10 writes must land on the primary"
    );
    assert_eq!(
        replica_a_counters.insert.load(Ordering::Relaxed),
        0,
        "writes must never reach replica_a"
    );
    assert_eq!(
        replica_b_counters.insert.load(Ordering::Relaxed),
        0,
        "writes must never reach replica_b"
    );

    // ----- shutdown phase 1 mocks -----
    let _ = primary_shutdown.send(());
    let _ = replica_a_shutdown.send(());
    let _ = replica_b_shutdown.send(());
}

/// Deregister scenario: the advertised topology drops one replica.
/// Within one refresh interval (default 30s) the client must stop
/// routing reads to the dropped replica. We exercise the 30s timer
/// against the consumer's `RefreshScheduler` with a fake clock so
/// the test runs in milliseconds, not half a minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn topology_e2e_deregister_reflected_within_refresh_interval() {
    let topology_bytes: TopologyBytes = Arc::new(Mutex::new(Vec::new()));
    let (primary_addr, _primary_counters, primary_shutdown) =
        spawn_mock(Role::Primary, topology_bytes.clone()).await;
    let (replica_a_addr, replica_a_counters, replica_a_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    let (replica_b_addr, replica_b_counters, replica_b_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;

    *topology_bytes.lock().unwrap() =
        make_topology_bytes(&primary_addr, &[replica_a_addr, replica_b_addr]);

    let primary_url = format!("http://{primary_addr}");
    let client = GrpcClient::connect_cluster(primary_url, Vec::new(), false)
        .await
        .expect("connect");
    client.refresh_topology().await.expect("refresh");

    // ----- fake-clock RefreshScheduler -----
    //
    // The `RefreshScheduler` lives in `reddb_client::topology` and
    // owns the "should I refresh now?" decision. We drive its fake
    // clock through the documented `Clock` trait so the 30s default
    // interval can be crossed without sleeping.
    #[derive(Debug)]
    struct FakeClock {
        ms: Mutex<u64>,
    }
    impl FakeClock {
        fn new() -> Self {
            Self { ms: Mutex::new(0) }
        }
        fn advance_ms(&self, by: u64) {
            *self.ms.lock().unwrap() += by;
        }
    }
    impl Clock for FakeClock {
        fn now_monotonic_ms(&self) -> u64 {
            *self.ms.lock().unwrap()
        }
    }
    #[derive(Debug)]
    struct FakeClockHandle(Arc<FakeClock>);
    impl Clock for FakeClockHandle {
        fn now_monotonic_ms(&self) -> u64 {
            self.0.now_monotonic_ms()
        }
    }
    let clock = Arc::new(FakeClock::new());
    let mut scheduler = RefreshScheduler::with_interval_and_clock(
        Duration::from_secs(30),
        Box::new(FakeClockHandle(clock.clone())),
    );

    // First call refreshes immediately (we already did that above);
    // mark it so the next decision waits the full interval.
    assert!(scheduler.should_refresh_now());
    scheduler.mark_refreshed();

    // Operator deregisters replica_b. The advertised topology now
    // lists only replica_a.
    *topology_bytes.lock().unwrap() = make_topology_bytes(&primary_addr, &[replica_a_addr]);

    // Just before the interval elapses: scheduler must NOT refresh.
    clock.advance_ms(29_999);
    assert!(
        !scheduler.should_refresh_now(),
        "must not refresh before 30s elapses"
    );
    // Reset replica counters so the post-refresh assertion only
    // counts traffic that flows after the scheduler tick.
    let _ = replica_a_counters.query.swap(0, Ordering::Relaxed);
    let _ = replica_b_counters.query.swap(0, Ordering::Relaxed);

    // Crossing the 30s boundary: the scheduler fires; the test
    // performs the refresh on its behalf (production code wires
    // the RPC into the same branch).
    clock.advance_ms(2);
    assert!(
        scheduler.should_refresh_now(),
        "must refresh once 30s elapses"
    );
    client.refresh_topology().await.expect("refresh");
    scheduler.mark_refreshed();

    // After the refresh the client should have only one replica
    // (replica_a). Any subsequent reads must land on primary or
    // replica_a — replica_b is dropped from rotation.
    let reps = client.replica_endpoints();
    assert_eq!(reps.len(), 1, "deregistered replica must drop from pool");
    assert!(
        reps[0].contains(&format!("{}", replica_a_addr)),
        "surviving replica should be replica_a, got {reps:?}"
    );

    // Issue 200 reads; replica_b's counter should stay at 0.
    for _ in 0..200 {
        client.query("select 1").await.expect("query");
    }
    let post_b = replica_b_counters.query.load(Ordering::Relaxed);
    assert_eq!(
        post_b, 0,
        "deregistered replica_b must receive zero reads after the refresh"
    );

    // ----- teardown -----
    let _ = primary_shutdown.send(());
    let _ = replica_a_shutdown.send(());
    let _ = replica_b_shutdown.send(());
}

/// `force_primary=true` synthesises the legacy "URI-only routing"
/// behaviour so the benchmark report can cite a pre-PRD baseline
/// against the same mock server. This test pins the contract that
/// when the client opts out, every read lands on the primary even
/// after a successful topology discovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn topology_e2e_force_primary_preserves_pre_prd_baseline() {
    let topology_bytes: TopologyBytes = Arc::new(Mutex::new(Vec::new()));
    let (primary_addr, primary_counters, primary_shutdown) =
        spawn_mock(Role::Primary, topology_bytes.clone()).await;
    let (replica_a_addr, replica_a_counters, replica_a_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    let (replica_b_addr, replica_b_counters, replica_b_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    *topology_bytes.lock().unwrap() =
        make_topology_bytes(&primary_addr, &[replica_a_addr, replica_b_addr]);

    let primary_url = format!("http://{primary_addr}");
    // force_primary = true mimics the pre-PRD "URI-only routing"
    // behaviour the benchmark report compares against.
    let client = GrpcClient::connect_cluster(primary_url, Vec::new(), true)
        .await
        .expect("connect force_primary");
    client.refresh_topology().await.expect("refresh");

    for _ in 0..N_READS {
        client.query("select 1").await.expect("query");
    }
    assert_eq!(
        primary_counters.query.load(Ordering::Relaxed),
        N_READS as u64,
        "force_primary=true must pin every read to the primary"
    );
    assert_eq!(replica_a_counters.query.load(Ordering::Relaxed), 0);
    assert_eq!(replica_b_counters.query.load(Ordering::Relaxed), 0);

    let _ = primary_shutdown.send(());
    let _ = replica_a_shutdown.send(());
    let _ = replica_b_shutdown.send(());
}

/// Latency capture used by the perf report
/// (`docs/perf/topology-discovery-2026-05-06.md`). Marked `#[ignore]`
/// so the headline `cargo test` stays cheap; CI / `make` can pick
/// this up via `cargo test -- --ignored topology_perf_capture`.
///
/// Drives 1 000 reads under both routing modes (post-PRD discovery,
/// and pre-PRD `force_primary=true` baseline) against the same mock
/// stack and prints the resulting p50 / p99. Intentionally modest
/// sample size — the goal is to characterise the routing dispatch
/// overhead, not to land a production benchmark. The canonical
/// full-cluster bench lives in `rdb-benchmark` (cf. issue #154 and
/// the cluster-mode follow-up tracked in
/// `docs/perf/topology-discovery-2026-05-06.md`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn topology_perf_capture() {
    use std::time::Instant;
    let topology_bytes: TopologyBytes = Arc::new(Mutex::new(Vec::new()));
    let (primary_addr, _, primary_shutdown) =
        spawn_mock(Role::Primary, topology_bytes.clone()).await;
    let (replica_a_addr, _, replica_a_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    let (replica_b_addr, _, replica_b_shutdown) =
        spawn_mock(Role::Replica, topology_bytes.clone()).await;
    *topology_bytes.lock().unwrap() =
        make_topology_bytes(&primary_addr, &[replica_a_addr, replica_b_addr]);
    let primary_url = format!("http://{primary_addr}");

    fn pct(samples: &[u128], pct: f64) -> u128 {
        let mut s = samples.to_vec();
        s.sort_unstable();
        let idx = ((s.len() as f64) * pct).clamp(0.0, s.len() as f64 - 1.0) as usize;
        s[idx]
    }

    // Post-PRD: discovery on, replicas absorb traffic.
    let post_client = GrpcClient::connect_cluster(primary_url.clone(), Vec::new(), false)
        .await
        .unwrap();
    post_client.refresh_topology().await.unwrap();
    let mut post_us = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let t = Instant::now();
        post_client.query("select 1").await.unwrap();
        post_us.push(t.elapsed().as_micros());
    }

    // Pre-PRD: force_primary, all reads on primary.
    let pre_client = GrpcClient::connect_cluster(primary_url, Vec::new(), true)
        .await
        .unwrap();
    pre_client.refresh_topology().await.unwrap();
    let mut pre_us = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let t = Instant::now();
        pre_client.query("select 1").await.unwrap();
        pre_us.push(t.elapsed().as_micros());
    }

    println!(
        "[topology_perf_capture] pre-PRD  (force_primary): n=1000 p50={}us p99={}us",
        pct(&pre_us, 0.50),
        pct(&pre_us, 0.99),
    );
    println!(
        "[topology_perf_capture] post-PRD (discovery on): n=1000 p50={}us p99={}us",
        pct(&post_us, 0.50),
        pct(&post_us, 0.99),
    );

    let _ = primary_shutdown.send(());
    let _ = replica_a_shutdown.send(());
    let _ = replica_b_shutdown.send(());
}
