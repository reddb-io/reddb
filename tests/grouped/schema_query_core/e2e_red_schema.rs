//! Runtime-backed virtual `red.*` schema tables.

use reddb::auth::policies::{EvalContext, Policy};
use reddb::auth::registry::{ConfigRegistryDraft, EvidenceRequirement, Mutability, Sensitivity};
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_auth_identity, set_current_connection_id, set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::storage::StorageDeployPreset;
use reddb::{RedDBOptions, RedDBRuntime};

const INDEX_COLUMNS: [&str; 10] = [
    "collection",
    "name",
    "kind",
    "declared",
    "operational",
    "enabled",
    "build_state",
    "in_sync",
    "queryable",
    "requires_rebuild",
];

const SHOW_INDEX_COLUMNS: [&str; 6] = [
    "name",
    "table",
    "columns",
    "kind",
    "unique",
    "entries_indexed",
];

const POLICY_COLUMNS: [&str; 8] = [
    "name",
    "collection",
    "kind",
    "effect",
    "actions",
    "principals",
    "predicate",
    "enabled",
];

// Issue #1787 — `red.stats` is the long-format computed profiling view.
const STATS_COLUMNS: [&str; 4] = ["collection", "entity", "metric", "value"];

const REGISTRY_COLUMNS: [&str; 12] = [
    "id",
    "version",
    "resource_type",
    "schema",
    "mutability",
    "sensitivity",
    "managed",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
];

const REGISTRY_HISTORY_COLUMNS: [&str; 15] = [
    "id",
    "version",
    "resource_type",
    "schema",
    "mutability",
    "sensitivity",
    "managed",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
    "superseded_by",
    "superseded_at",
    "change_reason",
];

const MANAGED_POLICY_COLUMNS: [&str; 9] = [
    "policy_id",
    "registry_id",
    "version",
    "schema",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
];

const CONTROL_EVENT_COLUMNS: [&str; 14] = [
    "id",
    "ts",
    "kind",
    "outcome",
    "actor_kind",
    "actor_user_id",
    "scope",
    "action",
    "resource",
    "reason",
    "matched_policy_id",
    "request_id",
    "trace_id",
    "fields_json",
];

const USER_EVIDENCE_COLUMNS: [&str; 7] = [
    "username",
    "tenant_id",
    "role",
    "enabled",
    "created_at",
    "updated_at",
    "api_key_count",
];

const API_KEY_EVIDENCE_COLUMNS: [&str; 6] = [
    "owner",
    "tenant_id",
    "name",
    "role",
    "created_at",
    "key_fingerprint",
];

const CONTROL_CAPABILITY_COLUMNS: [&str; 4] = ["action", "resource_kind", "scope", "description"];

const POLICY_ACTION_COLUMNS: [&str; 6] = [
    "name",
    "category",
    "lifecycle_state",
    "replacement",
    "since_version",
    "gates_description",
];

const FORK_COLUMNS: [&str; 8] = [
    "name",
    "parent_store",
    "fork_lsn",
    "hydration_state",
    "collections_total",
    "shared_by_reference",
    "hydrating",
    "hydrated",
];

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn text<'a>(row: &'a reddb::storage::query::unified::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn bool_field(row: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> bool {
    match row.get(field) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected {field} bool, got {other:?} in {row:?}"),
    }
}

fn cleanup_scope() {
    clear_current_auth_identity();
    clear_current_tenant();
    clear_current_connection_id();
}

fn query_snapshot(rt: &RedDBRuntime, sql: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result;
    let rows = result
        .records
        .iter()
        .map(|record| {
            result
                .columns
                .iter()
                .map(|column| record.get(column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    (result.columns, rows)
}

fn stat_value(rt: &RedDBRuntime, collection: &str, entity: Value, metric: &str) -> Value {
    let (_, rows) = query_snapshot(
        rt,
        &format!(
            "SELECT * FROM red.stats WHERE collection = '{collection}' AND metric = '{metric}'"
        ),
    );
    rows.iter().find(|row| row[1] == entity).unwrap_or_else(|| {
        panic!("missing stat collection={collection} entity={entity:?} metric={metric}: {rows:?}")
    })[3]
        .clone()
}

fn seed_stable_introspection_fixture(rt: &RedDBRuntime) {
    exec(
        rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE, active BOOLEAN NOT NULL)",
    );
    exec(rt, "CREATE TABLE projects (id INT, owner TEXT)");
    exec(
        rt,
        "CREATE INDEX users_email_idx ON users (email) USING HASH",
    );
    exec(
        rt,
        "INSERT INTO users (id, email, active) VALUES (1, 'a@example.com', true)",
    );
    exec(rt, "INSERT INTO projects (id, owner) VALUES (1, 'alice')");
    exec(
        rt,
        "CREATE POLICY active_users ON users FOR SELECT TO reader USING (active = true)",
    );
    exec(rt, "ALTER TABLE users ENABLE ROW LEVEL SECURITY");
}

fn registry_admin_ctx() -> EvalContext {
    EvalContext {
        principal_tenant: None,
        current_tenant: None,
        peer_ip: None,
        mfa_present: false,
        now_ms: 1_700_000_000_000,
        principal_is_admin_role: true,
        principal_is_platform_scoped: true,
    }
}

fn registry_policy(id: &str) -> Policy {
    Policy::from_json_str(&format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [{{
                "effect": "allow",
                "actions": ["red.registry:*"],
                "resources": ["registry:*"]
            }}]
        }}"#
    ))
    .unwrap()
}

fn table_read_policy(id: &str) -> Policy {
    Policy::from_json_str(&format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [{{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:*"]
            }}]
        }}"#
    ))
    .unwrap()
}

fn registry_draft(id: &str, schema: &str, managed: bool) -> ConfigRegistryDraft {
    ConfigRegistryDraft {
        id: id.to_string(),
        resource_type: "policy".to_string(),
        schema: schema.to_string(),
        mutability: Mutability::MutableViaGovernance,
        sensitivity: Sensitivity::Internal,
        managed,
        required_action: "policy:put".to_string(),
        required_resource: format!("policy:{id}"),
        evidence_requirement: EvidenceRequirement::Metadata,
    }
}

fn text_field<'a>(
    record: &'a reddb::storage::query::unified::UnifiedRecord,
    field: &str,
) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text field {field}, got {other:?}"),
    }
}

fn text_array_field(
    record: &reddb::storage::query::unified::UnifiedRecord,
    field: &str,
) -> Vec<String> {
    match record.get(field) {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::Text(text) => text.to_string(),
                other => panic!("expected text array item in {field}, got {other:?}"),
            })
            .collect(),
        other => panic!("expected array field {field}, got {other:?}"),
    }
}

fn uint_field(record: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        other => panic!("expected unsigned integer field {field}, got {other:?}"),
    }
}

