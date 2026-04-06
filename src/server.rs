//! Minimal HTTP server for RedDB management and remote access.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::{RedDBOptions, RedDBResult};
use crate::catalog::{CatalogModelSnapshot, CollectionDescriptor, CollectionModel, SchemaMode};
use crate::health::{HealthProvider, HealthReport, HealthState};
use crate::json::{parse_json, to_vec as json_to_vec, Map, Value as JsonValue};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphHitsResult, RuntimeGraphPattern, RuntimeGraphProjection,
    RuntimeGraphTopologicalSortResult, RuntimeIvfSearchResult,
    RuntimeGraphDirection, RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm,
    RuntimeGraphPathResult, RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy,
    RuntimeGraphCyclesResult, RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats,
    ScanCursor, ScanPage,
};
use crate::storage::unified::dsl::{MatchComponents, QueryResult as DslQueryResult};
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::MetadataValue;
use crate::storage::query::modes::QueryMode;
use crate::storage::query::unified::{GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult, VectorSearchResult};
use crate::storage::schema::Value;
use crate::storage::{CrossRef, EntityData, EntityId, EntityKind, SimilarResult, UnifiedEntity};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub bind_addr: String,
    pub max_body_bytes: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub max_scan_limit: usize,
    pub auth_token: Option<String>,
    pub write_token: Option<String>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".to_string(),
            max_body_bytes: 1024 * 1024,
            read_timeout_ms: 5_000,
            write_timeout_ms: 5_000,
            max_scan_limit: 1_000,
            auth_token: None,
            write_token: None,
        }
    }
}

#[derive(Clone)]
pub struct RedDBServer {
    runtime: RedDBRuntime,
    options: ServerOptions,
}

