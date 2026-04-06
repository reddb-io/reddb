use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBOptions, RedDBResult};
use crate::health::{HealthProvider, HealthState};
use crate::json::{from_str as json_from_str, to_string as json_to_string, Map, Value as JsonValue};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphCyclesResult, RuntimeGraphDirection, RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult,
    RuntimeGraphPathAlgorithm, RuntimeGraphPathResult, RuntimeGraphPattern, RuntimeGraphProjection,
    RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy,
    RuntimeIvfSearchResult, RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanPage,
};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef};
use crate::storage::unified::{Metadata, MetadataValue};
use crate::storage::{EntityData, EntityId, UnifiedEntity};
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

pub mod proto {
    tonic::include_proto!("reddb.v1");
}

use proto::red_db_server::{RedDb, RedDbServer};
use proto::{
    BulkEntityReply, CollectionRequest, CollectionsReply, DeleteEntityRequest, Empty, EntityReply,
    ExportRequest, GraphProjectionUpsertRequest, HealthReply, JsonBulkCreateRequest,
    JsonCreateRequest, JsonPayloadRequest, IndexNameRequest, IndexToggleRequest, ManifestRequest,
    OperationReply, PayloadReply, QueryReply, QueryRequest, ScanEntity, ScanReply, ScanRequest,
    StatsReply, UpdateEntityRequest,
};

#[derive(Debug, Clone)]
pub struct GrpcServerOptions {
    pub bind_addr: String,
    pub auth_token: Option<String>,
    pub write_token: Option<String>,
}

impl Default for GrpcServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_string(),
            auth_token: None,
            write_token: None,
        }
    }
}

#[derive(Clone)]
pub struct RedDBGrpcServer {
    runtime: RedDBRuntime,
    options: GrpcServerOptions,
}

impl RedDBGrpcServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self::with_options(runtime, GrpcServerOptions::default())
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        options: GrpcServerOptions,
    ) -> RedDBResult<Self> {
        let runtime = RedDBRuntime::with_options(db_options)?;
        Ok(Self::with_options(runtime, options))
    }

    pub fn with_options(runtime: RedDBRuntime, options: GrpcServerOptions) -> Self {
        Self { runtime, options }
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &GrpcServerOptions {
        &self.options
    }

    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.options.bind_addr.parse()?;
        tonic::transport::Server::builder()
            .add_service(RedDbServer::new(GrpcRuntime {
                runtime: self.runtime.clone(),
                auth_token: self.options.auth_token.clone(),
                write_token: self.options.write_token.clone(),
            }))
            .serve(addr)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_token: Option<String>,
    write_token: Option<String>,
}

#[tonic::async_trait]
impl RedDb for GrpcRuntime {
    async fn health(&self, _request: Request<Empty>) -> Result<Response<HealthReply>, Status> {
        Ok(Response::new(health_reply(self.runtime.health())))
    }

    async fn ready(&self, _request: Request<Empty>) -> Result<Response<HealthReply>, Status> {
        Ok(Response::new(health_reply(self.runtime.health())))
    }

    async fn stats(&self, _request: Request<Empty>) -> Result<Response<StatsReply>, Status> {
        self.authorize_read(_request.metadata())?;
        Ok(Response::new(stats_reply(self.runtime.stats())))
    }