#[test]
fn red_forks_surfaces_parent_lsn_and_hydration_progress() {
    cleanup_scope();
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("forks.rdb");
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("persistent runtime");

    let manifest = reddb_file::OperationalManifest::for_db_path(&db_path);
    manifest
        .recover_or_bootstrap(&["users".to_string(), "orders".to_string()])
        .expect("bootstrap operational manifest");
    manifest
        .create_fork("migration-test", 42)
        .expect("create fork");

    let result = rt
        .execute_query("SELECT * FROM red.forks")
        .expect("red.forks select");
    assert_eq!(result.result.columns, FORK_COLUMNS.map(str::to_string));
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(text_field(row, "name"), "migration-test");
    assert_eq!(text_field(row, "parent_store"), manifest.store_identity());
    assert_eq!(uint_field(row, "fork_lsn"), 42);
    assert_eq!(text_field(row, "hydration_state"), "shared_by_reference");
    assert_eq!(uint_field(row, "collections_total"), 2);
    assert_eq!(uint_field(row, "shared_by_reference"), 2);
    assert_eq!(uint_field(row, "hydrating"), 0);
    assert_eq!(uint_field(row, "hydrated"), 0);

    let fork = manifest.fork_handle("migration-test");
    std::fs::write(fork.collection_path_for_test("users"), b"partial copy")
        .expect("simulate in-progress hydration");
    let hydrating = rt
        .execute_query(
            "SELECT hydration_state, shared_by_reference, hydrating, hydrated FROM red.forks",
        )
        .expect("red.forks hydrating select");
    let row = &hydrating.result.records[0];
    assert_eq!(text_field(row, "hydration_state"), "hydrating");
    assert_eq!(uint_field(row, "shared_by_reference"), 1);
    assert_eq!(uint_field(row, "hydrating"), 1);
    assert_eq!(uint_field(row, "hydrated"), 0);

    fork.hydrate_collection("users").expect("hydrate users");
    let hydrated = rt
        .execute_query(
            "SELECT hydration_state, shared_by_reference, hydrating, hydrated FROM red.forks",
        )
        .expect("red.forks hydrated select");
    let row = &hydrated.result.records[0];
    assert_eq!(text_field(row, "hydration_state"), "shared_by_reference");
    assert_eq!(uint_field(row, "shared_by_reference"), 1);
    assert_eq!(uint_field(row, "hydrating"), 0);
    assert_eq!(uint_field(row, "hydrated"), 1);

    fork.hydrate_collection("orders").expect("hydrate orders");
    let hydrated = rt
        .execute_query(
            "SELECT hydration_state, shared_by_reference, hydrating, hydrated FROM red.forks",
        )
        .expect("red.forks fully hydrated select");
    let row = &hydrated.result.records[0];
    assert_eq!(text_field(row, "hydration_state"), "hydrated");
    assert_eq!(uint_field(row, "shared_by_reference"), 0);
    assert_eq!(uint_field(row, "hydrating"), 0);
    assert_eq!(uint_field(row, "hydrated"), 2);
}

#[test]
fn select_from_red_collections_materializes_catalog_rows() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");

    let result = rt
        .execute_query("SELECT * FROM red.collections WHERE name = 'users'")
        .expect("red.collections select");

    assert_eq!(
        result.result.columns,
        vec![
            "name",
            "model",
            "schema_mode",
            "entities",
            "segments",
            "indices",
            "in_memory_bytes",
            "on_disk_bytes",
            "internal",
            "tenant_id",
            "queue_mode",
            "dimension",
            "metric",
            // Timeseries session columns (#576); red.* is additive-only (ADR 0011).
            "session_key",
            "session_gap_ms",
        ]
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(row.get("name"), Some(&Value::text("users")));
    assert_eq!(row.get("model"), Some(&Value::text("table")));
    assert_eq!(
        row.get("schema_mode"),
        Some(&Value::text("semi_structured"))
    );
    assert_eq!(row.get("entities"), Some(&Value::UnsignedInteger(1)));
    assert!(matches!(
        row.get("indices"),
        Some(Value::UnsignedInteger(_))
    ));
    assert!(matches!(
        row.get("in_memory_bytes"),
        Some(Value::UnsignedInteger(_))
    ));
    assert_eq!(row.get("on_disk_bytes"), Some(&Value::Null));
    assert_eq!(row.get("internal"), Some(&Value::Boolean(false)));

    cleanup_scope();
}

#[test]
fn red_collections_reports_persistent_collection_on_disk_bytes() {
    cleanup_scope();
    let dir = tempfile::Builder::new()
        .prefix("reddb-red-collections-disk-bytes-")
        .tempdir()
        .expect("temp db dir");
    let path = dir.path().join("data.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should be valid");
    let rt = RedDBRuntime::with_options(options).expect("persistent runtime should open");
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");
    rt.checkpoint().expect("checkpoint should flush pages");

    let result = rt
        .execute_query("SELECT name, on_disk_bytes FROM red.collections WHERE name = 'users'")
        .expect("red.collections select");

    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    let on_disk_bytes = uint_field(row, "on_disk_bytes");
    let db_size_bytes = std::fs::metadata(&path)
        .expect("database file should exist")
        .len();
    assert!(
        on_disk_bytes > 0,
        "persistent collection should report measured bytes, got {row:?}"
    );
    assert!(
        on_disk_bytes <= db_size_bytes,
        "collection bytes ({on_disk_bytes}) should fit within db file size ({db_size_bytes})"
    );

    cleanup_scope();
}

#[test]
fn red_schema_introspection_is_stable_across_virtual_tables() {
    cleanup_scope();
    let rt = runtime();
    seed_stable_introspection_fixture(&rt);

    let cases = [
        (
            "SELECT * FROM red.collections WHERE name IN ('projects', 'users') ORDER BY name",
            vec![
                "name",
                "model",
                "schema_mode",
                "entities",
                "segments",
                "indices",
                "in_memory_bytes",
                "on_disk_bytes",
                "internal",
                "tenant_id",
                "queue_mode",
                "dimension",
                "metric",
                // Timeseries session columns (#576); red.* is additive-only (ADR 0011).
                "session_key",
                "session_gap_ms",
            ],
        ),
        (
            "SELECT * FROM red.columns WHERE collection = 'users' ORDER BY name",
            vec![
                "collection",
                "name",
                "type",
                "nullable",
                "default_value",
                "is_primary_key",
                "is_unique",
            ],
        ),
        (
            "SELECT * FROM red.indices WHERE collection = 'users' ORDER BY name",
            INDEX_COLUMNS.to_vec(),
        ),
        (
            "SELECT * FROM red.policies WHERE collection = 'users' ORDER BY name",
            POLICY_COLUMNS.to_vec(),
        ),
        (
            "SELECT * FROM red.stats WHERE collection IN ('projects', 'users') ORDER BY collection",
            STATS_COLUMNS.to_vec(),
        ),
    ];

    for (sql, expected_columns) in cases {
        let first = query_snapshot(&rt, sql);
        let second = query_snapshot(&rt, sql);

        assert_eq!(first.0, expected_columns, "{sql}");
        assert_eq!(first, second, "{sql} changed between reads");
        assert!(!first.1.is_empty(), "{sql} returned no rows");
    }

    cleanup_scope();
}

