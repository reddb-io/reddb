use crate::api::{RedDBOptions, RedDBResult};
use crate::health::{HealthProvider, HealthState};
use crate::json::{to_string as json_to_string, Map, Value as JsonValue};
use crate::runtime::{RedDBRuntime, RuntimeQueryResult, RuntimeStats, ScanPage};
use crate::storage::{EntityData, UnifiedEntity};
use tonic::{Request, Response, Status};

pub mod proto {
    tonic::include_proto!("reddb.v1");
}

use proto::red_db_server::{RedDb, RedDbServer};
use proto::{
    CollectionsReply, Empty, HealthReply, OperationReply, QueryReply, QueryRequest, ScanEntity,
    ScanReply, ScanRequest, StatsReply,
};

#[derive(Debug, Clone)]
pub struct GrpcServerOptions {
    pub bind_addr: String,
}

impl Default for GrpcServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_string(),
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
            }))
            .serve(addr)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
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
        Ok(Response::new(stats_reply(self.runtime.stats())))
    }

    async fn collections(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<CollectionsReply>, Status> {
        Ok(Response::new(CollectionsReply {
            collections: self.runtime.db().collections(),
        }))
    }

    async fn scan(&self, request: Request<ScanRequest>) -> Result<Response<ScanReply>, Status> {
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
        let result = self
            .runtime
            .execute_query(&request.into_inner().query)
            .map_err(to_status)?;
        Ok(Response::new(query_reply(result)))
    }

    async fn checkpoint(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<OperationReply>, Status> {
        self.runtime.checkpoint().map_err(to_status)?;
        Ok(Response::new(OperationReply {
            ok: true,
            message: "checkpoint completed".to_string(),
        }))
    }
}

fn to_status(err: crate::api::RedDBError) -> Status {
    Status::internal(err.to_string())
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
