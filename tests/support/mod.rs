use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reddb::application::{
    CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput,
    CreateTableColumnInput, CreateTableInput, CreateTimeSeriesInput, CreateTimeSeriesPointInput,
    CreateVectorInput, EntityUseCases, ExecuteQueryInput, NativeUseCases, QueryUseCases,
    SchemaUseCases,
};
use reddb::json::{from_slice as json_from_slice, json, Value as JsonValue};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::storage::{EntityData, EntityKind};
use reddb::{CatalogUseCases, HealthState, RedDBOptions, RedDBRuntime};

const ACCOUNTS_TTL_MS: u64 = 86_400_000;
const METRICS_RETENTION_MS: u64 = 7 * 86_400_000;
const FIVE_MINUTES_NS: u64 = 300_000_000_000;

#[derive(Debug)]
pub struct PersistentDbPath {
    base: PathBuf,
}

impl PersistentDbPath {
    pub fn new(prefix: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"));
        let path = Self { base };
        path.cleanup();
        path
    }

    pub fn open_runtime(&self) -> RedDBRuntime {
        let path = self.path_string();
        RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .unwrap_or_else(|err| panic!("failed to open persistent runtime at {path}: {err:?}"))
    }

    fn path_string(&self) -> String {
        self.base.to_string_lossy().to_string()
    }

    fn cleanup(&self) {
        if let Some(parent) = self.base.parent() {
            let stem = self
                .base
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if name == stem || name.starts_with(&format!("{stem}-")) {
                        let _ = std::fs::remove_file(&path);
                        let _ = std::fs::remove_dir_all(&path);
                    }
                }
            }
        }
    }
}