#[test]
fn show_stats_row_table_returns_long_format_metric_set() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, active BOOLEAN)",
    );
    exec(
        &rt,
        "INSERT INTO users (id, email, active) VALUES (1, 'a@example.com', true)",
    );
    exec(
        &rt,
        "INSERT INTO users (id, email, active) VALUES (2, 'b@example.com', true)",
    );
    exec(
        &rt,
        "INSERT INTO users (id, email, active) VALUES (3, NULL, false)",
    );

    let (columns, rows) = query_snapshot(&rt, "SHOW STATS users");
    assert_eq!(columns, vec!["collection", "entity", "metric", "value"]);
    assert!(
        rows.iter().all(|row| row[0] == Value::text("users")),
        "every SHOW STATS row is scoped to the requested collection: {rows:?}"
    );

    // Collection-wide `row_count` carries a NULL entity.
    let row_count = rows
        .iter()
        .find(|row| row[2] == Value::text("row_count"))
        .expect("row_count metric present");
    assert_eq!(row_count[1], Value::Null);
    assert_eq!(row_count[3], Value::UnsignedInteger(3));

    // Per-column null / distinct counts for `email` (one NULL, two distinct).
    let email_nulls = rows
        .iter()
        .find(|row| row[1] == Value::text("email") && row[2] == Value::text("null_count"))
        .expect("email null_count present");
    assert_eq!(email_nulls[3], Value::UnsignedInteger(1));
    let email_distinct = rows
        .iter()
        .find(|row| row[1] == Value::text("email") && row[2] == Value::text("distinct_count"))
        .expect("email distinct_count present");
    assert_eq!(email_distinct[3], Value::UnsignedInteger(2));

    // Most-common-values is emitted per column as an array.
    assert!(
        rows.iter().any(|row| row[1] == Value::text("email")
            && row[2] == Value::text("most_common_values")
            && matches!(row[3], Value::Array(_))),
        "email most_common_values array present: {rows:?}"
    );

    // The long format is directly filterable/joinable on `metric`.
    let (_, filtered) = query_snapshot(
        &rt,
        "SELECT * FROM red.stats WHERE collection = 'users' AND metric = 'distinct_count'",
    );
    assert!(!filtered.is_empty(), "metric-filtered select returns rows");
    assert!(
        filtered
            .iter()
            .all(|row| row[2] == Value::text("distinct_count")),
        "metric filter keeps only distinct_count rows: {filtered:?}"
    );

    cleanup_scope();
}