    async fn collections(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<CollectionsReply>, Status> {
        self.authorize_read(_request.metadata())?;
        Ok(Response::new(CollectionsReply {
            collections: self.runtime.db().collections(),
        }))
    }

    async fn physical_metadata(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let Some(metadata) = self.runtime.db().physical_metadata() else {
            return Err(Status::not_found("physical metadata is not available"));
        };
        let payload = json_to_string(&metadata.to_json_value())
            .unwrap_or_else(|_| "{}".to_string());
        Ok(Response::new(PayloadReply { ok: true, payload }))
    }

    async fn native_header(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let header = self.runtime.native_header().map_err(to_status)?;
        Ok(Response::new(json_payload_reply(native_header_json(header))))
    }

    async fn native_header_repair_policy(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let policy = self.runtime.native_header_repair_policy().map_err(to_status)?;
        Ok(Response::new(json_payload_reply(repair_policy_json(&policy))))
    }

    async fn repair_native_header(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<OperationReply>, Status> {
        self.authorize_write(request.metadata())?;
        let policy = self
            .runtime
            .repair_native_header_from_metadata()
            .map_err(to_status)?;
        Ok(Response::new(OperationReply {
            ok: true,
            message: format!("native header repair policy applied: {policy}"),
        }))
    }

    async fn manifest(
        &self,
        request: Request<ManifestRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let request = request.into_inner();
        let events = self
            .runtime
            .manifest_events_filtered(
                none_if_empty(&request.collection),
                none_if_empty(&request.kind),
                request.since_snapshot,
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(manifest_events_json(&events))))
    }

    async fn roots(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let roots = self.runtime.collection_roots().map_err(to_status)?;
        Ok(Response::new(json_payload_reply(collection_roots_json(&roots))))
    }

    async fn snapshots(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let snapshots = self.runtime.snapshots().map_err(to_status)?;
        Ok(Response::new(PayloadReply {
            ok: true,
            payload: format!("{snapshots:?}"),
        }))
    }

    async fn exports(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let exports = self.runtime.exports().map_err(to_status)?;
        Ok(Response::new(PayloadReply {
            ok: true,
            payload: format!("{exports:?}"),
        }))
    }

    async fn indexes(
        &self,
        request: Request<CollectionRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let request = request.into_inner();
        let indexes = match none_if_empty(&request.collection) {
            Some(collection) => self.runtime.indexes_for_collection(collection),
            None => self.runtime.indexes(),
        };
        Ok(Response::new(json_payload_reply(indexes_json(&indexes))))
    }

    async fn set_index_enabled(
        &self,
        request: Request<IndexToggleRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        if request.name.trim().is_empty() {
            return Err(Status::invalid_argument("index name cannot be empty"));
        }
        let index = self
            .runtime
            .set_index_enabled(request.name.trim(), request.enabled)
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(index_json(&index))))
    }

    async fn warmup_index(
        &self,
        request: Request<IndexNameRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        if request.name.trim().is_empty() {
            return Err(Status::invalid_argument("index name cannot be empty"));
        }
        let index = self
            .runtime
            .warmup_index(request.name.trim())
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(index_json(&index))))
    }

    async fn rebuild_indexes(
        &self,
        request: Request<CollectionRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        let indexes = self
            .runtime
            .rebuild_indexes(none_if_empty(&request.collection))
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(indexes_json(&indexes))))
    }

    async fn graph_projections(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let projections = self.runtime.graph_projections().map_err(to_status)?;
        Ok(Response::new(json_payload_reply(graph_projections_json(
            &projections,
        ))))
    }

    async fn save_graph_projection(
        &self,
        request: Request<GraphProjectionUpsertRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        if request.name.trim().is_empty() {
            return Err(Status::invalid_argument("graph projection name cannot be empty"));
        }

        let projection = RuntimeGraphProjection {
            node_labels: vec_to_option(request.node_labels),
            node_types: vec_to_option(request.node_types),
            edge_labels: vec_to_option(request.edge_labels),
        };
        if projection.node_labels.is_none()
            && projection.node_types.is_none()
            && projection.edge_labels.is_none()
        {
            return Err(Status::invalid_argument(
                "graph projection requires at least one of node_labels, node_types or edge_labels",
            ));
        }

        let saved = self
            .runtime
            .save_graph_projection(
                request.name,
                projection,
                none_if_empty_owned(request.source),
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(graph_projection_json(&saved))))
    }

    async fn analytics_jobs(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let jobs = self.runtime.analytics_jobs().map_err(to_status)?;
        Ok(Response::new(json_payload_reply(analytics_jobs_json(&jobs))))
    }

    async fn scan(&self, request: Request<ScanRequest>) -> Result<Response<ScanReply>, Status> {
        self.authorize_read(request.metadata())?;
        let request = request.into_inner();
        let page = self
            .runtime
            .scan_collection(
                &request.collection,
                Some(crate::runtime::ScanCursor {
                    offset: request.offset as usize,
                }),
                request.limit.max(1) as usize,
            )
            .map_err(to_status)?;
        Ok(Response::new(scan_reply(page)))
    }

    async fn query(&self, request: Request<QueryRequest>) -> Result<Response<QueryReply>, Status> {
        self.authorize_read(request.metadata())?;
        let result = self
            .runtime
            .execute_query(&request.into_inner().query)
            .map_err(to_status)?;
        Ok(Response::new(query_reply(result)))
    }

    async fn text_search(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let Some(query) = payload.get("query").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("payload_json must contain a string field named 'query'"));
        };
        if query.trim().is_empty() {
            return Err(Status::invalid_argument("field 'query' cannot be empty"));
        }

        let result = self
            .runtime
            .search_text(
                query.to_string(),
                json_string_list_field(&payload, "collections"),
                json_string_list_field(&payload, "fields"),
                json_usize_field(&payload, "limit"),
                json_bool_field(&payload, "fuzzy").unwrap_or(false),
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(dsl_query_result_json(&result))))
    }

    async fn hybrid_search(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let result = self
            .runtime
            .search_hybrid(
                optional_json_vector_field(&payload, "vector")?,
                json_usize_field(&payload, "k"),
                json_string_list_field(&payload, "collections"),
                json_graph_pattern(&payload)?,
                json_filters(&payload)?,
                json_weights(&payload),
                json_f32_field(&payload, "min_score"),
                json_usize_field(&payload, "limit"),
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(dsl_query_result_json(&result))))
    }

    async fn similar(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let request = request.into_inner();
        if request.collection.trim().is_empty() {
            return Err(Status::invalid_argument("collection cannot be empty"));
        }
        let payload = parse_json_payload(&request.payload_json)?;
        let vector = json_vector_field(&payload, "vector")?;
        let k = json_usize_field(&payload, "k").unwrap_or(10).max(1);
        let min_score = json_f32_field(&payload, "min_score").unwrap_or(0.0);
        let result = self
            .runtime
            .search_similar(&request.collection, &vector, k, min_score)
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(similar_results_json(
            &request.collection,
            k,
            min_score,
            &result,
        ))))
    }

    async fn ivf_search(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let request = request.into_inner();
        if request.collection.trim().is_empty() {
            return Err(Status::invalid_argument("collection cannot be empty"));
        }
        let payload = parse_json_payload(&request.payload_json)?;
        let vector = json_vector_field(&payload, "vector")?;
        let k = json_usize_field(&payload, "k").unwrap_or(10).max(1);
        let n_lists = json_usize_field(&payload, "n_lists").unwrap_or(32).max(1);
        let n_probes = json_usize_field(&payload, "n_probes");
        let result = self
            .runtime
            .search_ivf(&request.collection, &vector, k, n_lists, n_probes)
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(runtime_ivf_json(&result))))
    }

    async fn graph_neighborhood(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let Some(node) = payload.get("node").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("payload_json must contain a string field named 'node'"));
        };
        let result = self
            .runtime
            .graph_neighborhood(
                node,
                parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
                    .unwrap_or(RuntimeGraphDirection::Both),
                json_usize_field(&payload, "max_depth").unwrap_or(1).max(1),
                json_string_list_field(&payload, "edge_labels"),
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(graph_neighborhood_json(&result))))
    }

    async fn graph_traverse(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let Some(source) = payload.get("source").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("payload_json must contain a string field named 'source'"));
        };
        let result = self
            .runtime
            .graph_traverse(
                source,
                parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
                    .unwrap_or(RuntimeGraphDirection::Outgoing),
                json_usize_field(&payload, "max_depth").unwrap_or(3).max(1),
                parse_graph_traversal_strategy(payload.get("strategy").and_then(JsonValue::as_str))
                    .unwrap_or(RuntimeGraphTraversalStrategy::Bfs),
                json_string_list_field(&payload, "edge_labels"),
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(graph_traversal_json(&result))))
    }

    async fn graph_shortest_path(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let Some(source) = payload.get("source").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("payload_json must contain a string field named 'source'"));
        };
        let Some(target) = payload.get("target").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("payload_json must contain a string field named 'target'"));
        };
        let result = self
            .runtime
            .graph_shortest_path(
                source,
                target,
                parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
                    .unwrap_or(RuntimeGraphDirection::Outgoing),
                parse_graph_path_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
                    .unwrap_or(RuntimeGraphPathAlgorithm::Dijkstra),
                json_string_list_field(&payload, "edge_labels"),
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        Ok(Response::new(json_payload_reply(graph_path_result_json(&result))))
    }

    async fn graph_components(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let mode = parse_graph_components_mode(payload.get("mode").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphComponentsMode::Connected);
        let min_size = json_usize_field(&payload, "min_size").unwrap_or(1).max(1);
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_components(mode, min_size, resolve_projection_payload(&self.runtime, &payload)?)
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.components",
            projection_name,
            analytics_metadata(vec![
                ("mode", graph_components_mode_to_str(mode).to_string()),
                ("min_size", min_size.to_string()),
            ]),
        );
        Ok(Response::new(json_payload_reply(graph_components_json(&result))))
    }

    async fn graph_centrality(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let algorithm = parse_graph_centrality_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphCentralityAlgorithm::PageRank);
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let normalize = json_bool_field(&payload, "normalize").unwrap_or(true);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let alpha = json_f32_field(&payload, "alpha").map(|value| value as f64);
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_centrality(
                algorithm,
                top_k,
                normalize,
                max_iterations,
                epsilon,
                alpha,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            format!("graph.centrality.{}", graph_centrality_algorithm_to_str(algorithm)),
            projection_name,
            analytics_metadata(vec![
                ("top_k", top_k.to_string()),
                ("normalize", if normalize { "true" } else { "false" }.to_string()),
            ]),
        );
        Ok(Response::new(json_payload_reply(graph_centrality_json(&result))))
    }

    async fn graph_community(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let algorithm = parse_graph_community_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphCommunityAlgorithm::Louvain);
        let min_size = json_usize_field(&payload, "min_size").unwrap_or(1).max(1);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let resolution = json_f32_field(&payload, "resolution").map(|value| value as f64);
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_communities(
                algorithm,
                min_size,
                max_iterations,
                resolution,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            format!("graph.community.{}", graph_community_algorithm_to_str(algorithm)),
            projection_name,
            analytics_metadata(vec![("min_size", min_size.to_string())]),
        );
        Ok(Response::new(json_payload_reply(graph_community_json(&result))))
    }

    async fn graph_clustering(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let include_triangles = json_bool_field(&payload, "include_triangles").unwrap_or(false);
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_clustering(
                top_k,
                include_triangles,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.clustering",
            projection_name,
            analytics_metadata(vec![
                ("top_k", top_k.to_string()),
                (
                    "include_triangles",
                    if include_triangles { "true" } else { "false" }.to_string(),
                ),
            ]),
        );
        Ok(Response::new(json_payload_reply(graph_clustering_json(&result))))
    }

    async fn graph_personalized_pagerank(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload(&request.into_inner().payload_json)?;
        let Some(seeds) = json_string_list_field(&payload, "seeds") else {
            return Err(Status::invalid_argument("field 'seeds' must be a non-empty array of strings"));
        };
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let alpha = json_f32_field(&payload, "alpha").map(|value| value as f64);
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_personalized_pagerank(
                seeds,
                top_k,
                alpha,
                epsilon,
                max_iterations,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.pagerank.personalized",
            projection_name,
            analytics_metadata(vec![("top_k", top_k.to_string())]),
        );
        Ok(Response::new(json_payload_reply(graph_centrality_json(&result))))
    }

    async fn graph_hits(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_hits(
                top_k,
                epsilon,
                max_iterations,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.hits",
            projection_name,
            analytics_metadata(vec![("top_k", top_k.to_string())]),
        );
        Ok(Response::new(json_payload_reply(graph_hits_json(&result))))
    }

    async fn graph_cycles(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let max_length = json_usize_field(&payload, "max_length").unwrap_or(10).max(2);
        let max_cycles = json_usize_field(&payload, "max_cycles").unwrap_or(100).max(1);
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_cycles(
                max_length,
                max_cycles,
                resolve_projection_payload(&self.runtime, &payload)?,
            )
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.cycles",
            projection_name,
            analytics_metadata(vec![
                ("max_length", max_length.to_string()),
                ("max_cycles", max_cycles.to_string()),
            ]),
        );
        Ok(Response::new(json_payload_reply(graph_cycles_json(&result))))
    }

    async fn graph_topological_sort(
        &self,
        request: Request<JsonPayloadRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_read(request.metadata())?;
        let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
        let projection_name = json_string_field(&payload, "projection_name");
        let result = self
            .runtime
            .graph_topological_sort(resolve_projection_payload(&self.runtime, &payload)?)
            .map_err(to_status)?;
        let _ = self.runtime.record_analytics_job(
            "graph.topological_sort",
            projection_name,
            BTreeMap::new(),
        );
        Ok(Response::new(json_payload_reply(graph_topological_sort_json(
            &result,
        ))))
    }

    async fn create_row(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(create_row_reply(&self.runtime, request)?))
    }

    async fn create_node(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(create_node_reply(&self.runtime, request)?))
    }

    async fn create_edge(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(create_edge_reply(&self.runtime, request)?))
    }

    async fn create_vector(
        &self,
        request: Request<JsonCreateRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(create_vector_reply(&self.runtime, request)?))
    }

    async fn bulk_create_rows(
        &self,
        request: Request<JsonBulkCreateRequest>,
    ) -> Result<Response<BulkEntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(bulk_create_reply(
            &self.runtime,
            request,
            create_row_reply,
        )?))
    }

    async fn bulk_create_nodes(
        &self,
        request: Request<JsonBulkCreateRequest>,
    ) -> Result<Response<BulkEntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(bulk_create_reply(
            &self.runtime,
            request,
            create_node_reply,
        )?))
    }

    async fn bulk_create_edges(
        &self,
        request: Request<JsonBulkCreateRequest>,
    ) -> Result<Response<BulkEntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(bulk_create_reply(
            &self.runtime,
            request,
            create_edge_reply,
        )?))
    }

    async fn bulk_create_vectors(
        &self,
        request: Request<JsonBulkCreateRequest>,
    ) -> Result<Response<BulkEntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(bulk_create_reply(
            &self.runtime,
            request,
            create_vector_reply,
        )?))
    }

    async fn patch_entity(
        &self,
        request: Request<UpdateEntityRequest>,
    ) -> Result<Response<EntityReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        Ok(Response::new(patch_entity_reply(&self.runtime, request)?))
    }

    async fn create_snapshot(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let snapshot = self.runtime.create_snapshot().map_err(to_status)?;
        Ok(Response::new(PayloadReply {
            ok: true,
            payload: format!("{snapshot:?}"),
        }))
    }

    async fn create_export(
        &self,
        request: Request<ExportRequest>,
    ) -> Result<Response<PayloadReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        if request.name.trim().is_empty() {
            return Err(Status::invalid_argument("export name cannot be empty"));
        }
        let export = self
            .runtime
            .create_export(request.name)
            .map_err(to_status)?;
        Ok(Response::new(PayloadReply {
            ok: true,
            payload: format!("{export:?}"),
        }))
    }

    async fn apply_retention(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<OperationReply>, Status> {
        self.authorize_write(request.metadata())?;
        self.runtime.apply_retention_policy().map_err(to_status)?;
        Ok(Response::new(OperationReply {
            ok: true,
            message: "retention policy applied".to_string(),
        }))
    }

    async fn delete_entity(
        &self,
        request: Request<DeleteEntityRequest>,
    ) -> Result<Response<OperationReply>, Status> {
        self.authorize_write(request.metadata())?;
        let request = request.into_inner();
        let deleted = self
            .runtime
            .db()
            .store()
            .delete(&request.collection, EntityId::new(request.id))
            .map_err(|err| Status::internal(err.to_string()))?;
        if !deleted {
            return Err(Status::not_found(format!("entity not found: {}", request.id)));
        }
        Ok(Response::new(OperationReply {
            ok: true,
            message: format!("entity {} deleted", request.id),
        }))
    }

    async fn checkpoint(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<OperationReply>, Status> {
        self.authorize_write(_request.metadata())?;
        self.runtime.checkpoint().map_err(to_status)?;
        Ok(Response::new(OperationReply {
            ok: true,
            message: "checkpoint completed".to_string(),
        }))
    }
}

impl GrpcRuntime {
    fn authorize_read(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, false)
    }

    fn authorize_write(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, true)
    }

    fn authorize(&self, metadata: &MetadataMap, is_write: bool) -> Result<(), Status> {
        let token = grpc_token(metadata);

        if is_write {
            if let Some(expected) = self.write_token.as_deref() {
                if token != Some(expected) {
                    return Err(Status::unauthenticated("unauthorized"));
                }
                return Ok(());
            }
        }

        match self.auth_token.as_deref() {
            Some(expected) if token == Some(expected) => Ok(()),
            Some(_) => Err(Status::unauthenticated("unauthorized")),
            None if !is_write => Ok(()),
            None if self.write_token.is_none() => Ok(()),
            None => Err(Status::unauthenticated("unauthorized")),
        }
    }
}

