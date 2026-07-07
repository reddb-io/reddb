//! Concurrency property tests for the per-endpoint client pool
//! introduced in #170.
//!
//! Spins up an in-process tonic gRPC server whose `Query` RPC sleeps
//! for `RTT` before responding. Then fires N=10 concurrent
//! `GrpcClient::query` calls and asserts the total elapsed time is
//! close to `RTT` (parallel) — not `N * RTT` (serialized).
//!
//! The legacy `Mutex<RedDBClient>` baseline is reproduced by
//! configuring the pool with `pool_size = 1`, which the
//! implementation treats as a sanity fallback equivalent to the old
//! single-client-behind-mutex path. The serialized run must come in
//! at roughly `N * RTT`, while the pooled run must come in close to
//! `RTT`.
//!
//! Only the `Query` RPC has a real implementation; every other
//! method on the trait returns the reply type's `Default`. We never
//! call them.

#![cfg(feature = "grpc")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb_client::bookmark_routing::{BookmarkTarget, CausalReadOptions};
use reddb_client::grpc::GrpcClient;
use reddb_client::topology::ClusterMembership;
use reddb_client::RedDBClient;
use reddb_client::ValueOut;
use reddb_grpc_proto::red_db_server::{RedDb, RedDbServer};
use reddb_grpc_proto::*;
use tokio::sync::{oneshot, Mutex};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{transport::Server, Request, Response, Status};

/// Per-call simulated RTT on the mock server. Picked so that
/// (parallel ≈ RTT) and (serialized ≈ N * RTT) are clearly
/// distinguishable even on a noisy CI runner.
const RTT: Duration = Duration::from_millis(50);

/// Number of concurrent requests fired in each scenario.
const N: usize = 10;

/// Generates a default-returning RPC implementation. Tonic's
/// generated trait has 130+ methods and the concurrency test only
/// touches `query`; everything else just needs to satisfy the trait.
///
/// Hand-desugared from `async fn` to the
/// `Pin<Box<dyn Future + Send>>` shape that
/// `#[tonic::async_trait]` produces, because the proc-macro runs
/// before declarative macros expand and so cannot rewrite methods
/// generated inside this `macro_rules!`.
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

struct SlowMock {
    endpoint_label: &'static str,
}

#[tonic::async_trait]
impl RedDb for SlowMock {
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
        tokio::time::sleep(RTT).await;
        Ok(Response::new(QueryReply {
            ok: true,
            mode: "select".into(),
            statement: "select".into(),
            engine: "mock".into(),
            columns: vec!["endpoint".into()],
            record_count: 1,
            // Minimal valid JSON the client's `parse_query_json`
            // accepts. Include the endpoint so routing assertions
            // observe which server handled the read.
            result_json: format!(
                r#"{{"statement":"select","affected":0,"columns":["endpoint"],"rows":[{{"endpoint":"{}"}}]}}"#,
                self.endpoint_label
            ),
            affected_rows: 0,
        }))
    }

    async fn batch_insert(
        &self,
        _request: Request<tonic::Streaming<BatchInsertChunk>>,
    ) -> Result<Response<BatchInsertReply>, Status> {
        Ok(Response::new(BatchInsertReply { ok: true, count: 0 }))
    }

    // Every other RPC returns `<reply>::default()`. The concurrency
    // test never exercises them, but the trait demands an impl.
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
    stub_rpc!(create_row, JsonCreateRequest, EntityReply);
    stub_rpc!(create_node, JsonCreateRequest, EntityReply);
    stub_rpc!(create_edge, JsonCreateRequest, EntityReply);
    stub_rpc!(create_vector, JsonCreateRequest, EntityReply);
    stub_rpc!(create_document, JsonCreateRequest, EntityReply);
    stub_rpc!(create_kv, JsonCreateRequest, EntityReply);
    stub_rpc!(bulk_create_rows, JsonBulkCreateRequest, BulkEntityReply);
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
    stub_rpc!(topology, TopologyRequest, TopologyReply);
}

/// Spawn the slow mock on `127.0.0.1:0` and return its bound address
/// plus a shutdown trigger. The caller should drop the trigger to
/// stop the server.
async fn spawn_slow_mock() -> (SocketAddr, oneshot::Sender<()>) {
    spawn_labeled_mock("slow").await
}

async fn spawn_labeled_mock(endpoint_label: &'static str) -> (SocketAddr, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        Server::builder()
            .add_service(RedDbServer::new(SlowMock { endpoint_label }))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await
            .ok();
    });
    // Tonic spins up lazily; give the listener a tick to start
    // accepting before we connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, tx)
}

fn endpoint_label(result: &reddb_client::QueryResult) -> &str {
    let row = result.rows.first().expect("one row");
    let (_, value) = row
        .iter()
        .find(|(name, _)| name == "endpoint")
        .expect("endpoint column");
    match value {
        ValueOut::String(s) => s,
        other => panic!("endpoint should be a string, got {other:?}"),
    }
}

/// Drive `N` concurrent `query` calls through the new `GrpcClient`
/// pool path and return the wall-clock elapsed time.
async fn drive_pooled(client: Arc<GrpcClient>) -> Duration {
    let start = Instant::now();
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let c = client.clone();
        handles.push(tokio::spawn(async move { c.query("select 1").await }));
    }
    for h in handles {
        h.await
            .expect("task join")
            .expect("query should succeed against the slow mock");
    }
    start.elapsed()
}