#[test]
fn show_stats_returns_model_specific_metric_sets() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE KV counters");
    exec(
        &rt,
        "INSERT INTO counters KV (key, value) VALUES ('hits', 10)",
    );
    exec(
        &rt,
        "INSERT INTO counters KV (key, value) VALUES ('label', 'blue')",
    );

    exec(&rt, "CREATE GRAPH graph_stats");
    exec(
        &rt,
        "INSERT INTO graph_stats NODE (label, node_type, name) VALUES ('alpha', 'Service', 'Alpha')",
    );
    exec(
        &rt,
        "INSERT INTO graph_stats NODE (label, node_type, name) VALUES ('beta', 'Database', 'Beta')",
    );
    exec(
        &rt,
        "INSERT INTO graph_stats EDGE (label, from_rid, to_rid, weight) VALUES ('CONNECTS', 'alpha', 'beta', 1.0)",
    );

    exec(&rt, "CREATE VECTOR embeddings DIM 2 METRIC cosine");
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'a')",
    );
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'b')",
    );

    exec(&rt, "CREATE QUEUE jobs");
    exec(&rt, "QUEUE PUSH jobs 'one'");
    exec(&rt, "QUEUE PUSH jobs 'two'");
    exec(&rt, "QUEUE READ jobs CONSUMER worker1 COUNT 1");

    exec(&rt, "CREATE TIMESERIES cpu_metrics RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu.idle', 94.8, {host: 'srv1'}, 1704067200000000000)",
    );
    exec(
        &rt,
        "INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu.busy', 5.2, {host: 'srv1'}, 1704067201000000000)",
    );

    let (_, all_stats) = query_snapshot(&rt, "SHOW STATS");
    for collection in [
        "counters",
        "graph_stats",
        "embeddings",
        "jobs",
        "cpu_metrics",
    ] {
        assert!(
            all_stats
                .iter()
                .any(|row| row[0] == Value::text(collection)),
            "SHOW STATS should include {collection}: {all_stats:?}"
        );
    }

    assert_eq!(
        stat_value(&rt, "counters", Value::Null, "entry_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "counters", Value::text("integer"), "value_type_count"),
        Value::UnsignedInteger(1)
    );
    assert_eq!(
        stat_value(&rt, "counters", Value::Null, "total_value_bytes"),
        Value::UnsignedInteger(6)
    );

    assert_eq!(
        stat_value(&rt, "graph_stats", Value::Null, "node_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "graph_stats", Value::Null, "edge_count"),
        Value::UnsignedInteger(1)
    );
    assert_eq!(
        stat_value(
            &rt,
            "graph_stats",
            Value::text("CONNECTS"),
            "edge_label_count"
        ),
        Value::UnsignedInteger(1)
    );
    assert_eq!(
        stat_value(&rt, "graph_stats", Value::Null, "max_degree"),
        Value::UnsignedInteger(1)
    );

    assert_eq!(
        stat_value(&rt, "embeddings", Value::Null, "vector_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "embeddings", Value::Null, "dimension"),
        Value::UnsignedInteger(2)
    );

    assert_eq!(
        stat_value(&rt, "jobs", Value::Null, "message_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "jobs", Value::Null, "pending_count"),
        Value::UnsignedInteger(1)
    );
    assert_eq!(
        stat_value(&rt, "jobs", Value::Null, "delivered_count"),
        Value::UnsignedInteger(1)
    );

    assert_eq!(
        stat_value(&rt, "cpu_metrics", Value::Null, "point_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "cpu_metrics", Value::Null, "series_count"),
        Value::UnsignedInteger(2)
    );
    assert_eq!(
        stat_value(&rt, "cpu_metrics", Value::Null, "oldest_timestamp_ns"),
        Value::UnsignedInteger(1_704_067_200_000_000_000)
    );
    assert_eq!(
        stat_value(
            &rt,
            "cpu_metrics",
            Value::text("cpu.busy"),
            "metric_point_count"
        ),
        Value::UnsignedInteger(1)
    );

    cleanup_scope();
}

#[test]
fn show_stats_requires_tenant_for_non_admin_identity() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT)");
    set_current_connection_id(24403);
    set_current_auth_identity("alice".to_string(), Role::Read);

    // `red.stats` follows the shared `red.*` tenant gate: a tenant-less
    // non-admin identity is rejected before any profiling scan runs.
    let err = rt
        .execute_query("SHOW STATS")
        .expect_err("tenant-less non-admin should be rejected")
        .to_string();
    assert!(err.contains("active tenant"), "error was: {err}");

    // With an active tenant the scan runs and stays scoped to the tenant.
    set_current_tenant("acme".to_string());
    let scoped = rt
        .execute_query(
            "SELECT * FROM red.stats WHERE collection = 'users' AND metric = 'row_count'",
        )
        .expect("tenant-scoped red.stats read");
    assert!(scoped
        .result
        .records
        .iter()
        .all(|record| record.get("collection") == Some(&Value::text("users"))));

    cleanup_scope();
}

#[test]
fn governance_and_evidence_views_are_queryable_and_minimize_secret_material() {
    cleanup_scope();
    let rt = runtime();
    let auth = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("ops", "ops-password", Role::Admin)
        .unwrap();
    auth.create_user_in_tenant(Some("acme"), "alice", "alice-password", Role::Write)
        .unwrap();
    let alice_key = auth
        .create_api_key_in_tenant(Some("acme"), "alice", "deploy", Role::Write)
        .unwrap();
    auth.put_policy(registry_policy("registry-admin")).unwrap();
    auth.put_policy(table_read_policy("alice-red-schema-read"))
        .unwrap();
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(UserId::platform("ops")),
        "registry-admin",
    )
    .unwrap();
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(UserId::platform("alice")),
        "alice-red-schema-read",
    )
    .unwrap();
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(UserId::scoped("acme", "alice")),
        "alice-red-schema-read",
    )
    .unwrap();
    rt.set_auth_store(std::sync::Arc::clone(&auth));

    let registry = rt.config_registry();
    let actor = UserId::platform("ops");
    registry
        .register(
            auth.as_ref(),
            &actor,
            &registry_admin_ctx(),
            registry_draft("checkout_guardrail", "v1", true),
            1_700_000_000_001,
        )
        .expect("register managed policy");
    registry
        .supersede(
            auth.as_ref(),
            &actor,
            &registry_admin_ctx(),
            registry_draft("checkout_guardrail", "v2", true),
            "tighten approval evidence",
            1_700_000_000_002,
        )
        .expect("supersede managed policy");

    exec(&rt, "CREATE TABLE audit_docs (id INT)");
    let denied = {
        set_current_auth_identity("alice".to_string(), Role::Read);
        let err = rt
            .execute_query("CREATE TABLE denied_audit_docs (id INT)")
            .expect_err("reader must not create tables");
        clear_current_auth_identity();
        err
    };
    assert!(denied.to_string().contains("permission denied"));

    let registry_rows = rt
        .execute_query("SELECT * FROM red.registry WHERE id = 'checkout_guardrail'")
        .expect("red.registry select");
    assert_eq!(
        registry_rows.result.columns,
        REGISTRY_COLUMNS.map(str::to_string)
    );
    assert_eq!(registry_rows.result.records.len(), 1);
    let registry_row = &registry_rows.result.records[0];
    assert_eq!(
        registry_row.get("version"),
        Some(&Value::UnsignedInteger(2))
    );
    assert_eq!(registry_row.get("managed"), Some(&Value::Boolean(true)));
    assert_eq!(registry_row.get("schema"), Some(&Value::text("v2")));

    let history_rows = rt
        .execute_query("SELECT * FROM red.registry_history WHERE id = 'checkout_guardrail'")
        .expect("red.registry_history select");
    assert_eq!(
        history_rows.result.columns,
        REGISTRY_HISTORY_COLUMNS.map(str::to_string)
    );
    assert_eq!(history_rows.result.records.len(), 1);
    assert_eq!(
        history_rows.result.records[0].get("change_reason"),
        Some(&Value::text("tighten approval evidence"))
    );

    let managed = rt
        .execute_query("SELECT * FROM red.managed_policies WHERE policy_id = 'checkout_guardrail'")
        .expect("red.managed_policies select");
    assert_eq!(
        managed.result.columns,
        MANAGED_POLICY_COLUMNS.map(str::to_string)
    );
    assert_eq!(managed.result.records.len(), 1);

    let allowed_events = rt
        .execute_query(
            "SELECT actor_user_id, scope, action, resource, outcome FROM red.control_events \
             WHERE kind = 'schema.ddl' AND outcome = 'allowed'",
        )
        .expect("red.control_events allowed filter");
    assert!(
        allowed_events
            .result
            .records
            .iter()
            .any(
                |row| row.get("action") == Some(&Value::text("create_table"))
                    && row.get("resource") == Some(&Value::text("table:audit_docs"))
            ),
        "allowed control event missing: {:?}",
        allowed_events.result.records
    );

    let denied_events = rt
        .execute_query(
            "SELECT actor_user_id, scope, action, resource, outcome FROM red.control_events \
             WHERE actor_user_id = 'alice' AND outcome = 'denied'",
        )
        .expect("red.control_events actor/outcome filter");
    assert_eq!(denied_events.result.records.len(), 1);
    assert_eq!(
        denied_events.result.records[0].get("action"),
        Some(&Value::text("create_table"))
    );

    let all_event_columns = rt
        .execute_query("SELECT * FROM red.control_events WHERE outcome = 'denied'")
        .expect("red.control_events column contract");
    assert_eq!(
        all_event_columns.result.columns,
        CONTROL_EVENT_COLUMNS.map(str::to_string)
    );

    set_current_auth_identity("alice".to_string(), Role::Read);
    set_current_tenant("acme".to_string());
    let scoped_events = rt
        .execute_query("SELECT * FROM red.control_events WHERE actor_user_id = 'alice'")
        .expect("tenant-scoped control events");
    assert_eq!(scoped_events.result.records.len(), 0);
    clear_current_auth_identity();
    clear_current_tenant();

    let users = rt
        .execute_query("SELECT * FROM red.users WHERE username = 'alice'")
        .expect("red.users select");
    assert_eq!(
        users.result.columns,
        USER_EVIDENCE_COLUMNS.map(str::to_string)
    );
    assert_eq!(users.result.records.len(), 1);
    assert_eq!(
        users.result.records[0].get("tenant_id"),
        Some(&Value::text("acme"))
    );
    let users_body = format!("{:?}", users.result);
    assert!(!users.result.columns.iter().any(|c| c == "password_hash"));
    assert!(!users_body.contains("alice-password"), "{users_body}");

    let api_keys = rt
        .execute_query("SELECT * FROM red.api_keys WHERE owner = 'acme/alice'")
        .expect("red.api_keys select");
    assert_eq!(
        api_keys.result.columns,
        API_KEY_EVIDENCE_COLUMNS.map(str::to_string)
    );
    assert_eq!(api_keys.result.records.len(), 1);
    assert_eq!(
        api_keys.result.records[0].get("name"),
        Some(&Value::text("deploy"))
    );
    let key_body = format!("{:?}", api_keys.result);
    assert!(!api_keys.result.columns.iter().any(|c| c == "key"));
    assert!(!key_body.contains(&alice_key.key), "{key_body}");

    let capabilities = rt
        .execute_query(
            "SELECT * FROM red.control_capabilities WHERE action = 'red.registry:register'",
        )
        .expect("red.control_capabilities select");
    assert_eq!(
        capabilities.result.columns,
        CONTROL_CAPABILITY_COLUMNS.map(str::to_string)
    );
    assert_eq!(capabilities.result.records.len(), 1);
    assert_eq!(
        capabilities.result.records[0].get("resource_kind"),
        Some(&Value::text("registry"))
    );

    cleanup_scope();
}