fn to_status(err: crate::api::RedDBError) -> Status {
    Status::internal(err.to_string())
}

fn grpc_token<'a>(metadata: &'a MetadataMap) -> Option<&'a str> {
    if let Some(value) = metadata.get("authorization") {
        let value = value.to_str().ok()?;
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token);
        }
    }

    metadata.get("x-reddb-token")?.to_str().ok()
}

fn none_if_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn none_if_empty_owned(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn vec_to_option(values: Vec<String>) -> Option<Vec<String>> {
    let values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    (!values.is_empty()).then_some(values)
}

fn json_payload_reply(value: JsonValue) -> PayloadReply {
    PayloadReply {
        ok: true,
        payload: json_to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn parse_json_payload_allow_empty(payload_json: &str) -> Result<JsonValue, Status> {
    if payload_json.trim().is_empty() {
        return Ok(JsonValue::Object(Map::new()));
    }
    parse_json_payload(payload_json)
}

fn json_string_field(payload: &JsonValue, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(JsonValue::as_str)
        .map(|value| value.to_string())
}

fn json_string_list_field(payload: &JsonValue, field: &str) -> Option<Vec<String>> {
    let values = payload.get(field)?.as_array()?;
    let out: Vec<String> = values
        .iter()
        .filter_map(JsonValue::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    (!out.is_empty()).then_some(out)
}

fn json_bool_field(payload: &JsonValue, field: &str) -> Option<bool> {
    payload.get(field).and_then(JsonValue::as_bool)
}

fn json_usize_field(payload: &JsonValue, field: &str) -> Option<usize> {
    payload
        .get(field)
        .and_then(JsonValue::as_i64)
        .and_then(|value| usize::try_from(value).ok())
}

fn json_f32_field(payload: &JsonValue, field: &str) -> Option<f32> {
    payload.get(field).and_then(JsonValue::as_f64).map(|value| value as f32)
}

fn json_vector_field(payload: &JsonValue, field: &str) -> Result<Vec<f32>, Status> {
    let values = payload
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            Status::invalid_argument(format!(
                "payload_json must contain an array field named '{field}'"
            ))
        })?;
    if values.is_empty() {
        return Err(Status::invalid_argument(format!("field '{field}' cannot be empty")));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .ok_or_else(|| Status::invalid_argument(format!("field '{field}' must contain only numbers")))
        })
        .collect()
}

fn optional_json_vector_field(payload: &JsonValue, field: &str) -> Result<Option<Vec<f32>>, Status> {
    if payload.get(field).is_none() {
        return Ok(None);
    }
    json_vector_field(payload, field).map(Some)
}

fn json_graph_pattern(payload: &JsonValue) -> Result<Option<RuntimeGraphPattern>, Status> {
    let Some(graph) = payload.get("graph").and_then(JsonValue::as_object) else {
        return Ok(None);
    };
    Ok(Some(RuntimeGraphPattern {
        node_label: graph
            .get("label")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        node_type: graph
            .get("node_type")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        edge_labels: graph
            .get("edge_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }))
}

fn json_weights(payload: &JsonValue) -> Option<RuntimeQueryWeights> {
    let weights = payload.get("weights")?.as_object()?;
    Some(RuntimeQueryWeights {
        vector: weights.get("vector").and_then(JsonValue::as_f64).unwrap_or(1.0) as f32,
        graph: weights.get("graph").and_then(JsonValue::as_f64).unwrap_or(1.0) as f32,
        filter: weights.get("filter").and_then(JsonValue::as_f64).unwrap_or(1.0) as f32,
    })
}

fn json_filters(payload: &JsonValue) -> Result<Vec<RuntimeFilter>, Status> {
    let Some(values) = payload.get("filters").and_then(JsonValue::as_array) else {
        return Ok(Vec::new());
    };
    let mut filters = Vec::with_capacity(values.len());
    for value in values {
        let Some(object) = value.as_object() else {
            return Err(Status::invalid_argument("filters must contain only objects"));
        };
        let Some(field) = object.get("field").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("filter requires string field 'field'"));
        };
        let Some(op) = object.get("op").and_then(JsonValue::as_str) else {
            return Err(Status::invalid_argument("filter requires string field 'op'"));
        };
        let parsed_value = object.get("value").map(json_filter_value).transpose()?;
        filters.push(RuntimeFilter {
            field: field.to_string(),
            op: op.to_string(),
            value: parsed_value,
        });
    }
    Ok(filters)
}

fn json_filter_value(value: &JsonValue) -> Result<RuntimeFilterValue, Status> {
    Ok(match value {
        JsonValue::Null => RuntimeFilterValue::Null,
        JsonValue::Bool(value) => RuntimeFilterValue::Bool(*value),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                RuntimeFilterValue::Int(*value as i64)
            } else {
                RuntimeFilterValue::Float(*value)
            }
        }
        JsonValue::String(value) => RuntimeFilterValue::String(value.clone()),
        JsonValue::Array(values) => RuntimeFilterValue::List(
            values
                .iter()
                .map(json_filter_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        JsonValue::Object(object) => {
            if let (Some(start), Some(end)) = (object.get("start"), object.get("end")) {
                RuntimeFilterValue::Range(
                    Box::new(json_filter_value(start)?),
                    Box::new(json_filter_value(end)?),
                )
            } else {
                return Err(Status::invalid_argument(
                    "filter object values must contain 'start' and 'end'",
                ));
            }
        }
    })
}

fn resolve_projection_payload(
    runtime: &RedDBRuntime,
    payload: &JsonValue,
) -> Result<Option<RuntimeGraphProjection>, Status> {
    let named = json_string_field(payload, "projection_name");
    let inline = inline_projection(payload);
    runtime
        .resolve_graph_projection(named.as_deref(), inline)
        .map_err(to_status)
}

fn inline_projection(payload: &JsonValue) -> Option<RuntimeGraphProjection> {
    let projection = RuntimeGraphProjection {
        node_labels: json_string_list_field(payload, "node_labels"),
        node_types: json_string_list_field(payload, "node_types"),
        edge_labels: json_string_list_field(payload, "edge_labels"),
    };
    if projection.node_labels.is_none()
        && projection.node_types.is_none()
        && projection.edge_labels.is_none()
    {
        None
    } else {
        Some(projection)
    }
}

fn parse_graph_direction(value: Option<&str>) -> Option<RuntimeGraphDirection> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "outgoing" | "out" => Some(RuntimeGraphDirection::Outgoing),
        "incoming" | "in" => Some(RuntimeGraphDirection::Incoming),
        "both" | "any" => Some(RuntimeGraphDirection::Both),
        _ => None,
    }
}

fn parse_graph_traversal_strategy(value: Option<&str>) -> Option<RuntimeGraphTraversalStrategy> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "bfs" => Some(RuntimeGraphTraversalStrategy::Bfs),
        "dfs" => Some(RuntimeGraphTraversalStrategy::Dfs),
        _ => None,
    }
}