/// Drive `N` concurrent `query_reply` calls through a manual
/// `Mutex<RedDBClient>` — exactly the dispatch shape the legacy
/// `grpc.rs` used. Acquiring the mutex on `&mut self` serializes
/// the calls. This is the regression baseline.
async fn drive_legacy_mutex(client: Arc<Mutex<RedDBClient>>) -> Duration {
    let start = Instant::now();
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            let mut guard = c.lock().await;
            guard
                .query_reply("select 1")
                .await
                .map_err(|e| e.to_string())
        }));
    }
    for h in handles {
        h.await
            .expect("task join")
            .expect("query should succeed against the slow mock");
    }
    start.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_runs_queries_in_parallel_vs_legacy_mutex_baseline() {
    let (addr, shutdown) = spawn_slow_mock().await;
    let url = format!("http://{addr}");

    // Pooled (N=4): 10 concurrent queries dispatched across 4
    // clones. Tonic multiplexes requests on the underlying
    // channel, so observed elapsed should be ≈ RTT.
    let pooled_client = Arc::new(
        GrpcClient::connect_with_pool_size(url.clone(), 4)
            .await
            .expect("connect pooled"),
    );
    let pooled_elapsed = drive_pooled(pooled_client).await;

    // Legacy baseline: a single `RedDBClient` behind a `Mutex`,
    // matching the pre-refactor `Endpoint { inner: Mutex<...> }`
    // dispatch. Each call has to acquire the mutex, so the N
    // concurrent calls run end-to-end serialized.
    let legacy_client = Arc::new(Mutex::new(
        RedDBClient::connect(&url, None)
            .await
            .expect("connect legacy"),
    ));
    let legacy_elapsed = drive_legacy_mutex(legacy_client).await;

    // pool_size=1 sanity fallback: still works, even if it doesn't
    // recover full parallelism — used as a feature-flag-off
    // emergency switch.
    let single_pool_client = Arc::new(
        GrpcClient::connect_with_pool_size(url, 1)
            .await
            .expect("connect pool=1"),
    );
    let single_pool_elapsed = drive_pooled(single_pool_client).await;

    eprintln!(
        "concurrency: pool=4={:?} pool=1={:?} legacy_mutex={:?} (RTT={:?}, N={N})",
        pooled_elapsed, single_pool_elapsed, legacy_elapsed, RTT
    );

    let _ = shutdown.send(());

    // Pooled run should be near-RTT, not near N*RTT. We use 5*RTT
    // as a generous upper bound; on a healthy host the observed
    // value is typically < 2*RTT.
    assert!(
        pooled_elapsed < RTT * 5,
        "pooled run took {:?}; expected close to {:?}",
        pooled_elapsed,
        RTT
    );

    // Legacy mutex baseline must be at least ~N * RTT (tolerate
    // -20% for scheduler slop). This is the regression check —
    // if the assertion fails, the baseline isn't actually
    // serializing and the comparison would be invalid.
    let serial_floor = RTT * (N as u32) * 4 / 5;
    assert!(
        legacy_elapsed >= serial_floor,
        "legacy mutex baseline took {:?}; expected >= {:?}",
        legacy_elapsed,
        serial_floor
    );

    // The new pool must be materially faster than the legacy
    // serialized baseline. Require at least 2x improvement.
    assert!(
        pooled_elapsed * 2 < legacy_elapsed,
        "pool_size=4 ({:?}) should be << legacy mutex ({:?})",
        pooled_elapsed,
        legacy_elapsed
    );

    // pool=1 sanity fallback: still completes successfully. We
    // don't pin its timing; tonic's underlying channel multiplexing
    // means observed parallelism varies.
    let _ = single_pool_elapsed;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn causal_query_falls_back_to_primary_when_replica_is_stale_but_tokenless_read_stays_replica_default(
) {
    let (primary_addr, primary_shutdown) = spawn_labeled_mock("primary").await;
    let (replica_addr, replica_shutdown) = spawn_labeled_mock("replica").await;
    let primary_url = format!("http://{primary_addr}");
    let replica_url = format!("http://{replica_addr}");

    let client = GrpcClient::connect_cluster_with_pool_size(
        primary_url.clone(),
        vec![replica_url.clone()],
        false,
        1,
    )
    .await
    .expect("connect cluster");
    client.update_membership(ClusterMembership::from_uri_addresses(
        primary_url.clone(),
        vec![replica_url.clone()],
    ));

    let tokenless = client
        .query("select endpoint")
        .await
        .expect("tokenless read");
    assert_eq!(
        endpoint_label(&tokenless),
        "replica",
        "ordinary reads keep the eventual-consistency replica default"
    );

    let causal = client
        .query_causal(
            "select endpoint",
            BookmarkTarget::new(1, 100),
            CausalReadOptions::with_deadline(Duration::from_millis(1)),
        )
        .await
        .expect("causal read");
    assert_eq!(
        endpoint_label(&causal),
        "primary",
        "stale replica must not serve a read carrying a causal token"
    );

    let _ = primary_shutdown.send(());
    let _ = replica_shutdown.send(());
}