// Issue #709 — `red.policy.actions` virtual table contract. Verifies
// the six-column shape, that the catalog deprecated entry surfaces a
// non-NULL `replacement` + `since_version` pair, that active rows
// expose NULL there, and that WHERE / ORDER BY work as advertised.
#[test]
fn red_policy_actions_virtual_table_surfaces_catalog() {
    cleanup_scope();
    let rt = runtime();

    let result = rt
        .execute_query("SELECT * FROM red.policy.actions ORDER BY name")
        .expect("red.policy.actions select")
        .result;

    assert_eq!(result.columns, POLICY_ACTION_COLUMNS.map(str::to_string));
    // Matches the catalog cardinality so a stray addition trips the
    // test; the count is intentionally read off the live catalog
    // rather than hard-coded.
    let catalog_len = reddb::auth::action_catalog::ACTIONS.len();
    assert_eq!(result.records.len(), catalog_len);

    // Every catalog entry has all six columns populated (replacement
    // and since_version are NULL for Active entries, populated for
    // Deprecated entries — both shapes are valid).
    for record in &result.records {
        assert!(matches!(record.get("name"), Some(Value::Text(_))));
        assert!(matches!(record.get("category"), Some(Value::Text(_))));
        assert!(matches!(
            record.get("lifecycle_state"),
            Some(Value::Text(_))
        ));
        assert!(matches!(
            record.get("gates_description"),
            Some(Value::Text(_))
        ));
    }

    // Vault category WHERE filter.
    let vault = rt
        .execute_query("SELECT * FROM red.policy.actions WHERE category = 'vault'")
        .expect("vault filter")
        .result;
    assert!(!vault.records.is_empty());
    for record in &vault.records {
        assert_eq!(record.get("category"), Some(&Value::text("vault")));
    }

    // Deprecated entry: replacement + since_version populated.
    let dep = rt
        .execute_query("SELECT * FROM red.policy.actions WHERE name = 'vault:unseal_history'")
        .expect("deprecated lookup")
        .result;
    assert_eq!(dep.records.len(), 1);
    let row = &dep.records[0];
    assert_eq!(row.get("lifecycle_state"), Some(&Value::text("deprecated")));
    assert_eq!(
        row.get("replacement"),
        Some(&Value::text("vault:read_metadata"))
    );
    assert_eq!(row.get("since_version"), Some(&Value::text("0.5.0")));

    // Active entry: replacement + since_version are NULL.
    let active = rt
        .execute_query("SELECT * FROM red.policy.actions WHERE name = 'policy:put'")
        .expect("active lookup")
        .result;
    assert_eq!(active.records.len(), 1);
    let row = &active.records[0];
    assert_eq!(row.get("lifecycle_state"), Some(&Value::text("active")));
    assert_eq!(row.get("replacement"), Some(&Value::Null));
    assert_eq!(row.get("since_version"), Some(&Value::Null));

    cleanup_scope();
}

#[test]
fn lint_policy_json_returns_diagnostic_rows() {
    cleanup_scope();
    let rt = runtime();
    // No auth_store needed for the JSON form — the linter walks the
    // literal directly.
    let result = rt
        .execute_query(
            "LINT POLICY JSON '{\"id\":\"p\",\"version\":1,\"statements\":[\
             {\"effect\":\"allow\",\"actions\":[\"definitely-not-an-action\",\"vault:unseal_history\"],\
             \"resources\":[\"*\"]}]}'",
        )
        .expect("LINT POLICY JSON")
        .result;

    assert_eq!(
        result.columns,
        vec![
            "severity".to_string(),
            "code".to_string(),
            "message".to_string(),
            "suggested_fix".to_string(),
            "location".to_string(),
        ]
    );
    // 3 diagnostics: UnknownAction (error), DeprecatedAction (warning),
    // SuspectResource (warning).
    assert_eq!(result.records.len(), 3, "rows={:?}", result.records);
    // Errors sort before warnings.
    assert_eq!(text(&result.records[0], "severity"), "error");
    assert_eq!(text(&result.records[0], "code"), "unknown_action");
    // The deprecated diagnostic carries the catalog's replacement hint.
    let dep = result
        .records
        .iter()
        .find(
            |r| matches!(r.get("code"), Some(Value::Text(s)) if s.as_ref() == "deprecated_action"),
        )
        .expect("deprecated_action row");
    assert_eq!(text(dep, "suggested_fix"), "vault:read_metadata");
    cleanup_scope();
}

#[test]
fn lint_policy_by_id_matches_lint_policy_json() {
    cleanup_scope();
    let rt = runtime();
    let auth = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
    let policy_json = r#"{"id":"shadowed","version":1,"statements":[
        {"effect":"allow","actions":["select"],"resources":["table:foo"]},
        {"effect":"deny","actions":["select"],"resources":["table:foo"]}
    ]}"#;
    let policy = Policy::from_json_str(policy_json).expect("policy parse");
    auth.put_policy(policy).expect("put policy");
    rt.set_auth_store(auth.clone());

    // By id — fetches the stored document and lints it.
    let by_id = rt
        .execute_query("LINT POLICY 'shadowed'")
        .expect("LINT POLICY id")
        .result;
    // By JSON literal — same diagnostic shape, no auth_store touch.
    let by_json_sql = format!("LINT POLICY JSON '{}'", policy_json.replace('\'', "''"));
    let by_json = rt
        .execute_query(&by_json_sql)
        .expect("LINT POLICY JSON")
        .result;

    // Same diagnostic set in the same order.
    assert!(!by_id.records.is_empty());
    assert_eq!(by_id.records.len(), by_json.records.len());
    for (a, b) in by_id.records.iter().zip(by_json.records.iter()) {
        assert_eq!(a.get("code"), b.get("code"), "rows={a:?} vs {b:?}");
        assert_eq!(a.get("severity"), b.get("severity"));
    }
    let has_no_effect = by_id.records.iter().any(
        |r| matches!(r.get("code"), Some(Value::Text(s)) if s.as_ref() == "no_effect_statements"),
    );
    assert!(has_no_effect, "rows={:?}", by_id.records);
    cleanup_scope();
}

#[test]
fn lint_policy_unknown_id_errors() {
    cleanup_scope();
    let rt = runtime();
    let auth = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
    rt.set_auth_store(auth);
    let err = rt
        .execute_query("LINT POLICY 'missing'")
        .expect_err("missing policy must error");
    assert!(format!("{err:?}").contains("missing"));
    cleanup_scope();
}

#[test]
fn lint_policy_clean_policy_returns_zero_rows() {
    cleanup_scope();
    let rt = runtime();
    let result = rt
        .execute_query(
            "LINT POLICY JSON '{\"id\":\"p\",\"version\":1,\"statements\":[\
             {\"effect\":\"allow\",\"actions\":[\"select\"],\"resources\":[\"table:public.orders\"]}]}'",
        )
        .expect("clean lint")
        .result;
    assert!(result.records.is_empty(), "rows={:?}", result.records);
    cleanup_scope();
}

#[test]
fn show_commands_match_red_schema_queries_for_stable_introspection() {
    cleanup_scope();
    let rt = runtime();
    seed_stable_introspection_fixture(&rt);

    for (show_sql, select_sql) in [
        (
            "SHOW COLLECTIONS WHERE name IN ('projects', 'users') ORDER BY name",
            "SELECT * FROM red.collections WHERE name IN ('projects', 'users') AND internal = false ORDER BY name",
        ),
        (
            "SHOW TABLES WHERE name IN ('projects', 'users') ORDER BY name",
            "SELECT * FROM red.collections WHERE model = 'table' AND name IN ('projects', 'users') ORDER BY name",
        ),
        (
            "SHOW SCHEMA users",
            "SELECT * FROM red.columns WHERE collection = 'users'",
        ),
        (
            "SHOW POLICIES ON users ORDER BY name",
            "SELECT * FROM red.policies WHERE collection = 'users' ORDER BY name",
        ),
        (
            "SHOW STATS users",
            "SELECT * FROM red.stats WHERE collection = 'users'",
        ),
    ] {
        assert_eq!(
            query_snapshot(&rt, show_sql),
            query_snapshot(&rt, select_sql),
            "{show_sql} should match {select_sql}",
        );
    }

    cleanup_scope();
}