fn parse_graph_path_algorithm(value: Option<&str>) -> Option<RuntimeGraphPathAlgorithm> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "bfs" => Some(RuntimeGraphPathAlgorithm::Bfs),
        "dijkstra" => Some(RuntimeGraphPathAlgorithm::Dijkstra),
        _ => None,
    }
}

fn parse_graph_components_mode(value: Option<&str>) -> Option<RuntimeGraphComponentsMode> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "connected" => Some(RuntimeGraphComponentsMode::Connected),
        "weak" => Some(RuntimeGraphComponentsMode::Weak),
        "strong" => Some(RuntimeGraphComponentsMode::Strong),
        _ => None,
    }
}

fn parse_graph_centrality_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCentralityAlgorithm> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "degree" => Some(RuntimeGraphCentralityAlgorithm::Degree),
        "closeness" => Some(RuntimeGraphCentralityAlgorithm::Closeness),
        "betweenness" => Some(RuntimeGraphCentralityAlgorithm::Betweenness),
        "eigenvector" => Some(RuntimeGraphCentralityAlgorithm::Eigenvector),
        "pagerank" => Some(RuntimeGraphCentralityAlgorithm::PageRank),
        _ => None,
    }
}

fn parse_graph_community_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCommunityAlgorithm> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "label_propagation" | "label-propagation" => Some(RuntimeGraphCommunityAlgorithm::LabelPropagation),
        "louvain" => Some(RuntimeGraphCommunityAlgorithm::Louvain),
        _ => None,
    }
}

fn graph_direction_to_str(value: RuntimeGraphDirection) -> &'static str {
    match value {
        RuntimeGraphDirection::Outgoing => "outgoing",
        RuntimeGraphDirection::Incoming => "incoming",
        RuntimeGraphDirection::Both => "both",
    }
}

fn graph_traversal_strategy_to_str(value: RuntimeGraphTraversalStrategy) -> &'static str {
    match value {
        RuntimeGraphTraversalStrategy::Bfs => "bfs",
        RuntimeGraphTraversalStrategy::Dfs => "dfs",
    }
}

fn graph_path_algorithm_to_str(value: RuntimeGraphPathAlgorithm) -> &'static str {
    match value {
        RuntimeGraphPathAlgorithm::Bfs => "bfs",
        RuntimeGraphPathAlgorithm::Dijkstra => "dijkstra",
    }
}

fn graph_components_mode_to_str(value: RuntimeGraphComponentsMode) -> &'static str {
    match value {
        RuntimeGraphComponentsMode::Connected => "connected",
        RuntimeGraphComponentsMode::Weak => "weak",
        RuntimeGraphComponentsMode::Strong => "strong",
    }
}

fn graph_centrality_algorithm_to_str(value: RuntimeGraphCentralityAlgorithm) -> &'static str {
    match value {
        RuntimeGraphCentralityAlgorithm::Degree => "degree",
        RuntimeGraphCentralityAlgorithm::Closeness => "closeness",
        RuntimeGraphCentralityAlgorithm::Betweenness => "betweenness",
        RuntimeGraphCentralityAlgorithm::Eigenvector => "eigenvector",
        RuntimeGraphCentralityAlgorithm::PageRank => "pagerank",
    }
}

fn graph_community_algorithm_to_str(value: RuntimeGraphCommunityAlgorithm) -> &'static str {
    match value {
        RuntimeGraphCommunityAlgorithm::LabelPropagation => "label_propagation",
        RuntimeGraphCommunityAlgorithm::Louvain => "louvain",
    }
}

fn analytics_metadata(entries: Vec<(&str, String)>) -> BTreeMap<String, String> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn health_reply(report: crate::health::HealthReport) -> HealthReply {
    HealthReply {
        healthy: report.is_healthy(),
        state: match report.state {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
        }
        .to_string(),
        checked_at_unix_ms: report.checked_at_unix_ms as u64,
    }
}

fn stats_reply(stats: RuntimeStats) -> StatsReply {
    StatsReply {
        collection_count: stats.store.collection_count as u64,
        total_entities: stats.store.total_entities as u64,
        total_memory_bytes: stats.store.total_memory_bytes as u64,
        cross_ref_count: stats.store.cross_ref_count as u64,
        active_connections: stats.active_connections as u64,
        idle_connections: stats.idle_connections as u64,
        total_checkouts: stats.total_checkouts,
        paged_mode: stats.paged_mode,
        started_at_unix_ms: stats.started_at_unix_ms as u64,
    }
}