impl RedDBServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self::with_options(runtime, ServerOptions::default())
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        server_options: ServerOptions,
    ) -> RedDBResult<Self> {
        let runtime = RedDBRuntime::with_options(db_options)?;
        Ok(Self::with_options(runtime, server_options))
    }

    pub fn with_options(runtime: RedDBRuntime, options: ServerOptions) -> Self {
        Self { runtime, options }
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    pub fn serve(&self) -> io::Result<()> {
        let listener = TcpListener::bind(&self.options.bind_addr)?;
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let _ = self.handle_connection(stream);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub fn serve_in_background(&self) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || server.serve())
    }

    fn handle_connection(&self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(self.options.read_timeout_ms)))?;
        stream.set_write_timeout(Some(Duration::from_millis(self.options.write_timeout_ms)))?;

        let request = HttpRequest::read_from(&mut stream, self.options.max_body_bytes)?;
        let response = self.route(request);
        stream.write_all(&response.to_http_bytes())?;
        stream.flush()?;
        Ok(())
    }

    fn route(&self, request: HttpRequest) -> HttpResponse {
        let HttpRequest {
            method,
            path,
            query,
            headers,
            body,
        } = request;

        if !self.is_authorized(&method, &path, &headers) {
            return json_error(401, "unauthorized");
        }

        match (method.as_str(), path.as_str()) {
            ("GET", "/health") => {
                let report = self.runtime.health();
                let status = if report.is_healthy() { 200 } else { 503 };
                json_response(status, health_to_json(&report))
            }
            ("GET", "/ready") => {
                let report = self.runtime.health();
                let status = if report.is_healthy() { 200 } else { 503 };
                json_response(status, health_to_json(&report))
            }
            ("GET", "/catalog") => json_response(200, catalog_to_json(&self.runtime.catalog())),
            ("GET", "/physical/metadata") => match self.runtime.db().physical_metadata() {
                Some(metadata) => json_response(200, metadata.to_json_value()),
                None => json_error(404, "physical metadata is not available"),
            },
            ("GET", "/physical/native-header") => match self.runtime.native_header() {
                Ok(header) => json_response(200, native_header_to_json(header)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/physical/native-header/repair-policy") => {
                match self.runtime.native_header_repair_policy() {
                    Ok(policy) => json_response(200, repair_policy_to_json(&policy)),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/manifest") => match self.runtime.manifest_events_filtered(
                query.get("collection").map(String::as_str),
                query.get("kind").map(String::as_str),
                query
                    .get("since_snapshot")
                    .and_then(|value| value.parse::<u64>().ok()),
            ) {
                Ok(events) => json_response(200, manifest_events_to_json(&events)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/graph/projections") => match self.runtime.graph_projections() {
                Ok(projections) => json_response(200, graph_projections_to_json(&projections)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/graph/jobs") => match self.runtime.analytics_jobs() {
                Ok(jobs) => json_response(200, analytics_jobs_to_json(&jobs)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/roots") => match self.runtime.collection_roots() {
                Ok(roots) => json_response(200, collection_roots_to_json(&roots)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/snapshots") => match self.runtime.snapshots() {
                Ok(snapshots) => json_response(200, snapshots_to_json(&snapshots)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/exports") => match self.runtime.exports() {
                Ok(exports) => json_response(200, exports_to_json(&exports)),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/indexes") => {
                json_response(200, indexes_to_json(&self.runtime.indexes()))
            }
            ("GET", "/stats") => json_response(200, runtime_stats_to_json(&self.runtime.stats())),
            ("GET", "/collections") => {
                let values = self
                    .runtime
                    .db()
                    .collections()
                    .into_iter()
                    .map(JsonValue::String)
                    .collect();
                let mut object = Map::new();
                object.insert("collections".to_string(), JsonValue::Array(values));
                json_response(200, JsonValue::Object(object))
            }
            ("POST", "/checkpoint") => match self.runtime.checkpoint() {
                Ok(()) => json_ok("checkpoint completed"),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/snapshot") => match self.runtime.create_snapshot() {
                Ok(snapshot) => json_response(200, snapshot_descriptor_to_json(&snapshot)),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/physical/native-header/repair") => {
                match self.runtime.repair_native_header_from_metadata() {
                    Ok(policy) => json_response(200, repair_policy_to_json(&policy)),
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/export") => self.handle_export(body),
            ("POST", "/indexes/rebuild") => self.handle_rebuild_indexes(body, None),
            ("POST", "/retention/apply") => match self.runtime.apply_retention_policy() {
                Ok(()) => json_ok("retention policy applied"),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/maintenance") => match self.runtime.run_maintenance() {
                Ok(()) => json_ok("maintenance completed"),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/query") => self.handle_query(body),
            ("POST", "/text/search") => self.handle_text_search(body),
            ("POST", "/hybrid/search") => self.handle_hybrid_search(body),
            ("POST", "/graph/neighborhood") => self.handle_graph_neighborhood(body),
            ("POST", "/graph/traverse") => self.handle_graph_traverse(body),
            ("POST", "/graph/shortest-path") => self.handle_graph_shortest_path(body),
            ("POST", "/graph/analytics/components") => self.handle_graph_components(body),
            ("POST", "/graph/analytics/centrality") => self.handle_graph_centrality(body),
            ("POST", "/graph/analytics/community") => self.handle_graph_community(body),
            ("POST", "/graph/analytics/clustering") => self.handle_graph_clustering(body),
            ("POST", "/graph/analytics/pagerank/personalized") => {
                self.handle_graph_personalized_pagerank(body)
            }
            ("POST", "/graph/analytics/hits") => self.handle_graph_hits(body),
            ("POST", "/graph/analytics/cycles") => self.handle_graph_cycles(body),
            ("POST", "/graph/analytics/topological-sort") => {
                self.handle_graph_topological_sort(body)
            }
            ("POST", "/graph/projections") => self.handle_graph_projection_upsert(body),
            _ => {
                if method == "GET" {
                    if let Some(collection) = collection_from_scan_path(&path) {
                        return self.handle_scan(collection, &query);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "indexes") {
                        return json_response(
                            200,
                            indexes_to_json(&self.runtime.indexes_for_collection(collection)),
                        );
                    }
                }
                if method == "POST" {
                    if let Some(collection) = collection_from_action_path(&path, "bulk/rows") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_row);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/nodes") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_node);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/edges") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_edge);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/vectors") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_vector);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "rows") {
                        return self.handle_create_row(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "nodes") {
                        return self.handle_create_node(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "edges") {
                        return self.handle_create_edge(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "vectors") {
                        return self.handle_create_vector(collection, body);
                    }
                    if let Some(name) = index_named_action_path(&path, "enable") {
                        return match self.runtime.set_index_enabled(&name, true) {
                            Ok(index) => json_response(200, physical_index_state_to_json(&index)),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "disable") {
                        return match self.runtime.set_index_enabled(&name, false) {
                            Ok(index) => json_response(200, physical_index_state_to_json(&index)),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "warmup") {
                        return match self.runtime.warmup_index(&name) {
                            Ok(index) => json_response(200, physical_index_state_to_json(&index)),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(collection) = collection_from_action_path(&path, "similar") {
                        return self.handle_similar(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "ivf/search") {
                        return self.handle_ivf_search(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "indexes/rebuild") {
                        return self.handle_rebuild_indexes(body, Some(collection));
                    }
                }
                if method == "PATCH" {
                    if let Some((collection, id)) = collection_entity_path(&path) {
                        return self.handle_patch_entity(collection, id, body);
                    }
                }
                if method == "DELETE" {
                    if let Some((collection, id)) = collection_entity_path(&path) {
                        return self.handle_delete_entity(collection, id);
                    }
                }
                json_error(404, format!("route not found: {} {}", method, path))
            }
        }
    }

    fn resolve_projection_payload(
        &self,
        payload: &JsonValue,
    ) -> Result<Option<RuntimeGraphProjection>, HttpResponse> {
        self.runtime
            .resolve_graph_projection(
                json_string_field(payload, "projection_name").as_deref(),
                json_graph_projection(payload),
            )
            .map_err(|err| json_error(400, err.to_string()))
    }

    fn is_authorized(
        &self,
        method: &str,
        path: &str,
        headers: &BTreeMap<String, String>,
    ) -> bool {
        if matches!((method, path), ("GET", "/health") | ("GET", "/ready")) {
            return true;
        }

        let token = authorization_bearer_token(headers);
        let is_write = !matches!(method, "GET" | "HEAD");

        if is_write {
            if let Some(expected) = self.options.write_token.as_deref() {
                return token == Some(expected);
            }
        }

        match self.options.auth_token.as_deref() {
            Some(expected) => token == Some(expected),
            None => !is_write || self.options.write_token.is_none() || token.is_some(),
        }
    }

    fn handle_scan(&self, collection: &str, query: &BTreeMap<String, String>) -> HttpResponse {
        let offset = query
            .get("offset")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(100)
            .max(1)
            .min(self.options.max_scan_limit);

        match self
            .runtime
            .scan_collection(collection, Some(ScanCursor { offset }), limit)
        {
            Ok(page) => json_response(200, scan_page_to_json(&page)),
            Err(err) => json_error(404, err.to_string()),
        }
    }

    fn handle_create_row(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(fields) = payload.get("fields").and_then(JsonValue::as_object) else {
            return json_error(400, "JSON body must contain an object field named 'fields'");
        };

        let mut owned_fields = Vec::new();
        for (key, value) in fields {
            let value = match json_to_storage_value(value) {
                Ok(value) => value,
                Err(response) => return response,
            };
            owned_fields.push((key.clone(), value));
        }

        let columns: Vec<(&str, Value)> = owned_fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();

        let db = self.runtime.db();
        let mut builder = db.row(collection, columns);

        if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
            for (key, value) in metadata {
                let value = match json_to_metadata_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.metadata(key.clone(), value);
            }
        }

        if let Some(links) = payload.get("links").and_then(JsonValue::as_object) {
            if let Some(nodes) = links.get("nodes").and_then(JsonValue::as_array) {
                for node in nodes {
                    let (target_collection, id) = match json_collection_entity_ref(node, "node") {
                        Ok(result) => result,
                        Err(response) => return response,
                    };
                    builder =
                        builder.link_to_node(NodeRef::new(target_collection, EntityId::new(id)));
                }
            }
            if let Some(vectors) = links.get("vectors").and_then(JsonValue::as_array) {
                for vector in vectors {
                    let (target_collection, id) =
                        match json_collection_entity_ref(vector, "vector") {
                            Ok(result) => result,
                            Err(response) => return response,
                        };
                    builder = builder
                        .link_to_vector(VectorRef::new(target_collection, EntityId::new(id)));
                }
            }
        }

        match builder.save() {
            Ok(id) => json_response(200, created_entity_to_json(&db, id)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_create_node(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(label) = payload.get("label").and_then(JsonValue::as_str) else {
            return json_error(400, "JSON body must contain a string field named 'label'");
        };

        let db = self.runtime.db();
        let mut builder = db.node(collection, label);

        if let Some(node_type) = payload.get("node_type").and_then(JsonValue::as_str) {
            builder = builder.node_type(node_type.to_string());
        }

        if let Some(properties) = payload.get("properties").and_then(JsonValue::as_object) {
            for (key, value) in properties {
                let value = match json_to_storage_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.property(key.clone(), value);
            }
        }

        if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
            for (key, value) in metadata {
                let value = match json_to_metadata_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.metadata(key.clone(), value);
            }
        }

        if let Some(embeddings) = payload.get("embeddings").and_then(JsonValue::as_array) {
            for embedding in embeddings {
                let Some(object) = embedding.as_object() else {
                    return json_error(400, "node embeddings must be objects");
                };
                let Some(name) = object.get("name").and_then(JsonValue::as_str) else {
                    return json_error(400, "node embedding objects require 'name'");
                };
                let vector = match json_vector_from_value(object.get("vector"), "vector") {
                    Ok(vector) => vector,
                    Err(response) => return response,
                };
                if let Some(model) = object.get("model").and_then(JsonValue::as_str) {
                    builder = builder.embedding_with_model(
                        name.to_string(),
                        vector,
                        model.to_string(),
                    );
                } else {
                    builder = builder.embedding(name.to_string(), vector);
                }
            }
        }

        if let Some(links) = payload.get("links").and_then(JsonValue::as_object) {
            if let Some(tables) = links.get("tables").and_then(JsonValue::as_array) {
                for table in tables {
                    let Some(object) = table.as_object() else {
                        return json_error(400, "table links must be objects");
                    };
                    let Some(key) = object.get("key").and_then(JsonValue::as_str) else {
                        return json_error(400, "table links require 'key'");
                    };
                    let Some(table_name) = object.get("table").and_then(JsonValue::as_str) else {
                        return json_error(400, "table links require 'table'");
                    };
                    let Some(row_id) = object.get("row_id").and_then(JsonValue::as_i64) else {
                        return json_error(400, "table links require numeric 'row_id'");
                    };
                    builder = builder
                        .link_to_table(key.to_string(), TableRef::new(table_name, row_id as u64));
                }
            }
            if let Some(nodes) = links.get("nodes").and_then(JsonValue::as_array) {
                for node in nodes {
                    let Some(object) = node.as_object() else {
                        return json_error(400, "node links must be objects");
                    };
                    let Some(target) = object.get("id").and_then(JsonValue::as_i64) else {
                        return json_error(400, "node links require numeric 'id'");
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

        match builder.save() {
            Ok(id) => json_response(200, created_entity_to_json(&db, id)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_create_edge(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(label) = payload.get("label").and_then(JsonValue::as_str) else {
            return json_error(400, "JSON body must contain a string field named 'label'");
        };
        let Some(from) = payload.get("from").and_then(JsonValue::as_i64) else {
            return json_error(400, "JSON body must contain numeric field 'from'");
        };
        let Some(to) = payload.get("to").and_then(JsonValue::as_i64) else {
            return json_error(400, "JSON body must contain numeric field 'to'");
        };

        let db = self.runtime.db();
        let mut builder = db
            .edge(collection, label)
            .from(EntityId::new(from as u64))
            .to(EntityId::new(to as u64));

        if let Some(weight) = payload.get("weight").and_then(JsonValue::as_f64) {
            builder = builder.weight(weight as f32);
        }

        if let Some(properties) = payload.get("properties").and_then(JsonValue::as_object) {
            for (key, value) in properties {
                let value = match json_to_storage_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.property(key.clone(), value);
            }
        }

        if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
            for (key, value) in metadata {
                let value = match json_to_metadata_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.metadata(key.clone(), value);
            }
        }

        match builder.save() {
            Ok(id) => json_response(200, created_entity_to_json(&db, id)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_bulk_create(
        &self,
        collection: &str,
        body: Vec<u8>,
        handler: fn(&Self, &str, Vec<u8>) -> HttpResponse,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(items) = payload.get("items").and_then(JsonValue::as_array) else {
            return json_error(400, "JSON body must contain an array field named 'items'");
        };
        if items.is_empty() {
            return json_error(400, "field 'items' cannot be empty");
        }

        let mut results = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let item_body = match json_to_vec(item) {
                Ok(body) => body,
                Err(err) => {
                    return json_error(
                        400,
                        format!("failed to encode bulk item at index {index}: {err}"),
                    )
                }
            };
            let response = handler(self, collection, item_body);
            if response.status >= 400 {
                let message = String::from_utf8_lossy(&response.body);
                return json_error(
                    response.status,
                    format!("bulk item {index} failed: {message}"),
                );
            }

            let parsed = match std::str::from_utf8(&response.body) {
                Ok(text) => parse_json(text).ok().map(JsonValue::from).unwrap_or(JsonValue::Null),
                Err(_) => JsonValue::Null,
            };
            results.push(parsed);
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("count".to_string(), JsonValue::Number(results.len() as f64));
        object.insert("items".to_string(), JsonValue::Array(results));
        json_response(200, JsonValue::Object(object))
    }

    fn handle_create_vector(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let dense = match json_vector_from_value(payload.get("dense"), "dense") {
            Ok(vector) => vector,
            Err(response) => return response,
        };

        let db = self.runtime.db();
        let mut builder = db.vector(collection).dense(dense);

        if let Some(content) = payload.get("content").and_then(JsonValue::as_str) {
            builder = builder.content(content.to_string());
        }

        if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
            for (key, value) in metadata {
                let value = match json_to_metadata_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                builder = builder.metadata(key.clone(), value);
            }
        }

        if let Some(link) = payload.get("link").and_then(JsonValue::as_object) {
            if let Some(row) = link.get("row") {
                let Some(object) = row.as_object() else {
                    return json_error(400, "vector row link must be an object");
                };
                let Some(table) = object.get("table").and_then(JsonValue::as_str) else {
                    return json_error(400, "vector row link requires 'table'");
                };
                let Some(row_id) = object.get("row_id").and_then(JsonValue::as_i64) else {
                    return json_error(400, "vector row link requires numeric 'row_id'");
                };
                builder = builder.link_to_table(TableRef::new(table, row_id as u64));
            }
            if let Some(node) = link.get("node") {
                let (target_collection, id) = match json_collection_entity_ref(node, "node") {
                    Ok(result) => result,
                    Err(response) => return response,
                };
                builder = builder.link_to_node(NodeRef::new(target_collection, EntityId::new(id)));
            }
        }

        match builder.save() {
            Ok(id) => json_response(200, created_entity_to_json(&db, id)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_patch_entity(&self, collection: &str, id: u64, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let db = self.runtime.db();
        let store = db.store();
        let Some(manager) = store.get_collection(collection) else {
            return json_error(404, format!("collection not found: {collection}"));
        };
        let entity_id = EntityId::new(id);
        let Some(mut entity) = manager.get(entity_id) else {
            return json_error(404, format!("entity not found: {id}"));
        };

        if let Some(fields) = payload.get("fields").and_then(JsonValue::as_object) {
            match &mut entity.data {
                EntityData::Row(row) => {
                    let named = row.named.get_or_insert_with(Default::default);
                    for (key, value) in fields {
                        let value = match json_to_storage_value(value) {
                            Ok(value) => value,
                            Err(response) => return response,
                        };
                        named.insert(key.clone(), value);
                    }
                }
                EntityData::Node(node) => {
                    for (key, value) in fields {
                        let value = match json_to_storage_value(value) {
                            Ok(value) => value,
                            Err(response) => return response,
                        };
                        node.properties.insert(key.clone(), value);
                    }
                }
                EntityData::Edge(edge) => {
                    for (key, value) in fields {
                        let value = match json_to_storage_value(value) {
                            Ok(value) => value,
                            Err(response) => return response,
                        };
                        edge.properties.insert(key.clone(), value);
                    }
                }
                EntityData::Vector(vector) => {
                    if let Some(content) = fields.get("content").and_then(JsonValue::as_str) {
                        vector.content = Some(content.to_string());
                    }
                    if let Some(dense) = fields.get("dense") {
                        let dense = match json_vector_from_value(Some(dense), "dense") {
                            Ok(vector) => vector,
                            Err(response) => return response,
                        };
                        vector.dense = dense;
                    }
                }
            }
        }

        if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
            let mut merged = store
                .get_metadata(collection, entity_id)
                .unwrap_or_else(crate::storage::unified::Metadata::new);
            for (key, value) in metadata {
                let value = match json_to_metadata_value(value) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                merged.set(key.clone(), value);
            }
            if let Err(err) = store.set_metadata(collection, entity_id, merged) {
                return json_error(400, err.to_string());
            }
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

        match manager.update(entity) {
            Ok(()) => json_response(200, created_entity_to_json(&db, entity_id)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_delete_entity(&self, collection: &str, id: u64) -> HttpResponse {
        let db = self.runtime.db();
        let deleted = db
            .store()
            .delete(collection, EntityId::new(id))
            .map_err(|err| err.to_string());

        match deleted {
            Ok(true) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("deleted".to_string(), JsonValue::Bool(true));
                object.insert("id".to_string(), JsonValue::Number(id as f64));
                json_response(200, JsonValue::Object(object))
            }
            Ok(false) => json_error(404, format!("entity not found: {id}")),
            Err(err) => json_error(400, err),
        }
    }

    fn handle_query(&self, body: Vec<u8>) -> HttpResponse {
        let query = match extract_query(&body) {
            Ok(query) => query,
            Err(response) => return response,
        };

        match self.runtime.execute_query(&query) {
            Ok(result) => json_response(200, runtime_query_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_similar(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let vector = match json_vector_field(&payload, "vector") {
            Ok(vector) => vector,
            Err(response) => return response,
        };
        let k = json_usize_field(&payload, "k").unwrap_or(10).max(1);
        let min_score = json_f32_field(&payload, "min_score").unwrap_or(0.0);

        match self.runtime.search_similar(collection, &vector, k, min_score) {
            Ok(results) => json_response(200, similar_results_to_json(collection, k, min_score, &results)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_ivf_search(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let vector = match json_vector_field(&payload, "vector") {
            Ok(vector) => vector,
            Err(response) => return response,
        };
        let k = json_usize_field(&payload, "k").unwrap_or(10).max(1);
        let n_lists = json_usize_field(&payload, "n_lists").unwrap_or(32).max(1);
        let n_probes = json_usize_field(&payload, "n_probes");

        match self
            .runtime
            .search_ivf(collection, &vector, k, n_lists, n_probes)
        {
            Ok(result) => json_response(200, runtime_ivf_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_hybrid_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let vector = match optional_json_vector_field(&payload, "vector") {
            Ok(vector) => vector,
            Err(response) => return response,
        };
        let k = json_usize_field(&payload, "k");
        let collections = json_string_list_field(&payload, "collections");
        let graph_pattern = match json_graph_pattern(&payload) {
            Ok(pattern) => pattern,
            Err(response) => return response,
        };
        let filters = match json_filters(&payload) {
            Ok(filters) => filters,
            Err(response) => return response,
        };
        let weights = json_weights(&payload);
        let min_score = json_f32_field(&payload, "min_score");
        let limit = json_usize_field(&payload, "limit");

        match self.runtime.search_hybrid(
            vector,
            k,
            collections,
            graph_pattern,
            filters,
            weights,
            min_score,
            limit,
        ) {
            Ok(result) => json_response(200, dsl_query_result_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_text_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(query) = payload.get("query").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'query' must be a string");
        };
        if query.trim().is_empty() {
            return json_error(400, "field 'query' cannot be empty");
        }

        let collections = json_string_list_field(&payload, "collections");
        let fields = json_string_list_field(&payload, "fields");
        let limit = json_usize_field(&payload, "limit");
        let fuzzy = json_bool_field(&payload, "fuzzy").unwrap_or(false);

        match self.runtime.search_text(
            query.to_string(),
            collections,
            fields,
            limit,
            fuzzy,
        ) {
            Ok(result) => json_response(200, dsl_query_result_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_neighborhood(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(node) = payload.get("node").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'node' must be a string");
        };
        let direction = parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Both);
        let max_depth = json_usize_field(&payload, "max_depth").unwrap_or(1).max(1);
        let edge_labels = json_string_list_field(&payload, "edge_labels");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self
            .runtime
            .graph_neighborhood(node, direction, max_depth, edge_labels, projection)
        {
            Ok(result) => json_response(200, graph_neighborhood_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_traverse(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(source) = payload.get("source").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'source' must be a string");
        };
        let direction = parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Outgoing);
        let strategy =
            parse_graph_traversal_strategy(payload.get("strategy").and_then(JsonValue::as_str))
                .unwrap_or(RuntimeGraphTraversalStrategy::Bfs);
        let max_depth = json_usize_field(&payload, "max_depth").unwrap_or(3).max(1);
        let edge_labels = json_string_list_field(&payload, "edge_labels");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_traverse(
            source,
            direction,
            max_depth,
            strategy,
            edge_labels,
            projection,
        ) {
            Ok(result) => json_response(200, graph_traversal_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_shortest_path(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(source) = payload.get("source").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'source' must be a string");
        };
        let Some(target) = payload.get("target").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'target' must be a string");
        };
        let direction = parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Outgoing);
        let algorithm =
            parse_graph_path_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
                .unwrap_or(RuntimeGraphPathAlgorithm::Dijkstra);
        let edge_labels = json_string_list_field(&payload, "edge_labels");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_shortest_path(
            source,
            target,
            direction,
            algorithm,
            edge_labels,
            projection,
        ) {
            Ok(result) => json_response(200, graph_path_result_to_json(&result)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_components(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let mode = parse_graph_components_mode(
            payload
                .as_object()
                .and_then(|object| object.get("mode"))
                .and_then(JsonValue::as_str),
        )
        .unwrap_or(RuntimeGraphComponentsMode::Connected);
        let min_size = json_usize_field(&payload, "min_size").unwrap_or(1).max(1);
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_components(mode, min_size, projection) {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    "graph.components",
                    projection_name,
                    analytics_metadata(vec![
                        ("mode", graph_components_mode_to_str(mode).to_string()),
                        ("min_size", min_size.to_string()),
                    ]),
                );
                json_response(200, graph_components_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_centrality(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let algorithm = parse_graph_centrality_algorithm(
            payload
                .as_object()
                .and_then(|object| object.get("algorithm"))
                .and_then(JsonValue::as_str),
        )
        .unwrap_or(RuntimeGraphCentralityAlgorithm::PageRank);
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let normalize = json_bool_field(&payload, "normalize").unwrap_or(true);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let alpha = json_f32_field(&payload, "alpha").map(|value| value as f64);
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_centrality(
            algorithm,
            top_k,
            normalize,
            max_iterations,
            epsilon,
            alpha,
            projection,
        ) {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    format!("graph.centrality.{}", graph_centrality_algorithm_to_str(algorithm)),
                    projection_name,
                    analytics_metadata(vec![
                        ("top_k", top_k.to_string()),
                        (
                            "normalize",
                            if normalize { "true" } else { "false" }.to_string(),
                        ),
                    ]),
                );
                json_response(200, graph_centrality_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_community(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let algorithm = parse_graph_community_algorithm(
            payload
                .as_object()
                .and_then(|object| object.get("algorithm"))
                .and_then(JsonValue::as_str),
        )
        .unwrap_or(RuntimeGraphCommunityAlgorithm::Louvain);
        let min_size = json_usize_field(&payload, "min_size").unwrap_or(1).max(1);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let resolution = json_f32_field(&payload, "resolution").map(|value| value as f64);
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self
            .runtime
            .graph_communities(algorithm, min_size, max_iterations, resolution, projection)
        {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    format!("graph.community.{}", graph_community_algorithm_to_str(algorithm)),
                    projection_name,
                    analytics_metadata(vec![
                        ("min_size", min_size.to_string()),
                    ]),
                );
                json_response(200, graph_community_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_clustering(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let include_triangles = json_bool_field(&payload, "include_triangles").unwrap_or(false);
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_clustering(top_k, include_triangles, projection) {
            Ok(result) => {
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
                json_response(200, graph_clustering_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_personalized_pagerank(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(seeds) = json_string_list_field(&payload, "seeds") else {
            return json_error(400, "field 'seeds' must be a non-empty array of strings");
        };
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let alpha = json_f32_field(&payload, "alpha").map(|value| value as f64);
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_personalized_pagerank(
            seeds,
            top_k,
            alpha,
            epsilon,
            max_iterations,
            projection,
        ) {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    "graph.pagerank.personalized",
                    projection_name,
                    analytics_metadata(vec![
                        ("top_k", top_k.to_string()),
                    ]),
                );
                json_response(200, graph_centrality_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_hits(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let top_k = json_usize_field(&payload, "top_k").unwrap_or(25).max(1);
        let epsilon = json_f32_field(&payload, "epsilon").map(|value| value as f64);
        let max_iterations = json_usize_field(&payload, "max_iterations");
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self
            .runtime
            .graph_hits(top_k, epsilon, max_iterations, projection)
        {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    "graph.hits",
                    projection_name,
                    analytics_metadata(vec![
                        ("top_k", top_k.to_string()),
                    ]),
                );
                json_response(200, graph_hits_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_cycles(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let max_length = json_usize_field(&payload, "max_length").unwrap_or(10).max(2);
        let max_cycles = json_usize_field(&payload, "max_cycles").unwrap_or(100).max(1);
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self
            .runtime
            .graph_cycles(max_length, max_cycles, projection)
        {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    "graph.cycles",
                    projection_name,
                    analytics_metadata(vec![
                        ("max_length", max_length.to_string()),
                        ("max_cycles", max_cycles.to_string()),
                    ]),
                );
                json_response(200, graph_cycles_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_topological_sort(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };

        match self.runtime.graph_topological_sort(projection) {
            Ok(result) => {
                let _ = self.runtime.record_analytics_job(
                    "graph.topological_sort",
                    projection_name,
                    BTreeMap::new(),
                );
                json_response(200, graph_topological_sort_to_json(&result))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_projection_upsert(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(name) = json_string_field(&payload, "name") else {
            return json_error(400, "field 'name' must be a string");
        };
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(Some(projection)) => projection,
            Ok(None) => {
                return json_error(
                    400,
                    "graph projection requires at least one of node_labels, node_types, edge_labels or projection_name",
                )
            }
            Err(response) => return response,
        };
        let source = json_string_field(&payload, "source");

        match self.runtime.save_graph_projection(name, projection, source) {
            Ok(projection) => json_response(200, graph_projection_to_json(&projection)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_export(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(name) = payload.get("name").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'name' must be a string");
        };
        if name.trim().is_empty() {
            return json_error(400, "field 'name' cannot be empty");
        }

        match self.runtime.create_export(name.to_string()) {
            Ok(export) => json_response(200, export_descriptor_to_json(&export)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_rebuild_indexes(&self, body: Vec<u8>, path_collection: Option<&str>) -> HttpResponse {
        let body_collection = if body.iter().any(|byte| !byte.is_ascii_whitespace()) {
            match parse_json_body(&body) {
                Ok(payload) => payload
                    .get("collection")
                    .and_then(JsonValue::as_str)
                    .map(|value| value.to_string()),
                Err(response) => return response,
            }
        } else {
            None
        };

        let collection = path_collection
            .map(|value| value.to_string())
            .or(body_collection);

        match self.runtime.rebuild_indexes(collection.as_deref()) {
            Ok(indexes) => json_response(200, indexes_to_json(&indexes)),
            Err(err) => json_error(400, err.to_string()),
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn read_from(stream: &mut TcpStream, max_body_bytes: usize) -> io::Result<Self> {
        let mut buffer = Vec::with_capacity(4096);
        let mut chunk = [0_u8; 2048];
        let header_end = loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before request headers",
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > max_body_bytes.saturating_add(16 * 1024) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request headers too large",
                ));
            }
            if let Some(position) = find_header_end(&buffer) {
                break position;
            }
        };

        let head = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = head.lines();
        let request_line = lines
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?
            .to_string();
        let target = request_parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing path"))?;

        let mut headers = BTreeMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }

        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if content_length > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request body exceeds configured limit",
            ));
        }

        let total_needed = header_end + 4 + content_length;
        while buffer.len() < total_needed {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before request body",
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
        }

        let body = buffer[header_end + 4..total_needed].to_vec();
        let (path, query) = split_target(target);

        Ok(Self {
            method,
            path,
            query,
            headers,
            body,
        })
    }
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
}

impl HttpResponse {
    fn to_http_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.status,
            status_text(self.status),
            self.content_type,
            self.body.len()
        );
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

fn json_ok(message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("message".to_string(), JsonValue::String(message.into()));
    json_response(200, JsonValue::Object(object))
}

fn json_error(status: u16, message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    object.insert("error".to_string(), JsonValue::String(message.into()));
    json_response(status, JsonValue::Object(object))
}

fn json_response(status: u16, value: JsonValue) -> HttpResponse {
    HttpResponse {
        status,
        body: value.to_string_compact().into_bytes(),
        content_type: "application/json",
    }
}

fn collection_from_scan_path(path: &str) -> Option<&str> {
    let prefix = "/collections/";
    let suffix = "/scan";
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() {
        None
    } else {
        Some(collection)
    }
}

fn collection_from_action_path<'a>(path: &'a str, action: &str) -> Option<&'a str> {
    let prefix = "/collections/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() {
        None
    } else {
        Some(collection)
    }
}

fn collection_entity_path(path: &str) -> Option<(&str, u64)> {
    let prefix = "/collections/";
    let suffix = "/entities/";
    let trimmed = path.strip_prefix(prefix)?;
    let (collection, id) = trimmed.split_once(suffix)?;
    let collection = collection.trim_matches('/');
    let id = id.trim_matches('/').parse::<u64>().ok()?;
    if collection.is_empty() {
        None
    } else {
        Some((collection, id))
    }
}

fn index_named_action_path(path: &str, action: &str) -> Option<String> {
    let prefix = "/indexes/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let name = trimmed.trim_matches('/');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn split_target(target: &str) -> (String, BTreeMap<String, String>) {
    match target.split_once('?') {
        Some((path, raw_query)) => (path.to_string(), parse_query_string(raw_query)),
        None => (target.to_string(), BTreeMap::new()),
    }
}

fn parse_query_string(input: &str) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(key.to_string(), value.to_string());
    }
    params
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn extract_query(body: &[u8]) -> Result<String, HttpResponse> {
    let text = std::str::from_utf8(body).map_err(|_| json_error(400, "request body must be UTF-8"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(json_error(400, "missing query body"));
    }

    if trimmed.starts_with('{') {
        if let Ok(parsed) = parse_json(trimmed) {
            let json = JsonValue::from(parsed);
            if let Some(query) = json.get("query").and_then(JsonValue::as_str) {
                if query.trim().is_empty() {
                    return Err(json_error(400, "query field cannot be empty"));
                }
                return Ok(query.to_string());
            }
            return Err(json_error(
                400,
                "JSON body must contain a string field named 'query'",
            ));
        }
    }

    Ok(trimmed.to_string())
}

fn parse_json_body(body: &[u8]) -> Result<JsonValue, HttpResponse> {
    let text =
        std::str::from_utf8(body).map_err(|_| json_error(400, "request body must be UTF-8"))?;
    let parsed =
        parse_json(text).map_err(|err| json_error(400, format!("invalid JSON body: {err}")))?;
    Ok(JsonValue::from(parsed))
}

fn parse_json_body_allow_empty(body: &[u8]) -> Result<JsonValue, HttpResponse> {
    if body.is_empty() {
        return Ok(JsonValue::Object(Map::new()));
    }
    parse_json_body(body)
}

fn json_vector_field(payload: &JsonValue, field: &str) -> Result<Vec<f32>, HttpResponse> {
    let values = payload
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            json_error(
                400,
                format!("JSON body must contain an array field named '{field}'"),
            )
        })?;
    if values.is_empty() {
        return Err(json_error(400, format!("field '{field}' cannot be empty")));
    }

    let mut vector = Vec::with_capacity(values.len());
    for value in values {
        let number = value.as_f64().ok_or_else(|| {
            json_error(400, format!("field '{field}' must contain only numbers"))
        })?;
        vector.push(number as f32);
    }
    Ok(vector)
}

fn json_vector_from_value(
    value: Option<&JsonValue>,
    field: &str,
) -> Result<Vec<f32>, HttpResponse> {
    let Some(JsonValue::Array(values)) = value else {
        return Err(json_error(
            400,
            format!("JSON body must contain an array field named '{field}'"),
        ));
    };
    if values.is_empty() {
        return Err(json_error(400, format!("field '{field}' cannot be empty")));
    }

    let mut vector = Vec::with_capacity(values.len());
    for value in values {
        let number = value.as_f64().ok_or_else(|| {
            json_error(400, format!("field '{field}' must contain only numbers"))
        })?;
        vector.push(number as f32);
    }
    Ok(vector)
}

fn optional_json_vector_field(
    payload: &JsonValue,
    field: &str,
) -> Result<Option<Vec<f32>>, HttpResponse> {
    match payload.get(field) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(_) => json_vector_field(payload, field).map(Some),
    }
}

fn json_usize_field(payload: &JsonValue, field: &str) -> Option<usize> {
    payload
        .get(field)
        .and_then(JsonValue::as_i64)
        .and_then(|value| usize::try_from(value).ok())
}

fn json_string_field(payload: &JsonValue, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn json_f32_field(payload: &JsonValue, field: &str) -> Option<f32> {
    payload
        .get(field)
        .and_then(JsonValue::as_f64)
        .map(|value| value as f32)
}

fn json_bool_field(payload: &JsonValue, field: &str) -> Option<bool> {
    payload.get(field).and_then(JsonValue::as_bool)
}

fn authorization_bearer_token<'a>(headers: &'a BTreeMap<String, String>) -> Option<&'a str> {
    headers.get("authorization")?.strip_prefix("Bearer ")
}

fn json_collection_entity_ref(
    value: &JsonValue,
    kind: &str,
) -> Result<(String, u64), HttpResponse> {
    let Some(object) = value.as_object() else {
        return Err(json_error(400, format!("{kind} link must be an object")));
    };
    let Some(collection) = object.get("collection").and_then(JsonValue::as_str) else {
        return Err(json_error(400, format!("{kind} link requires 'collection'")));
    };
    let Some(id) = object.get("id").and_then(JsonValue::as_i64) else {
        return Err(json_error(400, format!("{kind} link requires numeric 'id'")));
    };
    Ok((collection.to_string(), id as u64))
}

fn json_to_storage_value(value: &JsonValue) -> Result<Value, HttpResponse> {
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
        JsonValue::Array(_) | JsonValue::Object(_) => json_to_vec(value)
            .map(Value::Json)
            .map_err(|err| json_error(400, format!("failed to serialize JSON value: {err}"))),
    }
}

fn json_to_metadata_value(value: &JsonValue) -> Result<MetadataValue, HttpResponse> {
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
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Array(items))
        }
        JsonValue::Object(map) => {
            let mut object = std::collections::HashMap::new();
            for (key, value) in map {
                object.insert(key.clone(), json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Object(object))
        }
    }
}

fn created_entity_to_json(db: &crate::storage::RedDB, id: EntityId) -> JsonValue {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("id".to_string(), JsonValue::Number(id.raw() as f64));
    object.insert(
        "entity".to_string(),
        db.get(id)
            .map(|entity| entity_to_json(&entity))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn json_string_list_field(payload: &JsonValue, field: &str) -> Option<Vec<String>> {
    let values = payload.get(field)?.as_array()?;
    let mut out = Vec::new();
    for value in values {
        if let Some(text) = value.as_str() {
            out.push(text.to_string());
        }
    }
    (!out.is_empty()).then_some(out)
}

fn parse_graph_direction(value: Option<&str>) -> Option<RuntimeGraphDirection> {
    match value.map(normalize_graph_token).as_deref() {
        Some("outgoing") | Some("out") => Some(RuntimeGraphDirection::Outgoing),
        Some("incoming") | Some("in") => Some(RuntimeGraphDirection::Incoming),
        Some("both") | Some("any") => Some(RuntimeGraphDirection::Both),
        Some(_) => None,
        None => None,
    }
}

fn parse_graph_traversal_strategy(value: Option<&str>) -> Option<RuntimeGraphTraversalStrategy> {
    match value.map(normalize_graph_token).as_deref() {
        Some("bfs") | Some("breadthfirst") => Some(RuntimeGraphTraversalStrategy::Bfs),
        Some("dfs") | Some("depthfirst") => Some(RuntimeGraphTraversalStrategy::Dfs),
        Some(_) => None,
        None => None,
    }
}

fn parse_graph_path_algorithm(value: Option<&str>) -> Option<RuntimeGraphPathAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("bfs") => Some(RuntimeGraphPathAlgorithm::Bfs),
        Some("dijkstra") | Some("weighted") => Some(RuntimeGraphPathAlgorithm::Dijkstra),
        Some(_) => None,
        None => None,
    }
}

fn parse_graph_components_mode(value: Option<&str>) -> Option<RuntimeGraphComponentsMode> {
    match value.map(normalize_graph_token).as_deref() {
        Some("connected") | Some("undirected") => Some(RuntimeGraphComponentsMode::Connected),
        Some("weak") | Some("wcc") => Some(RuntimeGraphComponentsMode::Weak),
        Some("strong") | Some("scc") => Some(RuntimeGraphComponentsMode::Strong),
        Some(_) => None,
        None => None,
    }
}

fn parse_graph_centrality_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCentralityAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("degree") => Some(RuntimeGraphCentralityAlgorithm::Degree),
        Some("closeness") => Some(RuntimeGraphCentralityAlgorithm::Closeness),
        Some("betweenness") => Some(RuntimeGraphCentralityAlgorithm::Betweenness),
        Some("eigenvector") => Some(RuntimeGraphCentralityAlgorithm::Eigenvector),
        Some("pagerank") | Some("page_rank") => Some(RuntimeGraphCentralityAlgorithm::PageRank),
        Some(_) => None,
        None => None,
    }
}

fn parse_graph_community_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCommunityAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("labelpropagation") | Some("label") => {
            Some(RuntimeGraphCommunityAlgorithm::LabelPropagation)
        }
        Some("louvain") => Some(RuntimeGraphCommunityAlgorithm::Louvain),
        Some(_) => None,
        None => None,
    }
}

fn graph_components_mode_to_str(mode: RuntimeGraphComponentsMode) -> &'static str {
    match mode {
        RuntimeGraphComponentsMode::Connected => "connected",
        RuntimeGraphComponentsMode::Weak => "weak",
        RuntimeGraphComponentsMode::Strong => "strong",
    }
}

fn graph_centrality_algorithm_to_str(algorithm: RuntimeGraphCentralityAlgorithm) -> &'static str {
    match algorithm {
        RuntimeGraphCentralityAlgorithm::Degree => "degree",
        RuntimeGraphCentralityAlgorithm::Closeness => "closeness",
        RuntimeGraphCentralityAlgorithm::Betweenness => "betweenness",
        RuntimeGraphCentralityAlgorithm::Eigenvector => "eigenvector",
        RuntimeGraphCentralityAlgorithm::PageRank => "pagerank",
    }
}

fn graph_community_algorithm_to_str(algorithm: RuntimeGraphCommunityAlgorithm) -> &'static str {
    match algorithm {
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

fn json_graph_projection(payload: &JsonValue) -> Option<RuntimeGraphProjection> {
    let node_labels = json_string_list_field(payload, "node_labels");
    let node_types = json_string_list_field(payload, "node_types");
    let edge_labels = json_string_list_field(payload, "edge_labels");

    if node_labels.is_none() && node_types.is_none() && edge_labels.is_none() {
        None
    } else {
        Some(RuntimeGraphProjection {
            node_labels,
            node_types,
            edge_labels,
        })
    }
}

fn normalize_graph_token(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn json_graph_pattern(payload: &JsonValue) -> Result<Option<RuntimeGraphPattern>, HttpResponse> {
    let Some(graph) = payload.get("graph") else {
        return Ok(None);
    };
    let Some(graph) = graph.as_object() else {
        return Err(json_error(400, "field 'graph' must be an object"));
    };

    let node_label = graph
        .get("label")
        .and_then(JsonValue::as_str)
        .map(|value| value.to_string());
    let node_type = graph
        .get("node_type")
        .and_then(JsonValue::as_str)
        .map(|value| value.to_string());
    let edge_labels = graph
        .get("edge_labels")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Some(RuntimeGraphPattern {
        node_label,
        node_type,
        edge_labels,
    }))
}

fn json_weights(payload: &JsonValue) -> Option<RuntimeQueryWeights> {
    let weights = payload.get("weights")?.as_object()?;
    Some(RuntimeQueryWeights {
        vector: weights.get("vector").and_then(JsonValue::as_f64).unwrap_or(0.5) as f32,
        graph: weights.get("graph").and_then(JsonValue::as_f64).unwrap_or(0.3) as f32,
        filter: weights.get("filter").and_then(JsonValue::as_f64).unwrap_or(0.2) as f32,
    })
}

fn json_filters(payload: &JsonValue) -> Result<Vec<RuntimeFilter>, HttpResponse> {
    let Some(values) = payload.get("filters") else {
        return Ok(Vec::new());
    };
    let Some(values) = values.as_array() else {
        return Err(json_error(400, "field 'filters' must be an array"));
    };

    let mut filters = Vec::with_capacity(values.len());
    for value in values {
        let Some(filter) = value.as_object() else {
            return Err(json_error(400, "every filter must be an object"));
        };
        let Some(field) = filter.get("field").and_then(JsonValue::as_str) else {
            return Err(json_error(400, "every filter must contain a string field named 'field'"));
        };
        let Some(op) = filter.get("op").and_then(JsonValue::as_str) else {
            return Err(json_error(400, "every filter must contain a string field named 'op'"));
        };
        let parsed_value = match filter.get("value") {
            Some(value) => Some(json_runtime_filter_value(value)?),
            None => None,
        };

        filters.push(RuntimeFilter {
            field: field.to_string(),
            op: op.to_string(),
            value: parsed_value,
        });
    }

    Ok(filters)
}

fn json_runtime_filter_value(value: &JsonValue) -> Result<RuntimeFilterValue, HttpResponse> {
    match value {
        JsonValue::Null => Ok(RuntimeFilterValue::Null),
        JsonValue::Bool(value) => Ok(RuntimeFilterValue::Bool(*value)),
        JsonValue::Number(value) => {
            if value.fract() == 0.0 {
                Ok(RuntimeFilterValue::Int(*value as i64))
            } else {
                Ok(RuntimeFilterValue::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(RuntimeFilterValue::String(value.clone())),
        JsonValue::Array(values) => Ok(RuntimeFilterValue::List(
            values
                .iter()
                .map(json_runtime_filter_value)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        JsonValue::Object(map) => {
            if let (Some(start), Some(end)) = (map.get("start"), map.get("end")) {
                Ok(RuntimeFilterValue::Range(
                    Box::new(json_runtime_filter_value(start)?),
                    Box::new(json_runtime_filter_value(end)?),
                ))
            } else {
                Err(json_error(
                    400,
                    "filter object values must be either scalars, arrays, or {start,end} ranges",
                ))
            }
        }
    }
}

fn query_mode_name(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Sql => "sql",
        QueryMode::Gremlin => "gremlin",
        QueryMode::Cypher => "cypher",
        QueryMode::Sparql => "sparql",
        QueryMode::Path => "path",
        QueryMode::Natural => "natural",
        QueryMode::Unknown => "unknown",
    }
}

fn query_mode_capability(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Sql => "table",
        QueryMode::Gremlin | QueryMode::Cypher | QueryMode::Sparql | QueryMode::Path => "graph",
        QueryMode::Natural => "multi",
        QueryMode::Unknown => "unknown",
    }
}

fn runtime_query_to_json(result: &RuntimeQueryResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("query".to_string(), JsonValue::String(result.query.clone()));
    object.insert(
        "mode".to_string(),
        JsonValue::String(query_mode_name(result.mode).to_string()),
    );
    object.insert(
        "capability".to_string(),
        JsonValue::String(query_mode_capability(result.mode).to_string()),
    );
    object.insert(
        "statement".to_string(),
        JsonValue::String(result.statement.to_string()),
    );
    object.insert(
        "engine".to_string(),
        JsonValue::String(result.engine.to_string()),
    );
    object.insert("result".to_string(), unified_result_to_json(&result.result));
    JsonValue::Object(object)
}

fn similar_results_to_json(
    collection: &str,
    k: usize,
    min_score: f32,
    results: &[SimilarResult],
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
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

fn runtime_ivf_to_json(result: &RuntimeIvfSearchResult) -> JsonValue {
    let mut stats = Map::new();
    stats.insert(
        "total_vectors".to_string(),
        JsonValue::Number(result.stats.total_vectors as f64),
    );
    stats.insert(
        "n_lists".to_string(),
        JsonValue::Number(result.stats.n_lists as f64),
    );
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
    object.insert(
        "n_lists".to_string(),
        JsonValue::Number(result.n_lists as f64),
    );
    object.insert(
        "n_probes".to_string(),
        JsonValue::Number(result.n_probes as f64),
    );
    object.insert("stats".to_string(), JsonValue::Object(stats));
    object.insert(
        "matches".to_string(),
        JsonValue::Array(
            result
                .matches
                .iter()
                .map(|item| {
                    let mut entry = Map::new();
                    entry.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(item.entity_id as f64),
                    );
                    entry.insert(
                        "distance".to_string(),
                        JsonValue::Number(item.distance as f64),
                    );
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

fn graph_neighborhood_to_json(result: &RuntimeGraphNeighborhoodResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(result.source.clone()));
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "max_depth".to_string(),
        JsonValue::Number(result.max_depth as f64),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(result.nodes.iter().map(graph_visit_to_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_traversal_to_json(result: &RuntimeGraphTraversalResult) -> JsonValue {
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
    object.insert(
        "max_depth".to_string(),
        JsonValue::Number(result.max_depth as f64),
    );
    object.insert(
        "visits".to_string(),
        JsonValue::Array(result.visits.iter().map(graph_visit_to_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_path_result_to_json(result: &RuntimeGraphPathResult) -> JsonValue {
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
            Some(path) => graph_path_to_json(path),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn graph_components_to_json(result: &RuntimeGraphComponentsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "mode".to_string(),
        JsonValue::String(
            match result.mode {
                RuntimeGraphComponentsMode::Connected => "connected",
                RuntimeGraphComponentsMode::Weak => "weak",
                RuntimeGraphComponentsMode::Strong => "strong",
            }
            .to_string(),
        ),
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
                        JsonValue::Array(
                            component
                                .nodes
                                .iter()
                                .cloned()
                                .map(JsonValue::String)
                                .collect(),
                        ),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_centrality_to_json(result: &RuntimeGraphCentralityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(
            match result.algorithm {
                RuntimeGraphCentralityAlgorithm::Degree => "degree",
                RuntimeGraphCentralityAlgorithm::Closeness => "closeness",
                RuntimeGraphCentralityAlgorithm::Betweenness => "betweenness",
                RuntimeGraphCentralityAlgorithm::Eigenvector => "eigenvector",
                RuntimeGraphCentralityAlgorithm::PageRank => "pagerank",
            }
            .to_string(),
        ),
    );
    object.insert(
        "normalized".to_string(),
        result
            .normalized
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "iterations".to_string(),
        result
            .iterations
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result
            .converged
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "scores".to_string(),
        JsonValue::Array(
            result
                .scores
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_to_json(&score.node));
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
                    item.insert("node".to_string(), graph_node_to_json(&score.node));
                    item.insert(
                        "in_degree".to_string(),
                        JsonValue::Number(score.in_degree as f64),
                    );
                    item.insert(
                        "out_degree".to_string(),
                        JsonValue::Number(score.out_degree as f64),
                    );
                    item.insert(
                        "total_degree".to_string(),
                        JsonValue::Number(score.total_degree as f64),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_community_to_json(result: &RuntimeGraphCommunityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(
            match result.algorithm {
                RuntimeGraphCommunityAlgorithm::LabelPropagation => "label_propagation",
                RuntimeGraphCommunityAlgorithm::Louvain => "louvain",
            }
            .to_string(),
        ),
    );
    object.insert("count".to_string(), JsonValue::Number(result.count as f64));
    object.insert(
        "iterations".to_string(),
        result
            .iterations
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result
            .converged
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "modularity".to_string(),
        result
            .modularity
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "passes".to_string(),
        result
            .passes
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
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
                        JsonValue::Array(
                            community
                                .nodes
                                .iter()
                                .cloned()
                                .map(JsonValue::String)
                                .collect(),
                        ),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_clustering_to_json(result: &RuntimeGraphClusteringResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("global".to_string(), JsonValue::Number(result.global));
    object.insert(
        "triangle_count".to_string(),
        result
            .triangle_count
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "local".to_string(),
        JsonValue::Array(
            result
                .local
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_to_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_hits_to_json(result: &RuntimeGraphHitsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "iterations".to_string(),
        JsonValue::Number(result.iterations as f64),
    );
    object.insert("converged".to_string(), JsonValue::Bool(result.converged));
    object.insert(
        "hubs".to_string(),
        JsonValue::Array(
            result
                .hubs
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_to_json(&score.node));
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
                    item.insert("node".to_string(), graph_node_to_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_cycles_to_json(result: &RuntimeGraphCyclesResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "limit_reached".to_string(),
        JsonValue::Bool(result.limit_reached),
    );
    object.insert(
        "cycles".to_string(),
        JsonValue::Array(result.cycles.iter().map(graph_path_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_topological_sort_to_json(result: &RuntimeGraphTopologicalSortResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("acyclic".to_string(), JsonValue::Bool(result.acyclic));
    object.insert(
        "ordered_nodes".to_string(),
        JsonValue::Array(
            result
                .ordered_nodes
                .iter()
                .map(graph_node_to_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn graph_path_to_json(path: &crate::runtime::RuntimeGraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "hop_count".to_string(),
        JsonValue::Number(path.hop_count as f64),
    );
    object.insert(
        "total_weight".to_string(),
        JsonValue::Number(path.total_weight),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().map(graph_node_to_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(graph_edge_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn graph_visit_to_json(visit: &crate::runtime::RuntimeGraphVisit) -> JsonValue {
    let mut object = Map::new();
    object.insert("depth".to_string(), JsonValue::Number(visit.depth as f64));
    object.insert("node".to_string(), graph_node_to_json(&visit.node));
    JsonValue::Object(object)
}

fn graph_node_to_json(node: &crate::runtime::RuntimeGraphNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_type.clone()),
    );
    object.insert(
        "out_edge_count".to_string(),
        JsonValue::Number(node.out_edge_count as f64),
    );
    object.insert(
        "in_edge_count".to_string(),
        JsonValue::Number(node.in_edge_count as f64),
    );
    JsonValue::Object(object)
}

fn graph_edge_to_json(edge: &crate::runtime::RuntimeGraphEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(edge.source.clone()));
    object.insert("target".to_string(), JsonValue::String(edge.target.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_type.clone()),
    );
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_direction_to_str(direction: RuntimeGraphDirection) -> &'static str {
    match direction {
        RuntimeGraphDirection::Outgoing => "outgoing",
        RuntimeGraphDirection::Incoming => "incoming",
        RuntimeGraphDirection::Both => "both",
    }
}

fn graph_traversal_strategy_to_str(strategy: RuntimeGraphTraversalStrategy) -> &'static str {
    match strategy {
        RuntimeGraphTraversalStrategy::Bfs => "bfs",
        RuntimeGraphTraversalStrategy::Dfs => "dfs",
    }
}

fn graph_path_algorithm_to_str(algorithm: RuntimeGraphPathAlgorithm) -> &'static str {
    match algorithm {
        RuntimeGraphPathAlgorithm::Bfs => "bfs",
        RuntimeGraphPathAlgorithm::Dijkstra => "dijkstra",
    }
}

fn dsl_query_result_to_json(result: &DslQueryResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "matches".to_string(),
        JsonValue::Array(result.matches.iter().map(scored_match_to_json).collect()),
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

fn scored_match_to_json(item: &crate::storage::ScoredMatch) -> JsonValue {
    let mut object = Map::new();
    object.insert("entity".to_string(), entity_to_json(&item.entity));
    object.insert("score".to_string(), JsonValue::Number(item.score as f64));
    object.insert(
        "components".to_string(),
        match_components_to_json(&item.components),
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

fn match_components_to_json(components: &MatchComponents) -> JsonValue {
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

fn unified_result_to_json(result: &UnifiedResult) -> JsonValue {
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
        "records".to_string(),
        JsonValue::Array(result.records.iter().map(unified_record_to_json).collect()),
    );
    object.insert("stats".to_string(), query_stats_to_json(&result.stats));
    JsonValue::Object(object)
}

fn unified_record_to_json(record: &UnifiedRecord) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "values".to_string(),
        JsonValue::Object(
            record
                .values
                .iter()
                .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Object(
            record
                .nodes
                .iter()
                .map(|(key, value)| (key.clone(), matched_node_to_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Object(
            record
                .edges
                .iter()
                .map(|(key, value)| (key.clone(), matched_edge_to_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "paths".to_string(),
        JsonValue::Array(record.paths.iter().map(graph_path_to_json).collect()),
    );
    object.insert(
        "vector_results".to_string(),
        JsonValue::Array(
            record
                .vector_results
                .iter()
                .map(vector_search_result_to_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn matched_node_to_json(node: &MatchedNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_type.as_str().to_string()),
    );
    JsonValue::Object(object)
}

fn matched_edge_to_json(edge: &MatchedEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("from".to_string(), JsonValue::String(edge.from.clone()));
    object.insert("to".to_string(), JsonValue::String(edge.to.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_type.as_str().to_string()),
    );
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_path_to_json(path: &GraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().cloned().map(JsonValue::String).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(matched_edge_to_json).collect()),
    );
    object.insert(
        "total_weight".to_string(),
        JsonValue::Number(path.total_weight as f64),
    );
    JsonValue::Object(object)
}

fn vector_search_result_to_json(result: &VectorSearchResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::Number(result.id as f64));
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "vector".to_string(),
        match &result.vector {
            Some(vector) => JsonValue::Array(
                vector
                    .iter()
                    .map(|value| JsonValue::Number(*value as f64))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        match &result.metadata {
            Some(metadata) => JsonValue::Object(
                metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_node".to_string(),
        match &result.linked_node {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_row".to_string(),
        match &result.linked_row {
            Some((table, row_id)) => {
                let mut linked = Map::new();
                linked.insert("table".to_string(), JsonValue::String(table.clone()));
                linked.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
                JsonValue::Object(linked)
            }
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn query_stats_to_json(stats: &QueryStats) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "nodes_scanned".to_string(),
        JsonValue::Number(stats.nodes_scanned as f64),
    );
    object.insert(
        "edges_scanned".to_string(),
        JsonValue::Number(stats.edges_scanned as f64),
    );
    object.insert(
        "rows_scanned".to_string(),
        JsonValue::Number(stats.rows_scanned as f64),
    );
    object.insert(
        "exec_time_us".to_string(),
        JsonValue::Number(stats.exec_time_us as f64),
    );
    JsonValue::Object(object)
}

fn runtime_stats_to_json(stats: &RuntimeStats) -> JsonValue {
    let mut store = Map::new();
    store.insert(
        "collection_count".to_string(),
        JsonValue::Number(stats.store.collection_count as f64),
    );
    store.insert(
        "total_entities".to_string(),
        JsonValue::Number(stats.store.total_entities as f64),
    );
    store.insert(
        "total_memory_bytes".to_string(),
        JsonValue::Number(stats.store.total_memory_bytes as f64),
    );
    store.insert(
        "cross_ref_count".to_string(),
        JsonValue::Number(stats.store.cross_ref_count as f64),
    );

    let mut object = Map::new();
    object.insert(
        "active_connections".to_string(),
        JsonValue::Number(stats.active_connections as f64),
    );
    object.insert(
        "idle_connections".to_string(),
        JsonValue::Number(stats.idle_connections as f64),
    );
    object.insert(
        "total_checkouts".to_string(),
        JsonValue::Number(stats.total_checkouts as f64),
    );
    object.insert("paged_mode".to_string(), JsonValue::Bool(stats.paged_mode));
    object.insert(
        "started_at_unix_ms".to_string(),
        JsonValue::Number(stats.started_at_unix_ms as f64),
    );
    object.insert("store".to_string(), JsonValue::Object(store));
    JsonValue::Object(object)
}

fn snapshots_to_json(snapshots: &[crate::SnapshotDescriptor]) -> JsonValue {
    JsonValue::Array(
        snapshots
            .iter()
            .map(snapshot_descriptor_to_json)
            .collect(),
    )
}

fn manifest_events_to_json(events: &[crate::ManifestEvent]) -> JsonValue {
    JsonValue::Array(events.iter().map(manifest_event_to_json).collect())
}

fn collection_roots_to_json(roots: &BTreeMap<String, u64>) -> JsonValue {
    let mut object = Map::new();
    for (collection, root) in roots {
        object.insert(collection.clone(), JsonValue::String(root.to_string()));
    }
    JsonValue::Object(object)
}

fn graph_projections_to_json(projections: &[crate::PhysicalGraphProjection]) -> JsonValue {
    JsonValue::Array(
        projections
            .iter()
            .map(graph_projection_to_json)
            .collect(),
    )
}

fn analytics_jobs_to_json(jobs: &[crate::PhysicalAnalyticsJob]) -> JsonValue {
    JsonValue::Array(jobs.iter().map(analytics_job_to_json).collect())
}

fn native_header_to_json(header: crate::storage::engine::PhysicalFileHeader) -> JsonValue {
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

fn repair_policy_to_json(policy: &str) -> JsonValue {
    let mut object = Map::new();
    object.insert("policy".to_string(), JsonValue::String(policy.to_string()));
    JsonValue::Object(object)
}

fn exports_to_json(exports: &[crate::ExportDescriptor]) -> JsonValue {
    JsonValue::Array(exports.iter().map(export_descriptor_to_json).collect())
}

fn indexes_to_json(indexes: &[crate::PhysicalIndexState]) -> JsonValue {
    JsonValue::Array(indexes.iter().map(physical_index_state_to_json).collect())
}

fn physical_index_state_to_json(index: &crate::PhysicalIndexState) -> JsonValue {
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

fn graph_projection_to_json(projection: &crate::PhysicalGraphProjection) -> JsonValue {
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
        projection
            .last_materialized_sequence
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn analytics_job_to_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(job.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
    object.insert("state".to_string(), JsonValue::String(job.state.clone()));
    object.insert(
        "projection".to_string(),
        match &job.projection {
            Some(projection) => JsonValue::String(projection.clone()),
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
        job.last_run_sequence
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
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

fn manifest_event_to_json(event: &crate::ManifestEvent) -> JsonValue {
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
    object.insert(
        "block".to_string(),
        block_reference_to_json(&event.block),
    );
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

fn block_reference_to_json(reference: &crate::BlockReference) -> JsonValue {
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

fn health_to_json(report: &HealthReport) -> JsonValue {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            let mut object = Map::new();
            object.insert(
                "component".to_string(),
                JsonValue::String(issue.component.clone()),
            );
            object.insert("message".to_string(), JsonValue::String(issue.message.clone()));
            JsonValue::Object(object)
        })
        .collect();

    let diagnostics = report
        .diagnostics
        .iter()
        .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
        .collect();

    let mut object = Map::new();
    object.insert(
        "state".to_string(),
        JsonValue::String(match report.state {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
        }
        .to_string()),
    );
    object.insert("issues".to_string(), JsonValue::Array(issues));
    object.insert("diagnostics".to_string(), JsonValue::Object(diagnostics));
    object.insert(
        "checked_at_unix_ms".to_string(),
        JsonValue::Number(report.checked_at_unix_ms as f64),
    );
    JsonValue::Object(object)
}

fn catalog_to_json(snapshot: &CatalogModelSnapshot) -> JsonValue {
    let mut summary_stats = Map::new();
    for (name, stats) in &snapshot.summary.stats_by_collection {
        let mut object = Map::new();
        object.insert("entities".to_string(), JsonValue::Number(stats.entities as f64));
        object.insert(
            "cross_refs".to_string(),
            JsonValue::Number(stats.cross_refs as f64),
        );
        object.insert("segments".to_string(), JsonValue::Number(stats.segments as f64));
        summary_stats.insert(name.clone(), JsonValue::Object(object));
    }

    let collections = snapshot
        .collections
        .iter()
        .map(collection_descriptor_to_json)
        .collect();

    let mut summary = Map::new();
    summary.insert(
        "name".to_string(),
        JsonValue::String(snapshot.summary.name.clone()),
    );
    summary.insert(
        "total_entities".to_string(),
        JsonValue::Number(snapshot.summary.total_entities as f64),
    );
    summary.insert(
        "total_collections".to_string(),
        JsonValue::Number(snapshot.summary.total_collections as f64),
    );
    summary.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::Number(unix_ms(snapshot.summary.updated_at) as f64),
    );
    summary.insert(
        "stats_by_collection".to_string(),
        JsonValue::Object(summary_stats),
    );

    let mut object = Map::new();
    object.insert("summary".to_string(), JsonValue::Object(summary));
    object.insert("collections".to_string(), JsonValue::Array(collections));
    JsonValue::Object(object)
}

fn collection_descriptor_to_json(descriptor: &CollectionDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(descriptor.name.clone()),
    );
    object.insert(
        "model".to_string(),
        JsonValue::String(match descriptor.model {
            CollectionModel::Table => "table",
            CollectionModel::Document => "document",
            CollectionModel::Graph => "graph",
            CollectionModel::Vector => "vector",
            CollectionModel::Mixed => "mixed",
        }
        .to_string()),
    );
    object.insert(
        "schema_mode".to_string(),
        JsonValue::String(match descriptor.schema_mode {
            SchemaMode::Strict => "strict",
            SchemaMode::SemiStructured => "semi_structured",
            SchemaMode::Dynamic => "dynamic",
        }
        .to_string()),
    );
    object.insert(
        "entities".to_string(),
        JsonValue::Number(descriptor.entities as f64),
    );
    object.insert(
        "cross_refs".to_string(),
        JsonValue::Number(descriptor.cross_refs as f64),
    );
    object.insert(
        "segments".to_string(),
        JsonValue::Number(descriptor.segments as f64),
    );
    object.insert(
        "indices".to_string(),
        JsonValue::Array(
            descriptor
                .indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn scan_page_to_json(page: &ScanPage) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(page.collection.clone()),
    );
    object.insert("total".to_string(), JsonValue::Number(page.total as f64));
    object.insert(
        "next_offset".to_string(),
        match page.next {
            Some(cursor) => JsonValue::Number(cursor.offset as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "items".to_string(),
        JsonValue::Array(page.items.iter().map(entity_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn entity_to_json(entity: &UnifiedEntity) -> JsonValue {
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
    object.insert("identity".to_string(), entity_kind_to_json(&entity.kind));
    object.insert("data".to_string(), entity_data_to_json(&entity.data));
    object.insert(
        "cross_refs".to_string(),
        JsonValue::Array(entity.cross_refs.iter().map(cross_ref_to_json).collect()),
    );
    JsonValue::Object(object)
}

fn entity_kind_to_json(kind: &EntityKind) -> JsonValue {
    let mut object = Map::new();
    match kind {
        EntityKind::TableRow { table, row_id } => {
            object.insert("table".to_string(), JsonValue::String(table.clone()));
            object.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
        }
        EntityKind::GraphNode { label, node_type } => {
            object.insert("label".to_string(), JsonValue::String(label.clone()));
            object.insert(
                "node_type".to_string(),
                JsonValue::String(node_type.clone()),
            );
        }
        EntityKind::GraphEdge {
            label,
            from_node,
            to_node,
            weight,
        } => {
            object.insert("label".to_string(), JsonValue::String(label.clone()));
            object.insert(
                "from_node".to_string(),
                JsonValue::String(from_node.clone()),
            );
            object.insert("to_node".to_string(), JsonValue::String(to_node.clone()));
            object.insert("weight".to_string(), JsonValue::Number(*weight as f64));
        }
        EntityKind::Vector { collection } => {
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.clone()),
            );
        }
    }
    JsonValue::Object(object)
}

fn entity_data_to_json(data: &EntityData) -> JsonValue {
    let mut object = Map::new();
    match data {
        EntityData::Row(row) => {
            object.insert(
                "columns".to_string(),
                JsonValue::Array(row.columns.iter().map(storage_value_to_json).collect()),
            );
            object.insert(
                "named".to_string(),
                match &row.named {
                    Some(named) => JsonValue::Object(
                        named
                            .iter()
                            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                            .collect(),
                    ),
                    None => JsonValue::Null,
                },
            );
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
            object.insert(
                "sparse".to_string(),
                match &vector.sparse {
                    Some(sparse) => {
                        let mut sparse_object = Map::new();
                        sparse_object.insert(
                            "indices".to_string(),
                            JsonValue::Array(
                                sparse
                                    .indices
                                    .iter()
                                    .map(|value| JsonValue::Number(*value as f64))
                                    .collect(),
                            ),
                        );
                        sparse_object.insert(
                            "values".to_string(),
                            JsonValue::Array(
                                sparse
                                    .values
                                    .iter()
                                    .map(|value| JsonValue::Number(*value as f64))
                                    .collect(),
                            ),
                        );
                        JsonValue::Object(sparse_object)
                    }
                    None => JsonValue::Null,
                },
            );
            object.insert(
                "content".to_string(),
                match &vector.content {
                    Some(content) => JsonValue::String(content.clone()),
                    None => JsonValue::Null,
                },
            );
        }
    }
    JsonValue::Object(object)
}

fn cross_ref_to_json(cross_ref: &CrossRef) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "source".to_string(),
        JsonValue::Number(cross_ref.source.raw() as f64),
    );
    object.insert(
        "target".to_string(),
        JsonValue::Number(cross_ref.target.raw() as f64),
    );
    object.insert(
        "target_collection".to_string(),
        JsonValue::String(cross_ref.target_collection.clone()),
    );
    object.insert(
        "ref_type".to_string(),
        JsonValue::String(format!("{:?}", cross_ref.ref_type)),
    );
    object.insert(
        "weight".to_string(),
        JsonValue::Number(cross_ref.weight as f64),
    );
    object.insert(
        "created_at".to_string(),
        JsonValue::Number(cross_ref.created_at as f64),
    );
    JsonValue::Object(object)
}

fn storage_value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Integer(value) => JsonValue::Number(*value as f64),
        Value::UnsignedInteger(value) => JsonValue::Number(*value as f64),
        Value::Float(value) => JsonValue::Number(*value),
        Value::Text(value) => JsonValue::String(value.clone()),
        Value::Blob(value) => JsonValue::String(hex::encode(value)),
        Value::Boolean(value) => JsonValue::Bool(*value),
        Value::Timestamp(value) => JsonValue::Number(*value as f64),
        Value::Duration(value) => JsonValue::Number(*value as f64),
        Value::IpAddr(value) => JsonValue::String(value.to_string()),
        Value::MacAddr(value) => JsonValue::String(format_mac(value)),
        Value::Vector(value) => JsonValue::Array(
            value
                .iter()
                .map(|entry| JsonValue::Number(*entry as f64))
                .collect(),
        ),
        Value::Json(value) => {
            let text = String::from_utf8_lossy(value);
            match parse_json(&text) {
                Ok(parsed) => JsonValue::from(parsed),
                Err(_) => JsonValue::String(hex::encode(value)),
            }
        }
        Value::Uuid(value) => JsonValue::String(hex::encode(value)),
        Value::NodeRef(value) => JsonValue::String(value.clone()),
        Value::EdgeRef(value) => JsonValue::String(value.clone()),
        Value::VectorRef(collection, id) => {
            let mut object = Map::new();
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.clone()),
            );
            object.insert("id".to_string(), JsonValue::Number(*id as f64));
            JsonValue::Object(object)
        }
        Value::RowRef(table, row_id) => {
            let mut object = Map::new();
            object.insert("table".to_string(), JsonValue::String(table.clone()));
            object.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
            JsonValue::Object(object)
        }
    }
}

fn format_mac(bytes: &[u8; 6]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn unix_ms(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}