#[test]
fn select_from_red_columns_materializes_table_schema() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT DEFAULT = 'unknown', active BOOLEAN NOT NULL)",
    );

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'users'")
        .expect("red.columns select");

    assert_eq!(
        result.result.columns,
        vec![
            "collection",
            "name",
            "type",
            "nullable",
            "default_value",
            "is_primary_key",
            "is_unique",
        ]
    );
    assert_eq!(result.result.records.len(), 4);

    let id = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "id")
        .expect("id column");
    assert_eq!(text(id, "collection"), "users");
    assert_eq!(text(id, "type"), "INTEGER");
    assert!(!bool_field(id, "nullable"));
    assert!(bool_field(id, "is_primary_key"));
    assert!(bool_field(id, "is_unique"));

    let email = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "email")
        .expect("email column");
    assert!(bool_field(email, "nullable"));
    assert!(bool_field(email, "is_unique"));

    let active = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "active")
        .expect("active column");
    assert_eq!(text(active, "type"), "BOOLEAN");
    assert!(!bool_field(active, "nullable"));

    cleanup_scope();
}

#[test]
fn select_from_red_indices_materializes_index_status_rows() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(
        &rt,
        "CREATE INDEX users_email_idx ON users (email) USING HASH",
    );

    let result = rt
        .execute_query("SELECT * FROM red.indices WHERE collection = 'users'")
        .expect("red.indices select");

    assert_eq!(result.result.columns, INDEX_COLUMNS.map(str::to_string));
    let row = result
        .result
        .records
        .iter()
        .find(|record| text_field(record, "name") == "users_email_idx")
        .expect("users_email_idx row");
    assert_eq!(row.get("collection"), Some(&Value::text("users")));
    assert_eq!(row.get("kind"), Some(&Value::text("hash")));
    assert_eq!(row.get("enabled"), Some(&Value::Boolean(true)));
    assert_eq!(row.get("build_state"), Some(&Value::text("ready")));
    assert_eq!(row.get("queryable"), Some(&Value::Boolean(true)));
    assert_eq!(row.get("requires_rebuild"), Some(&Value::Boolean(false)));

    cleanup_scope();
}

#[test]
fn select_from_red_stats_materializes_long_format_profiling_rows() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");

    let result = rt
        .execute_query("SELECT * FROM red.stats WHERE collection = 'users'")
        .expect("red.stats select");

    assert_eq!(
        result.result.columns,
        vec!["collection", "entity", "metric", "value"]
    );
    assert!(!result.result.records.is_empty());
    assert!(result
        .result
        .records
        .iter()
        .all(|row| row.get("collection") == Some(&Value::text("users"))));

    // Collection-wide row_count with a NULL entity.
    let row_count = result
        .result
        .records
        .iter()
        .find(|row| row.get("metric") == Some(&Value::text("row_count")))
        .expect("row_count metric present");
    assert_eq!(row_count.get("entity"), Some(&Value::Null));
    assert_eq!(row_count.get("value"), Some(&Value::UnsignedInteger(1)));

    // Per-column metrics carry the column name as `entity`.
    assert!(result.result.records.iter().any(|row| {
        row.get("entity") == Some(&Value::text("name"))
            && row.get("metric") == Some(&Value::text("distinct_count"))
    }));

    cleanup_scope();
}

// Issue #1958 (ADR 0073 §1) — the process memory budget is queryable through
// the public RQL surface as a `red.stats` section, with the per-pool and live
// accounting fields present-but-empty until the downstream slices fill them.
const MEMORY_BUDGET_COLLECTION: &str = "red.memory_budget";

const MEMORY_BUDGET_SOURCES: [&str; 5] = [
    "config",
    "profile-default",
    "cgroup-v2",
    "cgroup-v1",
    "physical-fraction",
];

fn budget_metric(rt: &RedDBRuntime, metric: &str) -> Value {
    let sql = format!(
        "SELECT * FROM red.stats WHERE collection = '{MEMORY_BUDGET_COLLECTION}' \
         AND metric = '{metric}'"
    );
    let result = rt.execute_query(&sql).expect("red.stats budget section");
    let row = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("budget metric {metric} present"));
    assert_eq!(row.get("entity"), Some(&Value::Null));
    row.get("value").cloned().expect("budget metric value")
}

#[test]
fn red_stats_exposes_the_memory_budget_section_with_empty_placeholders() {
    cleanup_scope();
    let rt = runtime();

    let result = rt
        .execute_query(&format!(
            "SELECT * FROM red.stats WHERE collection = '{MEMORY_BUDGET_COLLECTION}'"
        ))
        .expect("red.stats budget section");
    assert_eq!(result.result.columns, STATS_COLUMNS.map(str::to_string));

    let metrics = result
        .result
        .records
        .iter()
        .filter_map(|row| match row.get("metric") {
            Some(Value::Text(metric)) => Some(metric.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        metrics,
        vec!["resolved_bytes", "source", "pool_shares", "live_accounting"],
        "documented budget field names, in order"
    );

    // The budget always exists and is never zero — there is no unlimited mode.
    match budget_metric(&rt, "resolved_bytes") {
        Value::UnsignedInteger(bytes) => assert!(bytes > 0, "resolved budget must be positive"),
        other => panic!("resolved_bytes should be an unsigned integer, got {other:?}"),
    }

    // Source is whichever tier the host resolved through; on an unconfigured
    // embedded runtime that is a detection tier, never `config`.
    match budget_metric(&rt, "source") {
        Value::Text(source) => {
            assert!(
                MEMORY_BUDGET_SOURCES.contains(&source.as_ref()),
                "unknown budget source: {source}"
            );
            assert_ne!(source.as_ref(), "config", "no budget was configured");
        }
        other => panic!("source should be text, got {other:?}"),
    }

    // Placeholders for the pool-sizing and enforcement slices: present so the
    // surface shape is stable, empty because nothing is sized or accounted yet.
    assert_eq!(budget_metric(&rt, "pool_shares"), Value::Array(Vec::new()));
    assert_eq!(
        budget_metric(&rt, "live_accounting"),
        Value::Array(Vec::new())
    );

    // The section is part of the unfiltered profiling scan too.
    let (_, all_stats) = query_snapshot(&rt, "SHOW STATS");
    assert!(
        all_stats
            .iter()
            .any(|row| row[0] == Value::text(MEMORY_BUDGET_COLLECTION)),
        "SHOW STATS should include the budget section: {all_stats:?}"
    );

    cleanup_scope();
}

#[test]
fn red_stats_echoes_an_explicitly_configured_memory_budget() {
    cleanup_scope();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(512 << 20))
        .expect("runtime with an explicit memory budget");

    assert_eq!(
        budget_metric(&rt, "resolved_bytes"),
        Value::UnsignedInteger(512 << 20)
    );
    assert_eq!(budget_metric(&rt, "source"), Value::text("config"));

    cleanup_scope();
}

#[test]
fn a_zero_memory_budget_is_rejected_at_boot_never_silently_replaced() {
    cleanup_scope();
    let err = match RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(0)) {
        Ok(_) => panic!("a zero budget must fail the boot"),
        Err(err) => err.to_string(),
    };

    assert!(
        err.contains("invalid memory budget `0`"),
        "error was: {err}"
    );
    assert!(
        err.contains("expected a positive byte count"),
        "error names the valid form: {err}"
    );
    assert!(err.contains("no unlimited mode"), "error was: {err}");

    cleanup_scope();
}