fn similar_results_json(
    collection: &str,
    k: usize,
    min_score: f32,
    results: &[crate::storage::SimilarResult],
) -> JsonValue {
    let mut object = Map::new();
    object.insert("collection".to_string(), JsonValue::String(collection.to_string()));
    object.insert("k".to_string(), JsonValue::Number(k as f64));
    object.insert("min_score".to_string(), JsonValue::Number(min_score as f64));
    object.insert(
        "results".to_string(),
        JsonValue::Array(
            results
                .iter()
                .map(|result| {
                    let mut item = Map::new();
                    item.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(result.entity_id.raw() as f64),
                    );
                    item.insert("score".to_string(), JsonValue::Number(result.score as f64));
                    item.insert("entity".to_string(), entity_to_json(&result.entity));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn runtime_ivf_json(result: &RuntimeIvfSearchResult) -> JsonValue {
    let mut stats = Map::new();
    stats.insert(
        "total_vectors".to_string(),
        JsonValue::Number(result.stats.total_vectors as f64),
    );
    stats.insert("n_lists".to_string(), JsonValue::Number(result.stats.n_lists as f64));
    stats.insert(
        "non_empty_lists".to_string(),
        JsonValue::Number(result.stats.non_empty_lists as f64),
    );
    stats.insert(
        "avg_list_size".to_string(),
        JsonValue::Number(result.stats.avg_list_size),
    );
    stats.insert(
        "max_list_size".to_string(),
        JsonValue::Number(result.stats.max_list_size as f64),
    );
    stats.insert(
        "min_list_size".to_string(),
        JsonValue::Number(result.stats.min_list_size as f64),
    );
    stats.insert(
        "dimension".to_string(),
        JsonValue::Number(result.stats.dimension as f64),
    );
    stats.insert("trained".to_string(), JsonValue::Bool(result.stats.trained));

    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert("k".to_string(), JsonValue::Number(result.k as f64));
    object.insert("n_lists".to_string(), JsonValue::Number(result.n_lists as f64));
    object.insert("n_probes".to_string(), JsonValue::Number(result.n_probes as f64));
    object.insert("stats".to_string(), JsonValue::Object(stats));
    object.insert(
        "matches".to_string(),
        JsonValue::Array(
            result
                .matches
                .iter()
                .map(|item| {
                    let mut entry = Map::new();
                    entry.insert("entity_id".to_string(), JsonValue::Number(item.entity_id as f64));
                    entry.insert("distance".to_string(), JsonValue::Number(item.distance as f64));
                    entry.insert(
                        "entity".to_string(),
                        match &item.entity {
                            Some(entity) => entity_to_json(entity),
                            None => JsonValue::Null,
                        },
                    );
                    JsonValue::Object(entry)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn dsl_query_result_json(result: &crate::storage::unified::dsl::QueryResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "matches".to_string(),
        JsonValue::Array(result.matches.iter().map(scored_match_json).collect()),
    );
    object.insert("scanned".to_string(), JsonValue::Number(result.scanned as f64));
    object.insert(
        "execution_time_us".to_string(),
        JsonValue::Number(result.execution_time_us as f64),
    );
    object.insert(
        "explanation".to_string(),
        JsonValue::String(result.explanation.clone()),
    );
    JsonValue::Object(object)
}

fn graph_neighborhood_json(result: &RuntimeGraphNeighborhoodResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(result.source.clone()));
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert("max_depth".to_string(), JsonValue::Number(result.max_depth as f64));
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(result.nodes.iter().map(graph_visit_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_traversal_json(result: &RuntimeGraphTraversalResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(result.source.clone()));
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "strategy".to_string(),
        JsonValue::String(graph_traversal_strategy_to_str(result.strategy).to_string()),
    );
    object.insert("max_depth".to_string(), JsonValue::Number(result.max_depth as f64));
    object.insert(
        "visits".to_string(),
        JsonValue::Array(result.visits.iter().map(graph_visit_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_path_result_json(result: &RuntimeGraphPathResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(result.source.clone()));
    object.insert("target".to_string(), JsonValue::String(result.target.clone()));
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_path_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert(
        "nodes_visited".to_string(),
        JsonValue::Number(result.nodes_visited as f64),
    );
    object.insert(
        "path".to_string(),
        match &result.path {
            Some(path) => graph_path_json(path),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn graph_components_json(result: &RuntimeGraphComponentsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "mode".to_string(),
        JsonValue::String(graph_components_mode_to_str(result.mode).to_string()),
    );
    object.insert("count".to_string(), JsonValue::Number(result.count as f64));
    object.insert(
        "components".to_string(),
        JsonValue::Array(
            result
                .components
                .iter()
                .map(|component| {
                    let mut item = Map::new();
                    item.insert("id".to_string(), JsonValue::String(component.id.clone()));
                    item.insert("size".to_string(), JsonValue::Number(component.size as f64));
                    item.insert(
                        "nodes".to_string(),
                        JsonValue::Array(component.nodes.iter().cloned().map(JsonValue::String).collect()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_centrality_json(result: &RuntimeGraphCentralityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_centrality_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert(
        "normalized".to_string(),
        result.normalized.map(JsonValue::Bool).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "iterations".to_string(),
        result.iterations.map(|value| JsonValue::Number(value as f64)).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result.converged.map(JsonValue::Bool).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "scores".to_string(),
        JsonValue::Array(
            result
                .scores
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "degree_scores".to_string(),
        JsonValue::Array(
            result
                .degree_scores
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("in_degree".to_string(), JsonValue::Number(score.in_degree as f64));
                    item.insert("out_degree".to_string(), JsonValue::Number(score.out_degree as f64));
                    item.insert("total_degree".to_string(), JsonValue::Number(score.total_degree as f64));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_community_json(result: &RuntimeGraphCommunityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_community_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert("count".to_string(), JsonValue::Number(result.count as f64));
    object.insert(
        "iterations".to_string(),
        result.iterations.map(|value| JsonValue::Number(value as f64)).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result.converged.map(JsonValue::Bool).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "modularity".to_string(),
        result.modularity.map(JsonValue::Number).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "passes".to_string(),
        result.passes.map(|value| JsonValue::Number(value as f64)).unwrap_or(JsonValue::Null),
    );
    object.insert(
        "communities".to_string(),
        JsonValue::Array(
            result
                .communities
                .iter()
                .map(|community| {
                    let mut item = Map::new();
                    item.insert("id".to_string(), JsonValue::String(community.id.clone()));
                    item.insert("size".to_string(), JsonValue::Number(community.size as f64));
                    item.insert(
                        "nodes".to_string(),
                        JsonValue::Array(community.nodes.iter().cloned().map(JsonValue::String).collect()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_clustering_json(result: &RuntimeGraphClusteringResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("global".to_string(), JsonValue::Number(result.global));
    object.insert(
        "local".to_string(),
        JsonValue::Array(
            result
                .local
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "triangle_count".to_string(),
        result.triangle_count.map(|value| JsonValue::Number(value as f64)).unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn graph_hits_json(result: &RuntimeGraphHitsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("iterations".to_string(), JsonValue::Number(result.iterations as f64));
    object.insert("converged".to_string(), JsonValue::Bool(result.converged));
    object.insert(
        "hubs".to_string(),
        JsonValue::Array(
            result
                .hubs
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "authorities".to_string(),
        JsonValue::Array(
            result
                .authorities
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_cycles_json(result: &RuntimeGraphCyclesResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("limit_reached".to_string(), JsonValue::Bool(result.limit_reached));
    object.insert(
        "cycles".to_string(),
        JsonValue::Array(result.cycles.iter().map(graph_path_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_topological_sort_json(result: &RuntimeGraphTopologicalSortResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("acyclic".to_string(), JsonValue::Bool(result.acyclic));
    object.insert(
        "ordered_nodes".to_string(),
        JsonValue::Array(result.ordered_nodes.iter().map(graph_node_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_visit_json(visit: &crate::runtime::RuntimeGraphVisit) -> JsonValue {
    let mut object = Map::new();
    object.insert("depth".to_string(), JsonValue::Number(visit.depth as f64));
    object.insert("node".to_string(), graph_node_json(&visit.node));
    JsonValue::Object(object)
}

fn graph_node_json(node: &crate::runtime::RuntimeGraphNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert("node_type".to_string(), JsonValue::String(node.node_type.clone()));
    object.insert("out_edge_count".to_string(), JsonValue::Number(node.out_edge_count as f64));
    object.insert("in_edge_count".to_string(), JsonValue::Number(node.in_edge_count as f64));
    JsonValue::Object(object)
}

fn graph_edge_json(edge: &crate::runtime::RuntimeGraphEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(edge.source.clone()));
    object.insert("target".to_string(), JsonValue::String(edge.target.clone()));
    object.insert("edge_type".to_string(), JsonValue::String(edge.edge_type.clone()));
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_path_json(path: &crate::runtime::RuntimeGraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert("hop_count".to_string(), JsonValue::Number(path.hop_count as f64));
    object.insert("total_weight".to_string(), JsonValue::Number(path.total_weight));
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().map(graph_node_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}

fn scored_match_json(item: &crate::storage::ScoredMatch) -> JsonValue {
    let mut object = Map::new();
    object.insert("entity".to_string(), entity_to_json(&item.entity));
    object.insert("score".to_string(), JsonValue::Number(item.score as f64));
    object.insert(
        "components".to_string(),
        match_components_json(&item.components),
    );
    object.insert(
        "path".to_string(),
        match &item.path {
            Some(path) => JsonValue::Array(
                path.iter()
                    .map(|id| JsonValue::Number(id.raw() as f64))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn match_components_json(components: &crate::storage::MatchComponents) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "vector_similarity".to_string(),
        match components.vector_similarity {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "graph_match".to_string(),
        match components.graph_match {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "filter_match".to_string(),
        JsonValue::Bool(components.filter_match),
    );
    object.insert(
        "hop_distance".to_string(),
        match components.hop_distance {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn manifest_events_json(events: &[crate::ManifestEvent]) -> JsonValue {
    JsonValue::Array(events.iter().map(manifest_event_json).collect())
}

fn collection_roots_json(roots: &std::collections::BTreeMap<String, u64>) -> JsonValue {
    let mut object = Map::new();
    for (collection, root) in roots {
        object.insert(collection.clone(), JsonValue::String(root.to_string()));
    }
    JsonValue::Object(object)
}

fn indexes_json(indexes: &[crate::PhysicalIndexState]) -> JsonValue {
    JsonValue::Array(indexes.iter().map(index_json).collect())
}

fn index_json(index: &crate::PhysicalIndexState) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(index.name.clone()));
    object.insert(
        "kind".to_string(),
        JsonValue::String(index.kind.as_str().to_string()),
    );
    object.insert(
        "collection".to_string(),
        match &index.collection {
            Some(collection) => JsonValue::String(collection.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert("enabled".to_string(), JsonValue::Bool(index.enabled));
    object.insert("entries".to_string(), JsonValue::Number(index.entries as f64));
    object.insert(
        "estimated_memory_bytes".to_string(),
        JsonValue::String(index.estimated_memory_bytes.to_string()),
    );
    object.insert(
        "last_refresh_ms".to_string(),
        match index.last_refresh_ms {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "backend".to_string(),
        JsonValue::String(index.backend.clone()),
    );
    JsonValue::Object(object)
}

fn graph_projections_json(projections: &[crate::PhysicalGraphProjection]) -> JsonValue {
    JsonValue::Array(projections.iter().map(graph_projection_json).collect())
}

fn graph_projection_json(projection: &crate::PhysicalGraphProjection) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(projection.name.clone()));
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(projection.created_at_unix_ms.to_string()),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::String(projection.updated_at_unix_ms.to_string()),
    );
    object.insert(
        "source".to_string(),
        JsonValue::String(projection.source.clone()),
    );
    object.insert(
        "node_labels".to_string(),
        JsonValue::Array(
            projection
                .node_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "node_types".to_string(),
        JsonValue::Array(
            projection
                .node_types
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "edge_labels".to_string(),
        JsonValue::Array(
            projection
                .edge_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        match projection.last_materialized_sequence {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn analytics_jobs_json(jobs: &[crate::PhysicalAnalyticsJob]) -> JsonValue {
    JsonValue::Array(jobs.iter().map(analytics_job_json).collect())
}

fn native_header_json(header: crate::storage::engine::PhysicalFileHeader) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(header.format_version as f64),
    );
    object.insert(
        "sequence".to_string(),
        JsonValue::String(header.sequence.to_string()),
    );
    object.insert(
        "manifest_oldest_root".to_string(),
        JsonValue::String(header.manifest_oldest_root.to_string()),
    );
    object.insert(
        "manifest_root".to_string(),
        JsonValue::String(header.manifest_root.to_string()),
    );
    object.insert(
        "free_set_root".to_string(),
        JsonValue::String(header.free_set_root.to_string()),
    );
    object.insert(
        "collection_roots_page".to_string(),
        JsonValue::Number(header.collection_roots_page as f64),
    );
    object.insert(
        "collection_roots_checksum".to_string(),
        JsonValue::String(header.collection_roots_checksum.to_string()),
    );
    object.insert(
        "collection_root_count".to_string(),
        JsonValue::Number(header.collection_root_count as f64),
    );
    object.insert(
        "snapshot_count".to_string(),
        JsonValue::Number(header.snapshot_count as f64),
    );
    object.insert(
        "index_count".to_string(),
        JsonValue::Number(header.index_count as f64),
    );
    object.insert(
        "catalog_collection_count".to_string(),
        JsonValue::Number(header.catalog_collection_count as f64),
    );
    object.insert(
        "catalog_total_entities".to_string(),
        JsonValue::String(header.catalog_total_entities.to_string()),
    );
    object.insert(
        "export_count".to_string(),
        JsonValue::Number(header.export_count as f64),
    );
    object.insert(
        "graph_projection_count".to_string(),
        JsonValue::Number(header.graph_projection_count as f64),
    );
    object.insert(
        "analytics_job_count".to_string(),
        JsonValue::Number(header.analytics_job_count as f64),
    );
    object.insert(
        "manifest_event_count".to_string(),
        JsonValue::Number(header.manifest_event_count as f64),
    );
    JsonValue::Object(object)
}

fn repair_policy_json(policy: &str) -> JsonValue {
    let mut object = Map::new();
    object.insert("policy".to_string(), JsonValue::String(policy.to_string()));
    JsonValue::Object(object)
}

fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(job.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
    object.insert("state".to_string(), JsonValue::String(job.state.clone()));
    object.insert(
        "projection".to_string(),
        match &job.projection {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(job.created_at_unix_ms.to_string()),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::String(job.updated_at_unix_ms.to_string()),
    );
    object.insert(
        "last_run_sequence".to_string(),
        match job.last_run_sequence {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        JsonValue::Object(
            job.metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn manifest_event_json(event: &crate::ManifestEvent) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(event.collection.clone()),
    );
    object.insert(
        "object_key".to_string(),
        JsonValue::String(event.object_key.clone()),
    );
    object.insert(
        "kind".to_string(),
        JsonValue::String(
            match event.kind {
                crate::ManifestEventKind::Insert => "insert",
                crate::ManifestEventKind::Update => "update",
                crate::ManifestEventKind::Remove => "remove",
                crate::ManifestEventKind::Checkpoint => "checkpoint",
            }
            .to_string(),
        ),
    );
    object.insert("block".to_string(), block_reference_json(&event.block));
    object.insert(
        "snapshot_min".to_string(),
        JsonValue::String(event.snapshot_min.to_string()),
    );
    object.insert(
        "snapshot_max".to_string(),
        match event.snapshot_max {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn block_reference_json(reference: &crate::BlockReference) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "index".to_string(),
        JsonValue::String(reference.index.to_string()),
    );
    object.insert(
        "checksum".to_string(),
        JsonValue::String(reference.checksum.to_string()),
    );
    JsonValue::Object(object)
}

fn create_row_reply(runtime: &RedDBRuntime, request: JsonCreateRequest) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let Some(fields) = payload.get("fields").and_then(JsonValue::as_object) else {
        return Err(Status::invalid_argument(
            "payload_json must contain an object field named 'fields'",
        ));
    };

    let mut owned_fields = Vec::new();
    for (key, value) in fields {
        owned_fields.push((key.clone(), json_to_storage_value(value)?));
    }
    let columns: Vec<(&str, Value)> = owned_fields
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();

    let db = runtime.db();
    let mut builder = db.row(&request.collection, columns);
    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        for (key, value) in metadata {
            builder = builder.metadata(key.clone(), json_to_metadata_value(value)?);
        }
    }
    let id = builder
        .save()
        .map_err(|err| Status::invalid_argument(err.to_string()))?;
    Ok(entity_reply(&db, id))
}

fn create_node_reply(runtime: &RedDBRuntime, request: JsonCreateRequest) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let Some(label) = payload.get("label").and_then(JsonValue::as_str) else {
        return Err(Status::invalid_argument(
            "payload_json must contain a string field named 'label'",
        ));
    };

    let db = runtime.db();
    let mut builder = db.node(&request.collection, label);
    if let Some(node_type) = payload.get("node_type").and_then(JsonValue::as_str) {
        builder = builder.node_type(node_type.to_string());
    }
    if let Some(properties) = payload.get("properties").and_then(JsonValue::as_object) {
        for (key, value) in properties {
            builder = builder.property(key.clone(), json_to_storage_value(value)?);
        }
    }
    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        for (key, value) in metadata {
            builder = builder.metadata(key.clone(), json_to_metadata_value(value)?);
        }
    }
    if let Some(links) = payload.get("links").and_then(JsonValue::as_object) {
        if let Some(tables) = links.get("tables").and_then(JsonValue::as_array) {
            for table in tables {
                let Some(object) = table.as_object() else {
                    return Err(Status::invalid_argument("table links must be objects"));
                };
                let Some(key) = object.get("key").and_then(JsonValue::as_str) else {
                    return Err(Status::invalid_argument("table links require 'key'"));
                };
                let Some(table_name) = object.get("table").and_then(JsonValue::as_str) else {
                    return Err(Status::invalid_argument("table links require 'table'"));
                };
                let Some(row_id) = object.get("row_id").and_then(JsonValue::as_i64) else {
                    return Err(Status::invalid_argument("table links require numeric 'row_id'"));
                };
                builder = builder.link_to_table(key.to_string(), TableRef::new(table_name, row_id as u64));
            }
        }
        if let Some(nodes) = links.get("nodes").and_then(JsonValue::as_array) {
            for node in nodes {
                let Some(object) = node.as_object() else {
                    return Err(Status::invalid_argument("node links must be objects"));
                };
                let Some(target) = object.get("id").and_then(JsonValue::as_i64) else {
                    return Err(Status::invalid_argument("node links require numeric 'id'"));
                };
                let edge_label = object
                    .get("edge_label")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("RELATED_TO");
                let weight = object.get("weight").and_then(JsonValue::as_f64).unwrap_or(1.0);
                builder = builder.link_to_weighted(
                    EntityId::new(target as u64),
                    edge_label.to_string(),
                    weight as f32,
                );
            }
        }
    }
    let id = builder
        .save()
        .map_err(|err| Status::invalid_argument(err.to_string()))?;
    Ok(entity_reply(&db, id))
}

fn create_edge_reply(runtime: &RedDBRuntime, request: JsonCreateRequest) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let Some(label) = payload.get("label").and_then(JsonValue::as_str) else {
        return Err(Status::invalid_argument(
            "payload_json must contain a string field named 'label'",
        ));
    };
    let Some(from) = payload.get("from").and_then(JsonValue::as_i64) else {
        return Err(Status::invalid_argument("payload_json must contain numeric field 'from'"));
    };
    let Some(to) = payload.get("to").and_then(JsonValue::as_i64) else {
        return Err(Status::invalid_argument("payload_json must contain numeric field 'to'"));
    };

    let db = runtime.db();
    let mut builder = db
        .edge(&request.collection, label)
        .from(EntityId::new(from as u64))
        .to(EntityId::new(to as u64));
    if let Some(weight) = payload.get("weight").and_then(JsonValue::as_f64) {
        builder = builder.weight(weight as f32);
    }
    if let Some(properties) = payload.get("properties").and_then(JsonValue::as_object) {
        for (key, value) in properties {
            builder = builder.property(key.clone(), json_to_storage_value(value)?);
        }
    }
    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        for (key, value) in metadata {
            builder = builder.metadata(key.clone(), json_to_metadata_value(value)?);
        }
    }
    let id = builder
        .save()
        .map_err(|err| Status::invalid_argument(err.to_string()))?;
    Ok(entity_reply(&db, id))
}

fn create_vector_reply(runtime: &RedDBRuntime, request: JsonCreateRequest) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let Some(JsonValue::Array(dense_values)) = payload.get("dense") else {
        return Err(Status::invalid_argument(
            "payload_json must contain an array field named 'dense'",
        ));
    };
    let dense = dense_values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .ok_or_else(|| Status::invalid_argument("field 'dense' must contain only numbers"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let db = runtime.db();
    let mut builder = db.vector(&request.collection).dense(dense);
    if let Some(content) = payload.get("content").and_then(JsonValue::as_str) {
        builder = builder.content(content.to_string());
    }
    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        for (key, value) in metadata {
            builder = builder.metadata(key.clone(), json_to_metadata_value(value)?);
        }
    }
    if let Some(link) = payload.get("link").and_then(JsonValue::as_object) {
        if let Some(row) = link.get("row") {
            let Some(object) = row.as_object() else {
                return Err(Status::invalid_argument("vector row link must be an object"));
            };
            let Some(table) = object.get("table").and_then(JsonValue::as_str) else {
                return Err(Status::invalid_argument("vector row link requires 'table'"));
            };
            let Some(row_id) = object.get("row_id").and_then(JsonValue::as_i64) else {
                return Err(Status::invalid_argument("vector row link requires numeric 'row_id'"));
            };
            builder = builder.link_to_table(TableRef::new(table, row_id as u64));
        }
        if let Some(node) = link.get("node") {
            let Some(object) = node.as_object() else {
                return Err(Status::invalid_argument("vector node link must be an object"));
            };
            let Some(collection) = object.get("collection").and_then(JsonValue::as_str) else {
                return Err(Status::invalid_argument("vector node link requires 'collection'"));
            };
            let Some(id) = object.get("id").and_then(JsonValue::as_i64) else {
                return Err(Status::invalid_argument("vector node link requires numeric 'id'"));
            };
            builder = builder.link_to_node(NodeRef::new(collection, EntityId::new(id as u64)));
        }
    }
    let id = builder
        .save()
        .map_err(|err| Status::invalid_argument(err.to_string()))?;
    Ok(entity_reply(&db, id))
}

fn bulk_create_reply(
    runtime: &RedDBRuntime,
    request: JsonBulkCreateRequest,
    handler: fn(&RedDBRuntime, JsonCreateRequest) -> Result<EntityReply, Status>,
) -> Result<BulkEntityReply, Status> {
    if request.payload_json.is_empty() {
        return Err(Status::invalid_argument("payload_json cannot be empty"));
    }

    let mut items = Vec::with_capacity(request.payload_json.len());
    for payload_json in request.payload_json {
        items.push(handler(
            runtime,
            JsonCreateRequest {
                collection: request.collection.clone(),
                payload_json,
            },
        )?);
    }

    Ok(BulkEntityReply {
        ok: true,
        count: items.len() as u64,
        items,
    })
}

fn patch_entity_reply(
    runtime: &RedDBRuntime,
    request: UpdateEntityRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let db = runtime.db();
    let store = db.store();
    let Some(manager) = store.get_collection(&request.collection) else {
        return Err(Status::not_found(format!(
            "collection not found: {}",
            request.collection
        )));
    };
    let entity_id = EntityId::new(request.id);
    let Some(mut entity) = manager.get(entity_id) else {
        return Err(Status::not_found(format!("entity not found: {}", request.id)));
    };

    if let Some(fields) = payload.get("fields").and_then(JsonValue::as_object) {
        match &mut entity.data {
            EntityData::Row(row) => {
                let named = row.named.get_or_insert_with(Default::default);
                for (key, value) in fields {
                    named.insert(key.clone(), json_to_storage_value(value)?);
                }
            }
            EntityData::Node(node) => {
                for (key, value) in fields {
                    node.properties.insert(key.clone(), json_to_storage_value(value)?);
                }
            }
            EntityData::Edge(edge) => {
                for (key, value) in fields {
                    edge.properties.insert(key.clone(), json_to_storage_value(value)?);
                }
            }
            EntityData::Vector(vector) => {
                if let Some(content) = fields.get("content").and_then(JsonValue::as_str) {
                    vector.content = Some(content.to_string());
                }
                if let Some(JsonValue::Array(values)) = fields.get("dense") {
                    vector.dense = values
                        .iter()
                        .map(|value| {
                            value.as_f64().map(|value| value as f32).ok_or_else(|| {
                                Status::invalid_argument(
                                    "field 'dense' must contain only numbers",
                                )
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                }
            }
        }
    }

    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        let mut merged = store
            .get_metadata(&request.collection, entity_id)
            .unwrap_or_else(Metadata::new);
        for (key, value) in metadata {
            merged.set(key.clone(), json_to_metadata_value(value)?);
        }
        store
            .set_metadata(&request.collection, entity_id, merged)
            .map_err(|err| Status::internal(err.to_string()))?;
    }

    if let Some(weight) = payload.get("weight").and_then(JsonValue::as_f64) {
        if let EntityData::Edge(edge) = &mut entity.data {
            edge.weight = weight as f32;
        }
    }

    entity.updated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    manager
        .update(entity)
        .map_err(|err| Status::internal(err.to_string()))?;

    Ok(entity_reply(&db, entity_id))
}

fn entity_reply(db: &crate::storage::RedDB, id: EntityId) -> EntityReply {
    EntityReply {
        ok: true,
        id: id.raw(),
        entity_json: db
            .get(id)
            .map(|entity| entity_json_string(&entity))
            .unwrap_or_else(|| "{}".to_string()),
    }
}

fn parse_json_payload(payload_json: &str) -> Result<JsonValue, Status> {
    json_from_str::<JsonValue>(payload_json)
        .map_err(|err| Status::invalid_argument(format!("invalid payload_json: {err}")))
}

fn json_to_storage_value(value: &JsonValue) -> Result<Value, Status> {
    match value {
        JsonValue::Null => Ok(Value::Null),
        JsonValue::Bool(value) => Ok(Value::Boolean(*value)),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Ok(Value::Integer(*value as i64))
            } else {
                Ok(Value::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(Value::Text(value.clone())),
        JsonValue::Array(_) | JsonValue::Object(_) => crate::json::to_vec(value)
            .map(Value::Json)
            .map_err(|err| Status::invalid_argument(format!("failed to encode JSON value: {err}"))),
    }
}

fn json_to_metadata_value(value: &JsonValue) -> Result<MetadataValue, Status> {
    match value {
        JsonValue::Null => Ok(MetadataValue::Null),
        JsonValue::Bool(value) => Ok(MetadataValue::Bool(*value)),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Ok(MetadataValue::Int(*value as i64))
            } else {
                Ok(MetadataValue::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(MetadataValue::String(value.clone())),
        JsonValue::Array(values) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                out.push(json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Array(out))
        }
        JsonValue::Object(map) => {
            let mut out = std::collections::HashMap::new();
            for (key, value) in map {
                out.insert(key.clone(), json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Object(out))
        }
    }
}

fn scan_reply(page: ScanPage) -> ScanReply {
    ScanReply {
        collection: page.collection,
        total: page.total as u64,
        next_offset: page.next.map(|cursor| cursor.offset as u64),
        items: page.items.into_iter().map(scan_entity).collect(),
    }
}

fn scan_entity(entity: UnifiedEntity) -> ScanEntity {
    ScanEntity {
        id: entity.id.raw(),
        kind: entity.kind.storage_type().to_string(),
        collection: entity.kind.collection().to_string(),
        json: entity_json_string(&entity),
    }
}

fn query_reply(result: RuntimeQueryResult) -> QueryReply {
    QueryReply {
        ok: true,
        mode: format!("{:?}", result.mode).to_lowercase(),
        statement: result.statement.to_string(),
        engine: result.engine.to_string(),
        columns: result.result.columns.clone(),
        record_count: result.result.records.len() as u64,
        result_json: unified_result_json_string(&result.result),
    }
}

fn unified_result_json_string(result: &crate::storage::query::unified::UnifiedResult) -> String {
    let mut object = Map::new();
    object.insert(
        "columns".to_string(),
        JsonValue::Array(
            result
                .columns
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "record_count".to_string(),
        JsonValue::Number(result.records.len() as f64),
    );
    object.insert(
        "records".to_string(),
        JsonValue::Array(
            result
                .records
                .iter()
                .map(|record| {
                    JsonValue::Object(
                        record
                            .values
                            .iter()
                            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    );
    json_to_string(&JsonValue::Object(object)).unwrap_or_else(|_| "{}".to_string())
}

fn entity_json_string(entity: &UnifiedEntity) -> String {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::Number(entity.id.raw() as f64));
    object.insert(
        "kind".to_string(),
        JsonValue::String(entity.kind.storage_type().to_string()),
    );
    object.insert(
        "collection".to_string(),
        JsonValue::String(entity.kind.collection().to_string()),
    );
    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = &row.named {
                object.insert(
                    "row".to_string(),
                    JsonValue::Object(
                        named
                            .iter()
                            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                            .collect(),
                    ),
                );
            }
        }
        EntityData::Node(node) => {
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    node.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                        .collect(),
                ),
            );
        }
        EntityData::Edge(edge) => {
            object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    edge.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                        .collect(),
                ),
            );
        }
        EntityData::Vector(vector) => {
            object.insert(
                "dense".to_string(),
                JsonValue::Array(
                    vector
                        .dense
                        .iter()
                        .map(|value| JsonValue::Number(*value as f64))
                        .collect(),
                ),
            );
            if let Some(content) = &vector.content {
                object.insert("content".to_string(), JsonValue::String(content.clone()));
            }
        }
    }
    json_to_string(&JsonValue::Object(object)).unwrap_or_else(|_| "{}".to_string())
}

fn entity_to_json(entity: &UnifiedEntity) -> JsonValue {
    json_from_str::<JsonValue>(&entity_json_string(entity)).unwrap_or(JsonValue::Null)
}

fn storage_value_to_json(value: &crate::storage::schema::Value) -> JsonValue {
    match value {
        crate::storage::schema::Value::Null => JsonValue::Null,
        crate::storage::schema::Value::Integer(value) => JsonValue::Number(*value as f64),
        crate::storage::schema::Value::UnsignedInteger(value) => JsonValue::Number(*value as f64),
        crate::storage::schema::Value::Float(value) => JsonValue::Number(*value),
        crate::storage::schema::Value::Text(value) => JsonValue::String(value.clone()),
        crate::storage::schema::Value::Blob(value) => JsonValue::String(hex::encode(value)),
        crate::storage::schema::Value::Boolean(value) => JsonValue::Bool(*value),
        crate::storage::schema::Value::Timestamp(value) => JsonValue::Number(*value as f64),
        crate::storage::schema::Value::Duration(value) => JsonValue::Number(*value as f64),
        crate::storage::schema::Value::IpAddr(value) => JsonValue::String(value.to_string()),
        crate::storage::schema::Value::MacAddr(value) => JsonValue::String(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            value[0], value[1], value[2], value[3], value[4], value[5]
        )),
        crate::storage::schema::Value::Vector(value) => JsonValue::Array(
            value
                .iter()
                .map(|entry| JsonValue::Number(*entry as f64))
                .collect(),
        ),
        crate::storage::schema::Value::Json(value) => {
            crate::json::from_slice::<JsonValue>(value).unwrap_or_else(|_| JsonValue::String(hex::encode(value)))
        }
        crate::storage::schema::Value::Uuid(value) => JsonValue::String(hex::encode(value)),
        crate::storage::schema::Value::NodeRef(value) => JsonValue::String(value.clone()),
        crate::storage::schema::Value::EdgeRef(value) => JsonValue::String(value.clone()),
        crate::storage::schema::Value::VectorRef(collection, id) => {
            let mut object = Map::new();
            object.insert("collection".to_string(), JsonValue::String(collection.clone()));
            object.insert("id".to_string(), JsonValue::Number(*id as f64));
            JsonValue::Object(object)
        }
        crate::storage::schema::Value::RowRef(table, row_id) => {
            let mut object = Map::new();
            object.insert("table".to_string(), JsonValue::String(table.clone()));
            object.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
            JsonValue::Object(object)
        }
    }
}