impl Drop for PersistentDbPath {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogicalSnapshot {
    pub accounts: Vec<BTreeMap<String, String>>,
    pub docs: Vec<String>,
    pub settings: Vec<(String, String)>,
    pub graph_nodes: Vec<GraphNodeSnapshot>,
    pub graph_edges: Vec<GraphEdgeSnapshot>,
    pub vectors: Vec<VectorSnapshot>,
    pub metrics: Vec<TimeSeriesPointSnapshot>,
    pub queue_messages: Vec<QueueMessageSnapshot>,
    pub queue_meta: Vec<BTreeMap<String, String>>,
    pub timeseries_meta: Vec<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeSnapshot {
    pub label: String,
    pub node_type: String,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdgeSnapshot {
    pub label: String,
    pub from_label: String,
    pub to_label: String,
    pub weight: f64,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorSnapshot {
    pub content: Option<String>,
    pub dense: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeSeriesPointSnapshot {
    pub metric: String,
    pub timestamp_ns: u64,
    pub value: f64,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueMessageSnapshot {
    pub queue: String,
    pub position: u64,
    pub payload: String,
    pub priority: Option<i32>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub acked: bool,
}

pub fn build_sql_fixture(rt: &RedDBRuntime) {
    let query = QueryUseCases::new(rt);

    exec(
        &query,
        "CREATE TABLE accounts (id TEXT PRIMARY KEY, username TEXT UNIQUE, status TEXT NOT NULL DEFAULT = 'active', tier TEXT DEFAULT = 'basic', score FLOAT) WITH TTL 24 h WITH CONTEXT INDEX ON (username, status) WITH timestamps = true",
    );
    exec(
        &query,
        "INSERT INTO accounts (id, username, status, score) VALUES ('u1', 'alice', 'active', 91.5)",
    );
    exec(
        &query,
        "INSERT INTO accounts (id, username, status, tier, score) VALUES ('u2', 'bob', 'suspended', 'enterprise', 72.0)",
    );
    exec(
        &query,
        "INSERT INTO accounts (id, username, status, tier, score) VALUES ('u3', 'carol', 'active', 'pro', 88.25)",
    );

    exec(
        &query,
        "INSERT INTO docs DOCUMENT (body) VALUES ('{\"category\":\"ops\",\"slug\":\"guide\",\"title\":\"Guide\"}')",
    );
    exec(
        &query,
        "INSERT INTO docs DOCUMENT (body) VALUES ('{\"category\":\"db\",\"slug\":\"runbook\",\"title\":\"Runbook\"}')",
    );

    exec(
        &query,
        "INSERT INTO settings KV (key, value) VALUES ('feature_flag', true)",
    );
    exec(
        &query,
        "INSERT INTO settings KV (key, value) VALUES ('max_retries', 3)",
    );

    exec(
        &query,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3",
    );
    exec(&query, "QUEUE GROUP CREATE tasks workers");
    exec(&query, "QUEUE PUSH tasks 'job-1'");
    exec(&query, "QUEUE PUSH tasks 'job-2'");

    exec(
        &query,
        "INSERT INTO network NODE (label, node_type, role) VALUES ('gateway', 'Host', 'gateway')",
    );
    exec(
        &query,
        "INSERT INTO network NODE (label, node_type, role) VALUES ('app', 'Host', 'application')",
    );
    exec(
        &query,
        "INSERT INTO network NODE (label, node_type, role) VALUES ('db', 'Host', 'database')",
    );

    let gateway = graph_node_id(rt, "network", "gateway");
    let app = graph_node_id(rt, "network", "app");
    let db = graph_node_id(rt, "network", "db");

    exec(
        &query,
        &format!(
            "INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', {gateway}, {app}, 1.0)"
        ),
    );
    exec(
        &query,
        &format!(
            "INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', {app}, {db}, 1.0)"
        ),
    );
    exec(
        &query,
        &format!(
            "INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', {gateway}, {db}, 5.0)"
        ),
    );

    exec(
        &query,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway guide')",
    );
    exec(
        &query,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual')",
    );
    exec(
        &query,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.5, 0.5], 'shared note')",
    );

    exec(
        &query,
        "CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg",
    );
    exec(
        &query,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{\"host\":\"srv-a\",\"region\":\"sa\"}', 0)",
    );
    exec(
        &query,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{\"host\":\"srv-a\",\"region\":\"sa\"}', 60000000000)",
    );
    exec(
        &query,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 30.0, '{\"host\":\"srv-b\",\"region\":\"sa\"}', 300000000000)",
    );

    exec(
        &query,
        "CREATE TABLE auth_accounts (username TEXT PRIMARY KEY, pw PASSWORD)",
    );
    exec(
        &query,
        "INSERT INTO auth_accounts (username, pw) VALUES ('alice', PASSWORD('MyP@ss123'))",
    );
}

pub fn build_api_fixture(rt: &RedDBRuntime) {
    let schema = SchemaUseCases::new(rt);
    let entity = EntityUseCases::new(rt);
    let query = QueryUseCases::new(rt);

    schema
        .create_table(CreateTableInput {
            name: "accounts".into(),
            columns: vec![
                CreateTableColumnInput {
                    name: "id".into(),
                    data_type: "TEXT".into(),
                    not_null: false,
                    default: None,
                    compress: None,
                    unique: false,
                    primary_key: true,
                    enum_variants: Vec::new(),
                    array_element: None,
                    decimal_precision: None,
                },
                CreateTableColumnInput {
                    name: "username".into(),
                    data_type: "TEXT".into(),
                    not_null: false,
                    default: None,
                    compress: None,
                    unique: true,
                    primary_key: false,
                    enum_variants: Vec::new(),
                    array_element: None,
                    decimal_precision: None,
                },
                CreateTableColumnInput {
                    name: "status".into(),
                    data_type: "TEXT".into(),
                    not_null: true,
                    default: Some("active".into()),
                    compress: None,
                    unique: false,
                    primary_key: false,
                    enum_variants: Vec::new(),
                    array_element: None,
                    decimal_precision: None,
                },
                CreateTableColumnInput {
                    name: "tier".into(),
                    data_type: "TEXT".into(),
                    not_null: false,
                    default: Some("basic".into()),
                    compress: None,
                    unique: false,
                    primary_key: false,
                    enum_variants: Vec::new(),
                    array_element: None,
                    decimal_precision: None,
                },
                CreateTableColumnInput {
                    name: "score".into(),
                    data_type: "FLOAT".into(),
                    not_null: false,
                    default: None,
                    compress: None,
                    unique: false,
                    primary_key: false,
                    enum_variants: Vec::new(),
                    array_element: None,
                    decimal_precision: None,
                },
            ],
            if_not_exists: false,
            default_ttl_ms: Some(ACCOUNTS_TTL_MS),
            context_index_fields: vec!["username".into(), "status".into()],
            timestamps: true,
            partition_by: None,
            tenant_by: None,
            append_only: false,
        })
        .expect("api create_table accounts should succeed");

    entity
        .create_row(CreateRowInput {
            collection: "accounts".into(),
            fields: vec![
                ("id".into(), Value::text("u1")),
                ("username".into(), Value::text("alice")),
                ("status".into(), Value::text("active")),
                ("score".into(), Value::Float(91.5)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("api row alice should succeed");
    entity
        .create_row(CreateRowInput {
            collection: "accounts".into(),
            fields: vec![
                ("id".into(), Value::text("u2")),
                ("username".into(), Value::text("bob")),
                ("status".into(), Value::text("suspended")),
                ("tier".into(), Value::text("enterprise")),
                ("score".into(), Value::Float(72.0)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("api row bob should succeed");
    entity
        .create_row(CreateRowInput {
            collection: "accounts".into(),
            fields: vec![
                ("id".into(), Value::text("u3")),
                ("username".into(), Value::text("carol")),
                ("status".into(), Value::text("active")),
                ("tier".into(), Value::text("pro")),
                ("score".into(), Value::Float(88.25)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("api row carol should succeed");

    entity
        .create_document(CreateDocumentInput {
            collection: "docs".into(),
            body: json!({
                "category": "ops",
                "slug": "guide",
                "title": "Guide"
            }),
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("api document guide should succeed");
    entity
        .create_document(CreateDocumentInput {
            collection: "docs".into(),
            body: json!({
                "category": "db",
                "slug": "runbook",
                "title": "Runbook"
            }),
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("api document runbook should succeed");

    entity
        .create_kv(CreateKvInput {
            collection: "settings".into(),
            key: "feature_flag".into(),
            value: Value::Boolean(true),
            metadata: vec![],
        })
        .expect("api kv feature_flag should succeed");
    entity
        .create_kv(CreateKvInput {
            collection: "settings".into(),
            key: "max_retries".into(),
            value: Value::Integer(3),
            metadata: vec![],
        })
        .expect("api kv max_retries should succeed");

    exec(
        &query,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3",
    );
    exec(&query, "QUEUE GROUP CREATE tasks workers");
    exec(&query, "QUEUE PUSH tasks 'job-1'");
    exec(&query, "QUEUE PUSH tasks 'job-2'");

    let gateway = entity
        .create_node(CreateNodeInput {
            collection: "network".into(),
            label: "gateway".into(),
            node_type: Some("Host".into()),
            properties: vec![("role".into(), Value::text("gateway"))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("api graph node gateway should succeed");
    let app = entity
        .create_node(CreateNodeInput {
            collection: "network".into(),
            label: "app".into(),
            node_type: Some("Host".into()),
            properties: vec![("role".into(), Value::text("application"))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("api graph node app should succeed");
    let db = entity
        .create_node(CreateNodeInput {
            collection: "network".into(),
            label: "db".into(),
            node_type: Some("Host".into()),
            properties: vec![("role".into(), Value::text("database"))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("api graph node db should succeed");

    entity
        .create_edge(CreateEdgeInput {
            collection: "network".into(),
            label: "connects".into(),
            from: gateway.id,
            to: app.id,
            weight: Some(1.0),
            properties: vec![],
            metadata: vec![],
        })
        .expect("api graph edge gateway->app should succeed");
    entity
        .create_edge(CreateEdgeInput {
            collection: "network".into(),
            label: "connects".into(),
            from: app.id,
            to: db.id,
            weight: Some(1.0),
            properties: vec![],
            metadata: vec![],
        })
        .expect("api graph edge app->db should succeed");
    entity
        .create_edge(CreateEdgeInput {
            collection: "network".into(),
            label: "connects".into(),
            from: gateway.id,
            to: db.id,
            weight: Some(5.0),
            properties: vec![],
            metadata: vec![],
        })
        .expect("api graph edge gateway->db should succeed");

    entity
        .create_vector(CreateVectorInput {
            collection: "embeddings".into(),
            dense: vec![1.0, 0.0],
            content: Some("gateway guide".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("api vector gateway should succeed");
    entity
        .create_vector(CreateVectorInput {
            collection: "embeddings".into(),
            dense: vec![0.0, 1.0],
            content: Some("database manual".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("api vector database should succeed");
    entity
        .create_vector(CreateVectorInput {
            collection: "embeddings".into(),
            dense: vec![0.5, 0.5],
            content: Some("shared note".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("api vector shared should succeed");

    schema
        .create_timeseries(CreateTimeSeriesInput {
            name: "metrics".into(),
            retention_ms: Some(METRICS_RETENTION_MS),
            chunk_size: Some(64),
            downsample_policies: vec!["1h:5m:avg".into()],
            if_not_exists: false,
        })
        .expect("api create_timeseries should succeed");

    entity
        .create_timeseries_point(CreateTimeSeriesPointInput {
            collection: "metrics".into(),
            metric: "cpu.usage".into(),
            value: 10.0,
            timestamp_ns: Some(0),
            tags: vec![
                ("host".into(), "srv-a".into()),
                ("region".into(), "sa".into()),
            ],
            metadata: vec![],
        })
        .expect("api timeseries point 1 should succeed");
    entity
        .create_timeseries_point(CreateTimeSeriesPointInput {
            collection: "metrics".into(),
            metric: "cpu.usage".into(),
            value: 20.0,
            timestamp_ns: Some(60_000_000_000),
            tags: vec![
                ("host".into(), "srv-a".into()),
                ("region".into(), "sa".into()),
            ],
            metadata: vec![],
        })
        .expect("api timeseries point 2 should succeed");
    entity
        .create_timeseries_point(CreateTimeSeriesPointInput {
            collection: "metrics".into(),
            metric: "cpu.usage".into(),
            value: 30.0,
            timestamp_ns: Some(FIVE_MINUTES_NS),
            tags: vec![
                ("host".into(), "srv-b".into()),
                ("region".into(), "sa".into()),
            ],
            metadata: vec![],
        })
        .expect("api timeseries point 3 should succeed");
}

pub fn checkpoint_and_reopen(path: &PersistentDbPath, rt: RedDBRuntime) -> RedDBRuntime {
    let native = NativeUseCases::new(&rt);
    native
        .create_snapshot()
        .expect("create_snapshot should succeed before reopen");
    native
        .checkpoint()
        .expect("checkpoint should succeed before reopen");
    drop(rt);
    std::thread::sleep(Duration::from_millis(50));
    path.open_runtime()
}

pub fn logical_snapshot(rt: &RedDBRuntime) -> LogicalSnapshot {
    if std::env::var_os("REDDB_DEBUG_PERSISTENT").is_some() {
        eprintln!("store.format_version={}", rt.db().store().format_version());
        for collection in ["embeddings", "metrics"] {
            let entities = collection_entities(rt, collection);
            eprintln!("collection={collection} entity_count={}", entities.len());
            for entity in entities {
                eprintln!("  kind={:?} data={:?}", entity.kind, entity.data);
                let from_store = rt.db().store().get(collection, entity.id);
                eprintln!("  btree={from_store:?}");
            }
        }
    }

    LogicalSnapshot {
        accounts: normalized_row_collection(rt, "accounts", &["created_at", "updated_at"]),
        docs: normalized_document_collection(rt, "docs"),
        settings: normalized_kv_collection(rt, "settings"),
        graph_nodes: normalized_graph_nodes(rt, "network"),
        graph_edges: normalized_graph_edges(rt, "network"),
        vectors: normalized_vector_collection(rt, "embeddings"),
        metrics: normalized_timeseries_collection(rt, "metrics"),
        queue_messages: normalized_queue_collection(rt, "tasks"),
        queue_meta: normalized_row_collection(rt, "red_queue_meta", &["created_at_ns"]),
        timeseries_meta: normalized_row_collection(rt, "red_timeseries_meta", &[]),
    }
}

pub fn assert_shared_query_behavior(rt: &RedDBRuntime) {
    let query = QueryUseCases::new(rt);

    let accounts = exec(
        &query,
        "SELECT username, tier FROM accounts WHERE status = 'active' ORDER BY username ASC",
    );
    let rows: Vec<(String, String)> = accounts
        .result
        .records
        .iter()
        .map(|record| (text(record, "username"), text(record, "tier")))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("alice".to_string(), "basic".to_string()),
            ("carol".to_string(), "pro".to_string()),
        ]
    );

    let paged = exec(
        &query,
        "SELECT username FROM accounts ORDER BY username ASC LIMIT 1 OFFSET 1",
    );
    assert_eq!(text(&paged.result.records[0], "username"), "bob");

    let docs = exec(
        &query,
        "SELECT slug, title FROM docs WHERE slug = 'guide' ORDER BY slug ASC",
    );
    assert_eq!(docs.result.records.len(), 1);
    assert_eq!(text(&docs.result.records[0], "title"), "Guide");

    let settings = exec(&query, "SELECT key, value FROM settings ORDER BY key ASC");
    let settings_rows: Vec<(String, String)> = settings
        .result
        .records
        .iter()
        .map(|record| {
            (
                text(record, "key"),
                value_repr(record.get("value").unwrap()),
            )
        })
        .collect();
    assert_eq!(
        settings_rows,
        vec![
            ("feature_flag".to_string(), "true".to_string()),
            ("max_retries".to_string(), "3".to_string()),
        ]
    );

    let queue_len = exec(&query, "QUEUE LEN tasks");
    assert_eq!(uint(&queue_len.result.records[0], "len"), 2);

    let queue_peek = exec(&query, "QUEUE PEEK tasks");
    let payloads = queue_peek
        .result
        .records
        .iter()
        .map(|record| text(record, "payload"))
        .collect::<Vec<_>>();
    assert_eq!(payloads, vec!["job-1".to_string()]);

    let buckets = exec(
        &query,
        "SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)",
    );
    let mut rows: Vec<(u64, f64, u64)> = buckets
        .result
        .records
        .iter()
        .map(|record| {
            (
                uint(record, "bucket"),
                float(record, "avg_value"),
                uint(record, "samples"),
            )
        })
        .collect();
    rows.sort_by_key(|(bucket, _, _)| *bucket);
    assert_eq!(rows, vec![(0, 15.0, 2), (FIVE_MINUTES_NS, 30.0, 1)]);

    let gateway = graph_node_id(rt, "network", "gateway");
    let db = graph_node_id(rt, "network", "db");

    let shortest = exec(
        &query,
        &format!(
            "GRAPH SHORTEST_PATH '{}' TO '{}' ALGORITHM dijkstra",
            gateway, db
        ),
    );
    assert_eq!(uint(&shortest.result.records[0], "hop_count"), 2);
    assert_eq!(float(&shortest.result.records[0], "total_weight"), 2.0);

    let neighborhood = exec(&query, &format!("GRAPH NEIGHBORHOOD '{}' DEPTH 2", gateway));
    let mut seen = neighborhood
        .result
        .records
        .iter()
        .map(|record| text(record, "label"))
        .collect::<Vec<_>>();
    seen.sort();
    assert!(seen.contains(&"app".to_string()));
    assert!(seen.contains(&"db".to_string()));

    let vectors = exec(
        &query,
        "VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1",
    );
    assert_eq!(vectors.result.records.len(), 1);
    assert_eq!(text(&vectors.result.records[0], "content"), "gateway guide");

    let meta = exec(
        &query,
        "SELECT series, retention_ms, chunk_size FROM red_timeseries_meta WHERE series = 'metrics'",
    );
    assert_eq!(meta.result.records.len(), 1);
    assert_eq!(text(&meta.result.records[0], "series"), "metrics");
    assert_eq!(
        uint(&meta.result.records[0], "retention_ms"),
        METRICS_RETENTION_MS
    );
    assert_eq!(uint(&meta.result.records[0], "chunk_size"), 64);
}

pub fn apply_end_to_end_mutations(rt: &RedDBRuntime) {
    let query = QueryUseCases::new(rt);

    exec(
        &query,
        "UPDATE accounts SET tier = 'team' WHERE username = 'alice'",
    );
    exec(
        &query,
        "UPDATE accounts SET status = 'active', tier = 'enterprise', score = 80.0 WHERE username = 'bob'",
    );
    exec(&query, "DELETE FROM accounts WHERE username = 'carol'");
    exec(
        &query,
        "INSERT INTO accounts (id, username, status, tier, score) VALUES ('u4', 'dora', 'suspended', 'basic', 66.0)",
    );

    exec(&query, "DELETE FROM docs WHERE slug = 'runbook'");
    exec(
        &query,
        "INSERT INTO docs DOCUMENT (body) VALUES ('{\"category\":\"ops\",\"slug\":\"faq\",\"title\":\"FAQ\"}')",
    );

    exec(&query, "DELETE FROM settings WHERE key = 'feature_flag'");
    exec(
        &query,
        "INSERT INTO settings KV (key, value) VALUES ('feature_flag', false)",
    );
    exec(&query, "DELETE FROM settings WHERE key = 'max_retries'");
    exec(
        &query,
        "INSERT INTO settings KV (key, value) VALUES ('max_retries', 5)",
    );

    exec(
        &query,
        "CREATE QUEUE workflow WITH DLQ workflow_failed MAX_ATTEMPTS 3",
    );
    exec(&query, "QUEUE GROUP CREATE workflow workers");
    exec(&query, "QUEUE PUSH workflow 'task-a'");

    let delivered = exec(
        &query,
        "QUEUE READ workflow GROUP workers CONSUMER worker1 COUNT 1",
    );
    let message_id = text(&delivered.result.records[0], "message_id");
    assert_eq!(
        text(&delivered.result.records[0], "payload"),
        "task-a",
        "workflow read should deliver the inserted payload"
    );
    assert_eq!(
        text(&delivered.result.records[0], "consumer"),
        "worker1",
        "workflow read should bind the first consumer"
    );

    let claimed = exec(
        &query,
        "QUEUE CLAIM workflow GROUP workers CONSUMER worker2 MIN_IDLE 0",
    );
    assert_eq!(
        text(&claimed.result.records[0], "message_id"),
        message_id,
        "workflow claim should keep the same message id"
    );
    assert_eq!(
        text(&claimed.result.records[0], "consumer"),
        "worker2",
        "workflow claim should transfer the pending message"
    );
    exec(
        &query,
        &format!("QUEUE ACK workflow GROUP workers '{message_id}'"),
    );

    exec(
        &query,
        "INSERT INTO network NODE (label, node_type, role) VALUES ('cache', 'Host', 'cache')",
    );
    let app = graph_node_id(rt, "network", "app");
    let cache = graph_node_id(rt, "network", "cache");
    exec(
        &query,
        &format!(
            "INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', {app}, {cache}, 0.5)"
        ),
    );

    exec(
        &query,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.9, 0.1], 'gateway followup')",
    );

    exec(
        &query,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 50.0, '{\"host\":\"srv-a\",\"region\":\"sa\"}', 600000000000)",
    );
    exec(
        &query,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('mem.usage', 70.0, '{\"host\":\"srv-a\",\"region\":\"sa\"}', 0)",
    );
}

pub fn assert_end_to_end_query_behavior(rt: &RedDBRuntime) {
    let query = QueryUseCases::new(rt);

    let active_high_score = exec(
        &query,
        "SELECT username FROM accounts WHERE status = 'active' AND score >= 80 ORDER BY username ASC",
    );
    let active_names = active_high_score
        .result
        .records
        .iter()
        .map(|record| text(record, "username"))
        .collect::<Vec<_>>();
    assert_eq!(
        active_names,
        vec!["alice".to_string(), "bob".to_string()],
        "post-mutation filtered account query should match expected rows"
    );

    let accounts = exec(
        &query,
        "SELECT username, status, tier FROM accounts ORDER BY username ASC",
    );
    let account_rows = accounts
        .result
        .records
        .iter()
        .map(|record| {
            (
                text(record, "username"),
                text(record, "status"),
                text(record, "tier"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        account_rows,
        vec![
            (
                "alice".to_string(),
                "active".to_string(),
                "team".to_string(),
            ),
            (
                "bob".to_string(),
                "active".to_string(),
                "enterprise".to_string(),
            ),
            (
                "dora".to_string(),
                "suspended".to_string(),
                "basic".to_string(),
            ),
        ],
        "account rows should reflect insert/update/delete mutations"
    );

    let grouped_accounts = exec(
        &query,
        "SELECT status, count(*) AS total FROM accounts GROUP BY status ORDER BY status ASC",
    );
    let grouped_rows = grouped_accounts
        .result
        .records
        .iter()
        .map(|record| (text(record, "status"), uint(record, "total")))
        .collect::<Vec<_>>();
    assert_eq!(
        grouped_rows,
        vec![("active".to_string(), 2), ("suspended".to_string(), 1)],
        "GROUP BY over accounts should reflect the final state"
    );

    let docs = exec(&query, "SELECT slug, title FROM docs ORDER BY slug ASC");
    let doc_rows = docs
        .result
        .records
        .iter()
        .map(|record| (text(record, "slug"), text(record, "title")))
        .collect::<Vec<_>>();
    assert_eq!(
        doc_rows,
        vec![
            ("faq".to_string(), "FAQ".to_string()),
            ("guide".to_string(), "Guide".to_string()),
        ],
        "document collection should reflect delete + insert mutations"
    );

    let settings = exec(&query, "SELECT key, value FROM settings ORDER BY key ASC");
    let setting_rows = settings
        .result
        .records
        .iter()
        .map(|record| {
            (
                text(record, "key"),
                value_repr(record.get("value").expect("setting should have value")),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        setting_rows,
        vec![
            ("feature_flag".to_string(), "false".to_string()),
            ("max_retries".to_string(), "5".to_string()),
        ],
        "KV collection should reflect the final values"
    );

    let queue_len = exec(&query, "QUEUE LEN workflow");
    assert_eq!(
        uint(&queue_len.result.records[0], "len"),
        0,
        "workflow queue should be empty after the claimed message is acked"
    );

    let queue_pending = exec(&query, "QUEUE PENDING workflow GROUP workers");
    assert!(
        queue_pending.result.records.is_empty(),
        "workflow pending list should be empty after ack"
    );

    let metrics = exec(
        &query,
        "SELECT metric, time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples FROM metrics GROUP BY metric, time_bucket(5m) ORDER BY metric ASC, bucket ASC",
    );
    let metric_rows = metrics
        .result
        .records
        .iter()
        .map(|record| {
            (
                text(record, "metric"),
                uint(record, "bucket"),
                float(record, "avg_value"),
                uint(record, "samples"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        metric_rows,
        vec![
            ("cpu.usage".to_string(), 0, 15.0, 2),
            ("cpu.usage".to_string(), FIVE_MINUTES_NS, 30.0, 1),
            ("cpu.usage".to_string(), 600_000_000_000, 50.0, 1),
            ("mem.usage".to_string(), 0, 70.0, 1),
        ],
        "time-series aggregation should reflect inserted points"
    );

    let gateway = graph_node_id(rt, "network", "gateway");
    let app = graph_node_id(rt, "network", "app");
    let cache = graph_node_id(rt, "network", "cache");

    let shortest = exec(
        &query,
        &format!(
            "GRAPH SHORTEST_PATH '{}' TO '{}' ALGORITHM dijkstra",
            gateway, cache
        ),
    );
    assert_eq!(
        uint(&shortest.result.records[0], "hop_count"),
        2,
        "gateway -> cache should resolve through the app node"
    );
    assert_eq!(
        float(&shortest.result.records[0], "total_weight"),
        1.5,
        "gateway -> app -> cache should weigh 1.5 total"
    );

    let neighborhood = exec(&query, &format!("GRAPH NEIGHBORHOOD '{}' DEPTH 1", app));
    let mut seen = neighborhood
        .result
        .records
        .iter()
        .map(|record| text(record, "label"))
        .collect::<Vec<_>>();
    seen.sort();
    assert!(
        seen.contains(&"cache".to_string()),
        "graph neighborhood should include the inserted cache node"
    );
    assert!(
        seen.contains(&"db".to_string()),
        "graph neighborhood should still include the original db node"
    );

    let vectors = exec(
        &query,
        "VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 2",
    );
    assert_eq!(
        vectors.result.records.len(),
        2,
        "vector search should return the top two matches"
    );
    let vector_contents = vectors
        .result
        .records
        .iter()
        .map(|record| text(record, "content"))
        .collect::<Vec<_>>();
    assert_eq!(
        vector_contents.first().map(String::as_str),
        Some("gateway guide"),
        "the exact vector should remain the top result"
    );
    assert!(
        vector_contents.contains(&"gateway followup".to_string()),
        "the inserted near-neighbor vector should rank in the top two"
    );
}

pub fn assert_sql_function_queries(rt: &RedDBRuntime) {
    let query = QueryUseCases::new(rt);

    let ok = exec(
        &query,
        "SELECT VERIFY_PASSWORD(pw, 'MyP@ss123') AS ok FROM auth_accounts WHERE username = 'alice'",
    );
    assert!(boolean(&ok.result.records[0], "ok"));

    let bad = exec(
        &query,
        "SELECT VERIFY_PASSWORD(pw, 'wrong') AS ok FROM auth_accounts WHERE username = 'alice'",
    );
    assert!(!boolean(&bad.result.records[0], "ok"));
}

pub fn assert_native_consistency(rt: &RedDBRuntime) {
    let native = NativeUseCases::new(rt);
    let catalog = CatalogUseCases::new(rt);

    let health = native.health();
    assert!(
        health.state != HealthState::Unhealthy,
        "runtime health should not be unhealthy after reopen: {:?}",
        health
    );
    assert_eq!(
        health
            .diagnostics
            .get("native_bootstrap.ready")
            .map(String::as_str),
        Some("true"),
        "native bootstrap should be ready after reopen"
    );
    assert_eq!(
        health
            .diagnostics
            .get("native_header.matches_metadata")
            .map(String::as_str),
        Some("true"),
        "native header should match physical metadata after reopen"
    );
    assert_eq!(
        health
            .diagnostics
            .get("readiness_for_write")
            .map(String::as_str),
        Some("true"),
        "runtime should remain writable after reopen"
    );
    assert_eq!(
        health
            .diagnostics
            .get("readiness_for_repair")
            .map(String::as_str),
        Some("true"),
        "runtime should remain repairable after reopen"
    );

    let report = catalog.consistency_report();
    assert!(report.missing_operational_indexes.is_empty());
    assert!(report.undeclared_operational_indexes.is_empty());
    assert!(report.missing_operational_graph_projections.is_empty());
    assert!(report.undeclared_operational_graph_projections.is_empty());
    assert!(report.missing_operational_analytics_jobs.is_empty());
    assert!(report.undeclared_operational_analytics_jobs.is_empty());

    let physical = native
        .physical_metadata()
        .expect("physical metadata should be readable");
    assert!(
        physical
            .collection_contracts
            .iter()
            .any(|contract| contract.name == "accounts"),
        "physical metadata should include accounts contract"
    );
    assert!(
        physical
            .collection_contracts
            .iter()
            .any(|contract| contract.name == "metrics"),
        "physical metadata should include metrics contract"
    );
    assert_eq!(
        physical.collection_ttl_defaults_ms.get("accounts").copied(),
        Some(ACCOUNTS_TTL_MS)
    );
    assert_eq!(
        physical.collection_ttl_defaults_ms.get("metrics").copied(),
        Some(METRICS_RETENTION_MS)
    );
    assert!(
        !physical.snapshots.is_empty(),
        "physical metadata should retain at least one snapshot"
    );
    assert!(
        !physical.manifest_events.is_empty(),
        "physical metadata should retain manifest events"
    );

    let snapshots = native
        .snapshots()
        .expect("native snapshots should be readable");
    assert!(
        !snapshots.is_empty(),
        "native snapshots should be present after create_snapshot + checkpoint"
    );

    let native_state = native
        .native_physical_state()
        .expect("native physical state should be readable");
    assert!(
        !native_state.collection_roots.is_empty(),
        "native physical state should expose persisted collection roots"
    );
    assert!(
        native_state.header.sequence > 0,
        "native physical header sequence should advance after checkpoint"
    );
}

fn exec(query: &QueryUseCases<'_, RedDBRuntime>, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    query
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn graph_node_id(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    collection_entities(rt, collection)
        .into_iter()
        .find_map(|entity| match &entity.kind {
            EntityKind::GraphNode(node) if node.label == label => Some(entity.id.raw().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("graph node '{label}' not found in collection '{collection}'"))
}

fn collection_entities(rt: &RedDBRuntime, collection: &str) -> Vec<reddb::storage::UnifiedEntity> {
    rt.db()
        .store()
        .get_collection(collection)
        .unwrap_or_else(|| panic!("collection '{collection}' should exist"))
        .query_all(|_| true)
}

fn row_fields(entity: &reddb::storage::UnifiedEntity) -> BTreeMap<String, Value> {
    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = &row.named {
                named
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            } else {
                row.schema
                    .as_ref()
                    .expect("row schema should exist for columnar rows")
                    .iter()
                    .cloned()
                    .zip(row.columns.iter().cloned())
                    .collect()
            }
        }
        other => panic!("expected row entity, got {other:?}"),
    }
}

fn normalized_row_collection(
    rt: &RedDBRuntime,
    collection: &str,
    skip_fields: &[&str],
) -> Vec<BTreeMap<String, String>> {
    let mut rows = collection_entities(rt, collection)
        .into_iter()
        .map(|entity| {
            let mut fields = row_fields(&entity);
            for key in skip_fields {
                fields.remove(*key);
            }
            fields
                .into_iter()
                .map(|(key, value)| (key, value_repr(&value)))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| {
        row.get("id")
            .cloned()
            .or_else(|| row.get("key").cloned())
            .or_else(|| row.get("series").cloned())
            .unwrap_or_else(|| format!("{row:?}"))
    });
    rows
}

fn normalized_document_collection(rt: &RedDBRuntime, collection: &str) -> Vec<String> {
    let mut docs = collection_entities(rt, collection)
        .into_iter()
        .map(|entity| {
            let fields = row_fields(&entity);
            let body = fields
                .get("body")
                .unwrap_or_else(|| panic!("document in '{collection}' should have body"));
            match body {
                Value::Json(bytes) => canonical_json_bytes(bytes),
                other => panic!("document body should be JSON, got {other:?}"),
            }
        })
        .collect::<Vec<_>>();
    docs.sort();
    docs
}

fn normalized_kv_collection(rt: &RedDBRuntime, collection: &str) -> Vec<(String, String)> {
    let mut items = collection_entities(rt, collection)
        .into_iter()
        .map(|entity| {
            let fields = row_fields(&entity);
            let key = match fields.get("key") {
                Some(Value::Text(value)) => value.to_string(),
                other => panic!("kv key should be text, got {other:?}"),
            };
            let value = value_repr(
                fields
                    .get("value")
                    .unwrap_or_else(|| panic!("kv row in '{collection}' missing value")),
            );
            (key, value)
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.0.cmp(&right.0));
    items
}

fn normalized_graph_nodes(rt: &RedDBRuntime, collection: &str) -> Vec<GraphNodeSnapshot> {
    let mut nodes = collection_entities(rt, collection)
        .into_iter()
        .filter_map(|entity| match (&entity.kind, &entity.data) {
            (EntityKind::GraphNode(kind), EntityData::Node(node)) => Some(GraphNodeSnapshot {
                label: kind.label.clone(),
                node_type: kind.node_type.clone(),
                properties: node
                    .properties
                    .iter()
                    .map(|(key, value)| (key.clone(), value_repr(value)))
                    .collect(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| left.label.cmp(&right.label));
    nodes
}

fn normalized_graph_edges(rt: &RedDBRuntime, collection: &str) -> Vec<GraphEdgeSnapshot> {
    let entities = collection_entities(rt, collection);
    let label_by_id: BTreeMap<String, String> = entities
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::GraphNode(kind) => Some((entity.id.raw().to_string(), kind.label.clone())),
            _ => None,
        })
        .collect();

    let mut edges = entities
        .into_iter()
        .filter_map(|entity| match (&entity.kind, &entity.data) {
            (EntityKind::GraphEdge(kind), EntityData::Edge(edge)) => Some(GraphEdgeSnapshot {
                label: kind.label.clone(),
                from_label: label_by_id
                    .get(&kind.from_node)
                    .cloned()
                    .unwrap_or_else(|| kind.from_node.clone()),
                to_label: label_by_id
                    .get(&kind.to_node)
                    .cloned()
                    .unwrap_or_else(|| kind.to_node.clone()),
                weight: edge.weight as f64,
                properties: edge
                    .properties
                    .iter()
                    .map(|(key, value)| (key.clone(), value_repr(value)))
                    .collect(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        (
            left.from_label.as_str(),
            left.to_label.as_str(),
            left.label.as_str(),
        )
            .cmp(&(
                right.from_label.as_str(),
                right.to_label.as_str(),
                right.label.as_str(),
            ))
    });
    edges
}

fn normalized_vector_collection(rt: &RedDBRuntime, collection: &str) -> Vec<VectorSnapshot> {
    let mut vectors = collection_entities(rt, collection)
        .into_iter()
        .filter_map(|entity| match entity.data {
            EntityData::Vector(vector) => Some(VectorSnapshot {
                content: vector.content,
                dense: vector.dense,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    vectors.sort_by(|left, right| left.content.cmp(&right.content));
    vectors
}

fn normalized_timeseries_collection(
    rt: &RedDBRuntime,
    collection: &str,
) -> Vec<TimeSeriesPointSnapshot> {
    let mut points = collection_entities(rt, collection)
        .into_iter()
        .filter_map(|entity| match entity.data {
            EntityData::TimeSeries(point) => Some(TimeSeriesPointSnapshot {
                metric: point.metric,
                timestamp_ns: point.timestamp_ns,
                value: point.value,
                tags: point.tags.into_iter().collect(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    points.sort_by(|left, right| {
        (left.metric.as_str(), left.timestamp_ns).cmp(&(right.metric.as_str(), right.timestamp_ns))
    });
    points
}

fn normalized_queue_collection(rt: &RedDBRuntime, collection: &str) -> Vec<QueueMessageSnapshot> {
    let mut messages = collection_entities(rt, collection)
        .into_iter()
        .filter_map(|entity| match (entity.kind, entity.data) {
            (EntityKind::QueueMessage { queue, position }, EntityData::QueueMessage(message)) => {
                Some(QueueMessageSnapshot {
                    queue,
                    position,
                    payload: value_repr(&message.payload),
                    priority: message.priority,
                    attempts: message.attempts,
                    max_attempts: message.max_attempts,
                    acked: message.acked,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    messages.sort_by_key(|message| (message.queue.clone(), message.position));
    messages
}

fn canonical_json_bytes(bytes: &[u8]) -> String {
    json_from_slice::<JsonValue>(bytes)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).to_string())
}

fn value_repr(value: &Value) -> String {
    match value {
        Value::Array(values) => {
            let rendered = values.iter().map(value_repr).collect::<Vec<_>>().join(", ");
            format!("[{rendered}]")
        }
        Value::Json(bytes) => canonical_json_bytes(bytes),
        Value::Password(_) => "<password>".to_string(),
        Value::Secret(_) => "<secret>".to_string(),
        _ => value.to_string(),
    }
}

fn text(record: &UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        Some(Value::UnsignedInteger(value)) => value.to_string(),
        Some(Value::Integer(value)) => value.to_string(),
        other => panic!("expected text-like value for {column}, got {other:?}"),
    }
}

fn uint(record: &UnifiedRecord, column: &str) -> u64 {
    match record.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer for {column}, got {other:?}"),
    }
}

fn float(record: &UnifiedRecord, column: &str) -> f64 {
    match record.get(column) {
        Some(Value::Float(value)) => *value,
        Some(Value::Integer(value)) => *value as f64,
        Some(Value::UnsignedInteger(value)) => *value as f64,
        other => panic!("expected numeric value for {column}, got {other:?}"),
    }
}

fn boolean(record: &UnifiedRecord, column: &str) -> bool {
    match record.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected boolean for {column}, got {other:?}"),
    }
}

#[allow(dead_code)]
fn _exists(path: &Path) -> bool {
    path.exists()
}