#[test]
fn red_stats_scoped_to_a_collection_omits_the_process_budget_section() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "INSERT INTO users (id) VALUES (1)");

    let scoped = rt
        .execute_query("SHOW STATS users")
        .expect("show stats users");
    assert!(
        scoped
            .result
            .records
            .iter()
            .all(|row| row.get("collection") == Some(&Value::text("users"))),
        "a collection-scoped scan carries no process-scoped budget rows"
    );

    cleanup_scope();
}

#[test]
fn red_stats_exposes_checkpoint_projection_lag() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE events (id INT, name TEXT)");
    exec(&rt, "INSERT INTO events (id, name) VALUES (1, 'created')");

    rt.checkpoint().expect("checkpoint should succeed");

    let after_checkpoint = rt
        .execute_query(
            "SELECT * FROM red.stats WHERE collection = 'events' \
             AND metric IN ('last_materialized_lsn', 'projection_lag')",
        )
        .expect("red.stats checkpoint projection metrics")
        .result
        .records;

    let materialized = after_checkpoint
        .iter()
        .find(|row| row.get("metric") == Some(&Value::text("last_materialized_lsn")))
        .expect("last_materialized_lsn metric present");
    let materialized_lsn = uint_field(materialized, "value");
    assert!(
        materialized_lsn > 0,
        "checkpoint should publish a non-zero materialized LSN"
    );

    let lag = after_checkpoint
        .iter()
        .find(|row| row.get("metric") == Some(&Value::text("projection_lag")))
        .expect("projection_lag metric present");
    assert_eq!(uint_field(lag, "value"), 0);

    exec(&rt, "INSERT INTO events (id, name) VALUES (2, 'updated')");

    let after_write = rt
        .execute_query(
            "SELECT * FROM red.stats WHERE collection = 'events' \
             AND metric = 'projection_lag'",
        )
        .expect("red.stats projection lag after write")
        .result
        .records;
    let lag = after_write.first().expect("projection_lag row present");
    assert!(
        uint_field(lag, "value") > 0,
        "a write after checkpoint should create projection lag"
    );

    cleanup_scope();
}

#[test]
fn show_schema_desugars_to_red_columns_collection_filter() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );

    let via_select = rt
        .execute_query("SELECT name, type FROM red.columns WHERE collection = 'users'")
        .expect("red.columns select");
    let via_show = rt
        .execute_query("SHOW SCHEMA users")
        .expect("SHOW SCHEMA users");

    assert_eq!(
        via_show.result.columns,
        vec![
            "collection",
            "name",
            "type",
            "nullable",
            "default_value",
            "is_primary_key",
            "is_unique"
        ]
    );
    let show_pairs: Vec<_> = via_show
        .result
        .records
        .iter()
        .map(|row| (text(row, "name").to_string(), text(row, "type").to_string()))
        .collect();
    let select_pairs: Vec<_> = via_select
        .result
        .records
        .iter()
        .map(|row| (text(row, "name").to_string(), text(row, "type").to_string()))
        .collect();
    assert_eq!(show_pairs, select_pairs);

    cleanup_scope();
}

#[test]
fn red_columns_infers_document_top_level_fields_as_nullable_schema() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        r#"INSERT INTO logs DOCUMENT VALUES ({"level":"warn","ip":"10.0.0.1"})"#,
    );
    exec(
        &rt,
        r#"INSERT INTO logs DOCUMENT VALUES ({"level":"info","msg":"login"})"#,
    );

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'logs'")
        .expect("document red.columns select");

    let names: Vec<_> = result
        .result
        .records
        .iter()
        .map(|row| text(row, "name").to_string())
        .collect();
    assert!(names.contains(&"body".to_string()), "names = {names:?}");
    assert!(names.contains(&"level".to_string()), "names = {names:?}");
    assert!(names.contains(&"ip".to_string()), "names = {names:?}");
    assert!(names.contains(&"msg".to_string()), "names = {names:?}");

    let level = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "level")
        .expect("level field");
    assert_eq!(text(level, "type"), "TEXT");
    assert!(!bool_field(level, "nullable"));

    let ip = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "ip")
        .expect("ip field");
    assert!(bool_field(ip, "nullable"));

    cleanup_scope();
}

#[test]
fn show_indices_lists_all_and_show_indices_on_filters_collection() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(&rt, "CREATE TABLE orders (id INT, total INT)");
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (1, 'a@example.com')",
    );
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (2, 'b@example.com')",
    );
    exec(&rt, "INSERT INTO orders (id, total) VALUES (1, 10)");
    exec(&rt, "INSERT INTO orders (id, total) VALUES (2, 20)");
    exec(
        &rt,
        "CREATE INDEX users_email_idx ON users (email) USING HASH",
    );
    exec(
        &rt,
        "CREATE UNIQUE INDEX orders_total_idx ON orders (total) USING BTREE",
    );

    let all = rt.execute_query("SHOW INDEXES").expect("SHOW INDEXES");
    assert_eq!(all.result.columns, SHOW_INDEX_COLUMNS.map(str::to_string));
    let all_names: Vec<String> = all
        .result
        .records
        .iter()
        .map(|record| text_field(record, "name").to_string())
        .collect();
    assert!(all_names.iter().any(|name| name == "users_email_idx"));
    assert!(all_names.iter().any(|name| name == "orders_total_idx"));
    let aliases = rt.execute_query("SHOW INDICES").expect("SHOW INDICES");
    assert_eq!(
        query_snapshot(&rt, "SHOW INDEXES ORDER BY name"),
        query_snapshot(&rt, "SHOW INDICES ORDER BY name")
    );

    let users_idx = all
        .result
        .records
        .iter()
        .find(|record| text_field(record, "name") == "users_email_idx")
        .expect("users_email_idx row");
    assert_eq!(text_field(users_idx, "table"), "users");
    assert_eq!(
        text_array_field(users_idx, "columns"),
        vec!["email".to_string()]
    );
    assert_eq!(text_field(users_idx, "kind"), "HASH");
    assert!(!bool_field(users_idx, "unique"));
    assert_eq!(uint_field(users_idx, "entries_indexed"), 2);

    let orders_idx = aliases
        .result
        .records
        .iter()
        .find(|record| text_field(record, "name") == "orders_total_idx")
        .expect("orders_total_idx row");
    assert_eq!(text_field(orders_idx, "table"), "orders");
    assert_eq!(
        text_array_field(orders_idx, "columns"),
        vec!["total".to_string()]
    );
    assert_eq!(text_field(orders_idx, "kind"), "BTREE");
    assert!(bool_field(orders_idx, "unique"));
    assert_eq!(uint_field(orders_idx, "entries_indexed"), 2);

    let filtered = rt
        .execute_query("SHOW INDEXES ON users")
        .expect("SHOW INDEXES ON users");
    assert_eq!(
        filtered.result.columns,
        SHOW_INDEX_COLUMNS.map(str::to_string)
    );
    assert!(filtered
        .result
        .records
        .iter()
        .any(|record| text_field(record, "name") == "users_email_idx"));
    assert!(filtered
        .result
        .records
        .iter()
        .all(|record| text_field(record, "table") == "users"));
    assert_eq!(
        query_snapshot(&rt, "SHOW INDEXES ON users ORDER BY name"),
        query_snapshot(
            &rt,
            "SELECT * FROM red.show_indexes WHERE table = 'users' ORDER BY name"
        )
    );

    let explain = rt
        .execute_query("EXPLAIN SELECT * FROM users WHERE email = 'a@example.com'")
        .expect("EXPLAIN SELECT with index");
    let plan_text = explain
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("op") {
            Some(Value::Text(op)) => Some(op.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        plan_text.contains("index_seek") || plan_text.contains("hash_index"),
        "expected indexed query path, got plan ops: {plan_text}"
    );

    cleanup_scope();
}

#[test]
fn show_stats_desugars_to_red_stats() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "CREATE TABLE projects (id INT)");
    exec(&rt, "INSERT INTO users (id) VALUES (1)");

    let single = rt
        .execute_query("SHOW STATS users")
        .expect("show stats users");
    // Long-format: every row is scoped to `users` and carries a
    // NULL-entity `row_count` row plus per-column metric rows.
    assert!(single
        .result
        .records
        .iter()
        .all(|record| record.get("collection") == Some(&Value::text("users"))));
    let row_count = single
        .result
        .records
        .iter()
        .find(|record| record.get("metric") == Some(&Value::text("row_count")))
        .expect("row_count metric present");
    assert_eq!(row_count.get("entity"), Some(&Value::Null));
    assert_eq!(row_count.get("value"), Some(&Value::UnsignedInteger(1)));

    let all = rt.execute_query("SHOW STATS").expect("show stats");
    let collections = all
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("collection") {
            Some(Value::Text(name)) => Some(name.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(collections.contains(&"users"));
    assert!(collections.contains(&"projects"));

    cleanup_scope();
}

#[test]
fn red_columns_returns_empty_for_schemaless_table_contract() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "INSERT INTO scratch (id, note) VALUES (1, 'loose')");

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'scratch'")
        .expect("schemaless red.columns select");

    assert_eq!(result.result.records.len(), 0);
    cleanup_scope();
}

#[test]
fn red_collections_requires_tenant_for_non_admin_identity() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE events (id INT)");
    set_current_connection_id(24401);
    set_current_auth_identity("alice".to_string(), Role::Read);

    let err = rt
        .execute_query("SELECT * FROM red.collections")
        .expect_err("tenant-less non-admin should be rejected")
        .to_string();
    assert!(err.contains("active tenant"), "error was: {err}");

    set_current_tenant("acme".to_string());
    let result = rt
        .execute_query("SELECT tenant_id FROM red.collections WHERE name = 'events'")
        .expect("tenant-scoped catalog read");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("tenant_id"),
        Some(&Value::text("acme"))
    );

    cleanup_scope();
}

#[test]
fn red_collections_admin_identity_bypasses_tenant_requirement() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE admin_visible (id INT)");
    set_current_connection_id(24402);
    set_current_auth_identity("root".to_string(), Role::Admin);

    let result = rt
        .execute_query("SELECT tenant_id FROM red.collections WHERE name = 'admin_visible'")
        .expect("admin catalog read");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("tenant_id"),
        Some(&Value::Null)
    );

    cleanup_scope();
}

#[test]
fn red_schema_dml_is_read_only() {
    cleanup_scope();
    let rt = runtime();
    for sql in [
        "INSERT INTO red.collections (name) VALUES ('x')",
        "UPDATE red.collections SET name = 'x'",
        "DELETE FROM red.collections WHERE name = 'x'",
    ] {
        let err = match rt.execute_query(sql) {
            Ok(_) => panic!("expected read-only error for {sql}"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("system schema is read-only"),
            "{sql} returned unexpected error: {err}"
        );
    }
    cleanup_scope();
}

// Issue #1961 (ADR 0073 §5) — consolidation is the budget's reclamation tool,
// so what it reclaimed and what arms it are both queryable through the public
// RQL surface.
const CONSOLIDATION_COLLECTION: &str = "red.consolidation";

const CONSOLIDATION_COUNTERS: [&str; 5] = [
    "consolidation_runs_started",
    "consolidation_runs_completed",
    "consolidation_segments_merged",
    "consolidation_tombstones_reclaimed",
    "consolidation_bytes_reclaimed",
];

#[test]
fn red_stats_exposes_the_process_consolidation_thresholds() {
    cleanup_scope();
    let rt = runtime();

    let result = rt
        .execute_query(&format!(
            "SELECT * FROM red.stats WHERE collection = '{CONSOLIDATION_COLLECTION}'"
        ))
        .expect("red.stats consolidation section");
    assert_eq!(result.result.columns, STATS_COLUMNS.map(str::to_string));

    let metrics = result
        .result
        .records
        .iter()
        .filter_map(|row| match row.get("metric") {
            Some(Value::Text(metric)) => Some(metric.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        metrics,
        vec![
            "tombstone_ratio_threshold",
            "fragmentation_ratio_threshold",
            "entities_per_tick",
        ],
        "documented threshold names, in order"
    );

    for row in &result.result.records {
        assert_eq!(row.get("entity"), Some(&Value::Null));
    }

    // The two ratios are fractions; the pacing bound is a positive entity count.
    for metric in ["tombstone_ratio_threshold", "fragmentation_ratio_threshold"] {
        let value = result
            .result
            .records
            .iter()
            .find(|row| row.get("metric") == Some(&Value::text(metric)))
            .and_then(|row| row.get("value").cloned())
            .unwrap_or_else(|| panic!("{metric} present"));
        match value {
            Value::Float(ratio) => assert!(
                ratio > 0.0 && ratio < 1.0,
                "{metric} should be a fraction, got {ratio}"
            ),
            other => panic!("{metric} should be a float, got {other:?}"),
        }
    }

    let per_tick = result
        .result
        .records
        .iter()
        .find(|row| row.get("metric") == Some(&Value::text("entities_per_tick")))
        .and_then(|row| row.get("value").cloned())
        .expect("entities_per_tick present");
    match per_tick {
        Value::UnsignedInteger(bound) => assert!(bound > 0, "pacing bound must be positive"),
        other => panic!("entities_per_tick should be an unsigned integer, got {other:?}"),
    }

    cleanup_scope();
}

#[test]
fn red_stats_exposes_per_collection_consolidation_counters() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");

    let result = rt
        .execute_query("SELECT * FROM red.stats WHERE collection = 'users'")
        .expect("red.stats select");

    for metric in CONSOLIDATION_COUNTERS {
        let row = result
            .result
            .records
            .iter()
            .find(|row| row.get("metric") == Some(&Value::text(metric)))
            .unwrap_or_else(|| panic!("{metric} present for a user collection"));
        assert_eq!(row.get("entity"), Some(&Value::Null));
        // A fresh collection has consolidated nothing — a real zero, not an
        // absent row.
        assert_eq!(row.get("value"), Some(&Value::UnsignedInteger(0)));
    }

    // The dead `compact_ops` counter is gone: nothing reports it any more.
    assert!(
        !result
            .result
            .records
            .iter()
            .any(|row| row.get("metric") == Some(&Value::text("compact_ops"))),
        "compact_ops was replaced by the consolidation counters"
    );

    cleanup_scope();
}

#[test]
fn red_stats_scoped_to_a_collection_omits_the_process_consolidation_thresholds() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "INSERT INTO users (id) VALUES (1)");

    let scoped = rt
        .execute_query("SHOW STATS users")
        .expect("show stats users");
    assert!(
        scoped
            .result
            .records
            .iter()
            .all(|row| row.get("collection") == Some(&Value::text("users"))),
        "a collection-scoped scan carries no process-scoped threshold rows"
    );

    cleanup_scope();
}
