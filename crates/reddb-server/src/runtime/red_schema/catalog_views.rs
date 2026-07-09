//! Schema / catalog introspection `red.*` snapshot builders.
//!
//! Extracted from the `red_schema` dispatcher (issue #1640). Serves
//! `red.collections`, `red.columns`, `red.describe`, `red.show_create`,
//! `red.show_indexes`, `red.indices`, and `red.stats`, along with the
//! DDL renderers and document-column inference that back them.

use super::helpers::*;
use super::*;

pub(super) fn indices_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        INDEX_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for status in snapshot.index_statuses {
        if !index_collection_visible(status.collection.as_deref(), visible_collections) {
            continue;
        }
        seen.insert((status.collection.clone(), status.name.clone()));
        rows.push(index_status_record(Arc::clone(&schema), status));
    }

    for collection in snapshot.collections {
        if !visible_collections.is_none_or(|visible| visible.contains(&collection.name)) {
            continue;
        }
        for index in runtime.index_store_ref().list_indices(&collection.name) {
            let key = (Some(index.collection.clone()), index.name.clone());
            if !seen.insert(key) {
                continue;
            }
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(index.collection),
                    Value::text(index.name),
                    Value::text(index_method_kind_name(index.method)),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::text("ready"),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ],
            ));
        }
    }

    rows
}

pub(super) fn show_indexes_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        SHOW_INDEX_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut rows = Vec::new();

    for collection in snapshot.collections {
        if !collection_is_visible(&collection.name, visible_collections) {
            continue;
        }
        for index in runtime.index_store_ref().list_indices(&collection.name) {
            let entries_indexed = runtime.index_store_ref().entries_indexed(&index);
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(index.name),
                    Value::text(index.collection),
                    Value::Array(
                        index
                            .columns
                            .into_iter()
                            .map(|value| Value::text(value.as_str()))
                            .collect(),
                    ),
                    Value::text(render_index_method_for_ddl(index.method)),
                    Value::Boolean(index.unique),
                    Value::UnsignedInteger(entries_indexed),
                ],
            ));
        }
    }

    rows
}

fn index_status_record(
    schema: Arc<Vec<Arc<str>>>,
    status: crate::catalog::CatalogIndexStatus,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            status
                .collection
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
            Value::text(status.name),
            Value::text(status.kind),
            Value::Boolean(status.declared),
            Value::Boolean(status.operational),
            Value::Boolean(status.enabled),
            status
                .build_state
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
            Value::Boolean(status.in_sync),
            Value::Boolean(status.queryable),
            Value::Boolean(status.requires_rebuild),
        ],
    )
}

fn index_collection_visible(
    collection: Option<&str>,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> bool {
    visible_collections
        .is_none_or(|visible| collection.is_some_and(|collection| visible.contains(collection)))
}

fn index_method_kind_name(kind: super::index_store::IndexMethodKind) -> &'static str {
    match kind {
        super::index_store::IndexMethodKind::Hash => "hash",
        super::index_store::IndexMethodKind::BTree => "btree",
        super::index_store::IndexMethodKind::Bitmap => "bitmap",
        super::index_store::IndexMethodKind::H3 { .. } => "spatial.h3",
    }
}

pub(super) fn describe_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let collection = describe_target_collection(query)?;
    let db = runtime.db();
    let exists = db
        .catalog_model_snapshot()
        .collections
        .into_iter()
        .any(|entry| entry.name == collection);
    if !exists || !collection_is_visible(&collection, visible_collections) {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    }

    let contracts = db.collection_contracts();
    let Some(contract) = contracts
        .iter()
        .find(|contract| contract.name == collection)
    else {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: DESCRIBE {collection} has no declared column schema"
        )));
    };
    if contract.declared_columns.is_empty() {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: DESCRIBE {collection} has no declared column schema"
        )));
    }

    let schema = Arc::new(
        DESCRIBE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let indexed_columns = runtime.index_store_ref().indexed_columns_set(&collection);
    Ok(contract
        .declared_columns
        .iter()
        .map(|column| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(column.name.clone()),
                    Value::text(
                        column
                            .sql_type
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| column.data_type.clone()),
                    ),
                    Value::Boolean(!(column.not_null || column.primary_key)),
                    column
                        .default
                        .as_deref()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::Boolean(indexed_columns.contains(&column.name)),
                ],
            )
        })
        .collect())
}

fn describe_target_collection(query: &TableQuery) -> RedDBResult<String> {
    match query.filter.as_ref() {
        Some(Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op: CompareOp::Eq,
            value: Value::Text(collection),
        }) if column == "collection" => Ok(collection.to_string()),
        _ => Err(RedDBError::Query(
            "DESCRIBE requires a collection name".to_string(),
        )),
    }
}

pub(super) fn show_create_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let collection = show_create_target_collection(query)?;
    let db = runtime.db();
    let catalog_entry = db
        .catalog_model_snapshot()
        .collections
        .into_iter()
        .find(|entry| entry.name == collection);
    let Some(catalog_entry) = catalog_entry else {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    };
    if !collection_is_visible(&collection, visible_collections) {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    }
    if catalog_entry.model != CollectionModel::Table {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} is only supported for table collections"
        )));
    }

    let contracts = db.collection_contracts();
    let Some(contract) = contracts
        .iter()
        .find(|contract| contract.name == collection)
    else {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} has no declared column schema"
        )));
    };
    if contract.declared_columns.is_empty() {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} has no declared column schema"
        )));
    }

    let ddl = render_show_create_table_ddl(
        contract,
        runtime.index_store_ref().list_indices(&collection),
    );
    let schema = Arc::new(
        SHOW_CREATE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(vec![UnifiedRecord::with_schema(
        schema,
        vec![Value::text(ddl)],
    )])
}

fn show_create_target_collection(query: &TableQuery) -> RedDBResult<String> {
    match query.filter.as_ref() {
        Some(Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op: CompareOp::Eq,
            value: Value::Text(collection),
        }) if column == "collection" => Ok(collection.to_string()),
        _ => Err(RedDBError::Query(
            "SHOW CREATE TABLE requires a table name".to_string(),
        )),
    }
}

fn render_show_create_table_ddl(
    contract: &crate::physical::CollectionContract,
    mut indices: Vec<super::index_store::RegisteredIndex>,
) -> String {
    let columns = contract
        .declared_columns
        .iter()
        .map(render_show_create_column)
        .collect::<Vec<_>>()
        .join(", ");
    let mut statements = vec![format!(
        "CREATE TABLE {} ({columns})",
        render_sql_identifier(&contract.name)
    )];

    indices.sort_by(|left, right| left.name.cmp(&right.name));
    for index in indices {
        let unique = if index.unique { "UNIQUE " } else { "" };
        let columns = index
            .columns
            .iter()
            .map(|column| render_sql_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        statements.push(format!(
            "CREATE {unique}INDEX {} ON {} ({columns}) USING {}",
            render_sql_identifier(&index.name),
            render_sql_identifier(&contract.name),
            render_index_method_for_ddl(index.method)
        ));
    }

    format!("{};", statements.join(";\n"))
}

fn render_show_create_column(column: &crate::physical::DeclaredColumnContract) -> String {
    let mut parts = vec![
        render_sql_identifier(&column.name),
        column
            .sql_type
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| column.data_type.clone()),
    ];

    if column.not_null && !column.primary_key {
        parts.push("NOT NULL".to_string());
    }
    if let Some(default) = column.default.as_deref() {
        parts.push(format!(
            "DEFAULT = {}",
            render_show_create_default(column, default)
        ));
    }
    if let Some(compress) = column.compress {
        parts.push(format!("COMPRESS:{compress}"));
    }
    if column.unique {
        parts.push("UNIQUE".to_string());
    }
    if column.primary_key {
        parts.push("PRIMARY KEY".to_string());
    }

    parts.join(" ")
}

fn render_show_create_default(
    column: &crate::physical::DeclaredColumnContract,
    default: &str,
) -> String {
    if default.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if show_create_default_needs_quotes(column) {
        return format!("'{}'", default.replace('\'', "''"));
    }
    default.to_string()
}

fn show_create_default_needs_quotes(column: &crate::physical::DeclaredColumnContract) -> bool {
    let base = column
        .sql_type
        .as_ref()
        .map(|sql_type| sql_type.base_name())
        .unwrap_or_else(|| column.data_type.to_ascii_uppercase());
    matches!(
        base.as_str(),
        "TEXT" | "STRING" | "EMAIL" | "UUID" | "IPADDR" | "MACADDR" | "ENUM"
    )
}

fn render_index_method_for_ddl(method: super::index_store::IndexMethodKind) -> &'static str {
    match method {
        super::index_store::IndexMethodKind::Hash => "HASH",
        super::index_store::IndexMethodKind::BTree => "BTREE",
        super::index_store::IndexMethodKind::Bitmap => "BITMAP",
        super::index_store::IndexMethodKind::H3 { .. } => "H3",
    }
}

fn render_sql_identifier(identifier: &str) -> String {
    identifier.to_string()
}

pub(super) fn collections_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        COLLECTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .filter(|collection| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &collection.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let collection_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = collection_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let queue_mode = if collection.model == CollectionModel::Queue {
                Value::text(super::impl_queue::queue_mode_str(
                    store.as_ref(),
                    &collection.name,
                ))
            } else {
                Value::Null
            };
            let vector_dimension = collection
                .vector_dimension
                .map(|dimension| Value::UnsignedInteger(dimension as u64))
                .unwrap_or(Value::Null);
            let vector_metric = collection
                .vector_metric
                .map(|metric| Value::text(distance_metric_name(metric)))
                .unwrap_or(Value::Null);
            let session_key = collection
                .session_key
                .as_ref()
                .map(|key| Value::text(key.clone()))
                .unwrap_or(Value::Null);
            let session_gap_ms = collection
                .session_gap_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::text(collection_model_name(collection.model)),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(collection.segments as u64),
                    Value::UnsignedInteger(collection.indices.len() as u64),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    queue_mode,
                    vector_dimension,
                    vector_metric,
                    session_key,
                    session_gap_ms,
                ],
            )
        })
        .collect()
}

/// Number of most-common values surfaced per column in `red.stats`.
/// The planner tracks up to 100; the introspection view keeps the
/// hottest handful so the long-format `value` array stays readable.
const STATS_MCV_LIMIT: usize = 10;

/// Synthetic `collection` label carrying the process-scoped memory-budget
/// rows in `red.stats`. The budget belongs to the process, not to any user
/// collection, so it occupies a reserved `red.`-prefixed label rather than
/// being repeated once per collection.
const MEMORY_BUDGET_COLLECTION: &str = "red.memory_budget";
const SCRUB_COLLECTION: &str = "red.scrub";

/// Long-format `red.stats` profiling view (issue #1787). This is the
/// **computed** freshness tier: every read runs an on-demand profiling
/// scan over the target collections rather than serving a cached
/// snapshot. Emitted rows are `(collection, entity, metric, value)`:
///
/// * `row_count` — one row per collection, `entity` is `NULL`.
/// * `null_count` / `distinct_count` / `most_common_values` — one row
///   per column, `entity` is the column name.
///
/// The view also carries the process-scoped memory-budget section under the
/// `red.memory_budget` collection label — see `append_memory_budget_stats`.
pub(super) fn stats_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
    query: &TableQuery,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        STATS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    // `SHOW STATS <name>` desugars to a `collection = '<name>'` filter;
    // scope the profiling scan to that one collection so an unfiltered
    // `SELECT * FROM red.stats` is the only path that scans everything.
    let target = stats_target_collection(query);

    let checkpoint_stats = runtime
        .inner
        .checkpoint_projection_stats
        .snapshot(runtime.cdc_current_lsn());
    let pending_wal_records = runtime.db().embedded_pending_wal_records().unwrap_or(0);

    let mut rows = Vec::new();
    append_memory_budget_stats(
        &mut rows,
        &schema,
        &runtime.memory_budget(),
        target.as_deref(),
    );
    append_scrub_stats(
        &mut rows,
        &schema,
        &runtime.scrub_stats_snapshot(),
        target.as_deref(),
    );
    for collection in snapshot.collections {
        if let Some(target) = target.as_deref() {
            if collection.name != target {
                continue;
            }
        }
        if !visible_collections.is_none_or(|visible| visible.contains(&collection.name)) {
            continue;
        }

        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "pending_wal_records",
            Value::UnsignedInteger(pending_wal_records),
        ));
        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "current_lsn",
            Value::UnsignedInteger(checkpoint_stats.current_lsn),
        ));
        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "last_materialized_lsn",
            Value::UnsignedInteger(checkpoint_stats.last_materialized_lsn),
        ));
        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "projection_lag",
            Value::UnsignedInteger(checkpoint_stats.projection_lag),
        ));
        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "checkpoints_completed",
            Value::UnsignedInteger(checkpoint_stats.checkpoints_completed),
        ));
        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "last_checkpoint_duration_ms",
            Value::UnsignedInteger(checkpoint_stats.last_checkpoint_duration_ms),
        ));

        match collection.model {
            CollectionModel::Table => {
                append_table_stats(&mut rows, &schema, store.as_ref(), &collection.name);
            }
            CollectionModel::Kv => {
                append_kv_stats(&mut rows, &schema, store.as_ref(), &collection.name);
            }
            CollectionModel::Graph => {
                append_graph_stats(&mut rows, &schema, store.as_ref(), &collection.name);
            }
            CollectionModel::Vector => {
                append_vector_stats(
                    &mut rows,
                    &schema,
                    store.as_ref(),
                    &collection.name,
                    collection.vector_dimension,
                    collection.vector_metric,
                );
            }
            CollectionModel::Queue => {
                append_queue_stats(&mut rows, &schema, store.as_ref(), &collection.name);
            }
            CollectionModel::TimeSeries => {
                append_timeseries_stats(&mut rows, &schema, store.as_ref(), &collection.name);
            }
            _ => {}
        }
    }
    rows
}

fn append_scrub_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    scrub: &crate::runtime::ScrubStatsSnapshot,
    target: Option<&str>,
) {
    if target.is_some_and(|target| target != SCRUB_COLLECTION) {
        return;
    }

    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::Null,
        "last_run_unix_ms",
        Value::UnsignedInteger(scrub.last_run_unix_ms),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::Null,
        "last_findings_count",
        Value::UnsignedInteger(scrub.last_findings_count),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::Null,
        "background_status",
        Value::text(scrub.background_status.as_str()),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::Null,
        "background_verified_objects",
        Value::UnsignedInteger(scrub.background_verified_objects),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::Null,
        "background_total_objects",
        Value::UnsignedInteger(scrub.background_total_objects),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::text("superblock"),
        "verified_objects",
        Value::UnsignedInteger(scrub.verified.superblock),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::text("manifest"),
        "verified_objects",
        Value::UnsignedInteger(scrub.verified.manifest),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::text("wal"),
        "verified_objects",
        Value::UnsignedInteger(scrub.verified.wal),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::text("page"),
        "verified_objects",
        Value::UnsignedInteger(scrub.verified.page),
    ));
    rows.push(stats_row(
        schema,
        SCRUB_COLLECTION,
        Value::text("segment-chunk"),
        "verified_objects",
        Value::UnsignedInteger(scrub.verified.segment_chunk),
    ));
}

/// Memory-budget section of `red.stats` (ADR 0073 §1, issue #1958).
///
/// Four rows under the `red.memory_budget` collection label, all with a NULL
/// `entity`:
///
/// * `resolved_bytes` — the one budget this process runs under.
/// * `source` — which precedence tier produced it (`config`,
///   `profile-default`, `cgroup-v2`, `cgroup-v1`, `physical-fraction`).
/// * `pool_shares` — per-pool budget shares. Empty until the pool-sizing
///   slice fills it; present from day one so the surface shape is stable.
/// * `live_accounting` — live per-pool usage. Empty until the enforcement
///   slice fills it.
///
/// The two placeholders are empty arrays rather than zeros: this slice
/// resolves the number, it does not size or account anything, and a `0` would
/// claim otherwise.
///
/// Budget rows are process-scoped, so a `SHOW STATS <collection>` scan that
/// targets a user collection skips them entirely.
fn append_memory_budget_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    budget: &crate::storage::memory_budget::MemoryBudget,
    target: Option<&str>,
) {
    if target.is_some_and(|target| target != MEMORY_BUDGET_COLLECTION) {
        return;
    }

    rows.push(stats_row(
        schema,
        MEMORY_BUDGET_COLLECTION,
        Value::Null,
        "resolved_bytes",
        Value::UnsignedInteger(budget.resolved_bytes),
    ));
    rows.push(stats_row(
        schema,
        MEMORY_BUDGET_COLLECTION,
        Value::Null,
        "source",
        Value::text(budget.source.as_str()),
    ));
    rows.push(stats_row(
        schema,
        MEMORY_BUDGET_COLLECTION,
        Value::Null,
        "pool_shares",
        Value::Array(Vec::new()),
    ));
    rows.push(stats_row(
        schema,
        MEMORY_BUDGET_COLLECTION,
        Value::Null,
        "live_accounting",
        Value::Array(Vec::new()),
    ));
}

fn append_table_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
) {
    let Some(analyzed) =
        crate::storage::query::planner::stats_catalog::analyze_collection(store, collection)
    else {
        return;
    };

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "row_count",
        Value::UnsignedInteger(analyzed.row_count),
    ));
    for column in &analyzed.columns {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(column.name.clone()),
            "null_count",
            Value::UnsignedInteger(column.null_count),
        ));
        rows.push(stats_row(
            schema,
            collection,
            Value::text(column.name.clone()),
            "distinct_count",
            Value::UnsignedInteger(column.distinct_count),
        ));
        rows.push(stats_row(
            schema,
            collection,
            Value::text(column.name.clone()),
            "most_common_values",
            Value::Array(most_common_values(column.mcv.as_ref())),
        ));
    }
}

fn append_kv_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
) {
    let mut entry_count = 0u64;
    let mut total_key_bytes = 0u64;
    let mut total_value_bytes = 0u64;
    let mut type_counts = BTreeMap::<&'static str, u64>::new();

    for (name, entity) in store.query_all(|_| true) {
        if name != collection {
            continue;
        }
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        entry_count += 1;
        if let Some(Value::Text(key)) = row.get_field("key") {
            total_key_bytes += key.len() as u64;
        }
        if let Some(value) = row.get_field("value") {
            total_value_bytes += value_estimated_bytes(value);
            *type_counts.entry(value_type_name(value)).or_default() += 1;
        }
    }

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "entry_count",
        Value::UnsignedInteger(entry_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "total_key_bytes",
        Value::UnsignedInteger(total_key_bytes),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "total_value_bytes",
        Value::UnsignedInteger(total_value_bytes),
    ));
    for (value_type, count) in type_counts {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(value_type),
            "value_type_count",
            Value::UnsignedInteger(count),
        ));
    }
}

fn append_graph_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
) {
    let mut node_count = 0u64;
    let mut edge_count = 0u64;
    let mut degree = BTreeMap::<String, u64>::new();
    let mut node_labels = BTreeMap::<String, u64>::new();
    let mut edge_labels = BTreeMap::<String, u64>::new();

    for (name, entity) in store.query_all(|_| true) {
        if name != collection {
            continue;
        }
        match &entity.kind {
            crate::storage::unified::EntityKind::GraphNode(node) => {
                node_count += 1;
                *node_labels.entry(node.node_type.clone()).or_default() += 1;
                degree.entry(node.label.clone()).or_default();
            }
            crate::storage::unified::EntityKind::GraphEdge(edge) => {
                edge_count += 1;
                *edge_labels.entry(edge.label.clone()).or_default() += 1;
                *degree.entry(edge.from_node.clone()).or_default() += 1;
                *degree.entry(edge.to_node.clone()).or_default() += 1;
            }
            _ => {}
        }
    }

    let max_degree = degree.values().copied().max().unwrap_or(0);
    let avg_degree = degree
        .values()
        .sum::<u64>()
        .checked_div(node_count)
        .unwrap_or(0);

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "node_count",
        Value::UnsignedInteger(node_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "edge_count",
        Value::UnsignedInteger(edge_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "max_degree",
        Value::UnsignedInteger(max_degree),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "avg_degree",
        Value::UnsignedInteger(avg_degree),
    ));
    for (label, count) in node_labels {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(label),
            "node_label_count",
            Value::UnsignedInteger(count),
        ));
    }
    for (label, count) in edge_labels {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(label),
            "edge_label_count",
            Value::UnsignedInteger(count),
        ));
    }
}

fn append_vector_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
    declared_dimension: Option<usize>,
    declared_metric: Option<crate::storage::engine::distance::DistanceMetric>,
) {
    let mut vector_count = 0u64;
    let mut observed_dimension = None;
    let mut hybrid_count = 0u64;

    for (name, entity) in store.query_all(|_| true) {
        if name != collection {
            continue;
        }
        let EntityData::Vector(vector) = entity.data else {
            continue;
        };
        vector_count += 1;
        observed_dimension.get_or_insert_with(|| vector.dimension());
        if vector.is_hybrid() {
            hybrid_count += 1;
        }
    }

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "vector_count",
        Value::UnsignedInteger(vector_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "dimension",
        Value::UnsignedInteger(declared_dimension.or(observed_dimension).unwrap_or(0) as u64),
    ));
    if let Some(metric) = declared_metric {
        rows.push(stats_row(
            schema,
            collection,
            Value::Null,
            "distance_metric",
            Value::text(distance_metric_name(metric)),
        ));
    }
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "hybrid_vector_count",
        Value::UnsignedInteger(hybrid_count),
    ));
}

fn append_queue_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
) {
    let mut message_count = 0u64;
    let mut pending_count = 0u64;
    let mut delivered_count = 0u64;
    let mut acked_count = 0u64;
    let mut max_attempts_seen = 0u64;

    for (name, entity) in store.query_all(|_| true) {
        if name != collection {
            continue;
        }
        let EntityData::QueueMessage(message) = entity.data else {
            continue;
        };
        message_count += 1;
        max_attempts_seen = max_attempts_seen.max(message.attempts as u64);
        if message.acked {
            acked_count += 1;
        } else if message.attempts == 0 {
            pending_count += 1;
        } else {
            delivered_count += 1;
        }
    }

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "message_count",
        Value::UnsignedInteger(message_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "pending_count",
        Value::UnsignedInteger(pending_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "delivered_count",
        Value::UnsignedInteger(delivered_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "acked_count",
        Value::UnsignedInteger(acked_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "max_attempts_seen",
        Value::UnsignedInteger(max_attempts_seen),
    ));
}

fn append_timeseries_stats(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    store: &UnifiedStore,
    collection: &str,
) {
    let mut point_count = 0u64;
    let mut series_keys = std::collections::BTreeSet::<String>::new();
    let mut metric_counts = BTreeMap::<String, u64>::new();
    let mut oldest = None::<u64>;
    let mut newest = None::<u64>;

    for (name, entity) in store.query_all(|_| true) {
        if name != collection {
            continue;
        }
        let EntityData::TimeSeries(point) = entity.data else {
            continue;
        };
        point_count += 1;
        series_keys.insert(timeseries_series_key(&point.metric, &point.tags));
        *metric_counts.entry(point.metric.clone()).or_default() += 1;
        oldest = Some(oldest.map_or(point.timestamp_ns, |current| {
            current.min(point.timestamp_ns)
        }));
        newest = Some(newest.map_or(point.timestamp_ns, |current| {
            current.max(point.timestamp_ns)
        }));
    }

    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "point_count",
        Value::UnsignedInteger(point_count),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "series_count",
        Value::UnsignedInteger(series_keys.len() as u64),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "oldest_timestamp_ns",
        oldest.map(Value::UnsignedInteger).unwrap_or(Value::Null),
    ));
    rows.push(stats_row(
        schema,
        collection,
        Value::Null,
        "newest_timestamp_ns",
        newest.map(Value::UnsignedInteger).unwrap_or(Value::Null),
    ));
    for (metric, count) in metric_counts {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(metric),
            "metric_point_count",
            Value::UnsignedInteger(count),
        ));
    }
}

fn stats_row(
    schema: &Arc<Vec<Arc<str>>>,
    collection: &str,
    entity: Value,
    metric: &str,
    value: Value,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        Arc::clone(schema),
        vec![Value::text(collection), entity, Value::text(metric), value],
    )
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Integer(_) => "integer",
        Value::UnsignedInteger(_) => "unsigned_integer",
        Value::Float(_) => "float",
        Value::Text(_) => "text",
        Value::Blob(_) => "blob",
        Value::Boolean(_) => "boolean",
        Value::Timestamp(_) => "timestamp",
        Value::Duration(_) => "duration",
        Value::IpAddr(_) => "ipaddr",
        Value::MacAddr(_) => "macaddr",
        Value::Vector(_) => "vector",
        Value::Json(_) => "json",
        Value::Array(_) => "array",
        _ => "other",
    }
}

fn value_estimated_bytes(value: &Value) -> u64 {
    match value {
        Value::Null => 0,
        Value::Integer(value) => value.to_string().len() as u64,
        Value::UnsignedInteger(value) => value.to_string().len() as u64,
        Value::Float(value) => value.to_string().len() as u64,
        Value::Boolean(value) => value.to_string().len() as u64,
        Value::Text(value) => value.len() as u64,
        Value::Blob(value) | Value::Json(value) => value.len() as u64,
        Value::Vector(value) => (value.len() * std::mem::size_of::<f32>()) as u64,
        Value::Array(values) => values.iter().map(value_estimated_bytes).sum(),
        _ => format!("{value:?}").len() as u64,
    }
}

fn timeseries_series_key(metric: &str, tags: &std::collections::HashMap<String, String>) -> String {
    let mut key = metric.to_string();
    for (name, value) in tags.iter().collect::<BTreeMap<_, _>>() {
        key.push('|');
        key.push_str(name);
        key.push('=');
        key.push_str(value);
    }
    key
}

/// Extract the single collection targeted by a `collection = '<name>'`
/// equality filter (as produced by `SHOW STATS <name>`). Returns `None`
/// for any other shape so the caller profiles every visible row table.
fn stats_target_collection(query: &TableQuery) -> Option<String> {
    match query.filter.as_ref() {
        Some(Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op: CompareOp::Eq,
            value: Value::Text(collection),
        }) if column == "collection" => Some(collection.to_string()),
        _ => None,
    }
}

fn most_common_values(
    mcv: Option<&crate::storage::query::planner::MostCommonValues>,
) -> Vec<Value> {
    use crate::storage::query::planner::ColumnValue;
    let Some(mcv) = mcv else {
        return Vec::new();
    };
    mcv.values
        .iter()
        .take(STATS_MCV_LIMIT)
        .map(|(value, _freq)| match value {
            ColumnValue::Int(v) => Value::Integer(*v),
            ColumnValue::Float(v) => Value::Float(*v),
            ColumnValue::Text(v) => Value::text(v.clone()),
        })
        .collect()
}

pub(super) fn columns_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let db = runtime.db();
    let mut records = Vec::new();
    let schema = Arc::new(
        COLUMN_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let snapshot = db.catalog_model_snapshot();
    let contracts = db.collection_contracts();
    let contracts_by_name: HashMap<_, _> = contracts
        .iter()
        .map(|contract| (contract.name.as_str(), contract))
        .collect();

    for collection in snapshot.collections {
        if visible_collections.is_some_and(|visible| !visible.contains(&collection.name)) {
            continue;
        }
        let Some(contract) = contracts_by_name.get(collection.name.as_str()).copied() else {
            continue;
        };

        if !contract.declared_columns.is_empty() {
            records.extend(contract.declared_columns.iter().map(|column| {
                column_record(
                    Arc::clone(&schema),
                    &collection.name,
                    &column.name,
                    column
                        .sql_type
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| column.data_type.clone()),
                    !(column.not_null || column.primary_key),
                    column.default.as_deref(),
                    column.primary_key,
                    column.unique || column.primary_key,
                )
            }));
        } else if collection.model == CollectionModel::Document
            || contract.declared_model == CollectionModel::Document
        {
            records.extend(infer_document_columns(
                runtime,
                &collection.name,
                Arc::clone(&schema),
            ));
        }
    }

    records
}

fn column_record(
    schema: Arc<Vec<Arc<str>>>,
    collection: &str,
    name: &str,
    data_type: String,
    nullable: bool,
    default_value: Option<&str>,
    is_primary_key: bool,
    is_unique: bool,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(collection),
            Value::text(name),
            Value::text(data_type),
            Value::Boolean(nullable),
            default_value.map(Value::text).unwrap_or(Value::Null),
            Value::Boolean(is_primary_key),
            Value::Boolean(is_unique),
        ],
    )
}

#[derive(Debug, Clone)]
struct InferredColumn {
    data_type: Option<DataType>,
    seen: usize,
    saw_null: bool,
}

fn infer_document_columns(
    runtime: &RedDBRuntime,
    collection: &str,
    schema: Arc<Vec<Arc<str>>>,
) -> Vec<UnifiedRecord> {
    let mut fields: BTreeMap<String, InferredColumn> = BTreeMap::new();
    let mut document_count = 0usize;

    for (_, entity) in runtime
        .db()
        .store()
        .query_all(|entity| entity.kind.collection() == collection)
    {
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if !row
            .iter_fields()
            .any(|(name, value)| name == "body" && matches!(value, Value::Json(_) | Value::Text(_)))
        {
            continue;
        }

        document_count += 1;

        // Record every stored row field, plus the top-level fields derived
        // from the binary document body. Post-cutover (PRD-1398) documents
        // store the canonical document only inside the binary `body`
        // container and no longer promote top-level columns onto the row, so
        // schema inference must offset-read the body's top-level fields —
        // mirroring the GET presentation derive in `named_fields_json`.
        let mut recorded: Vec<(String, Value)> = row
            .iter_fields()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        if let Some(Value::Json(bytes)) = row.get_field("body") {
            if let Some(body_fields) = crate::document_body::body_fields(bytes) {
                recorded.extend(body_fields);
            }
        }

        for (name, value) in recorded {
            let entry = fields.entry(name).or_insert(InferredColumn {
                data_type: None,
                seen: 0,
                saw_null: false,
            });
            entry.seen += 1;
            if value.is_null() {
                entry.saw_null = true;
                continue;
            }
            let value_type = value.data_type();
            entry.data_type = match entry.data_type {
                None => Some(value_type),
                Some(existing) if existing == value_type => Some(existing),
                Some(_) => Some(DataType::Unknown),
            };
        }
    }

    if document_count == 0 {
        return Vec::new();
    }

    fields
        .into_iter()
        .map(|(name, inferred)| {
            let data_type = inferred
                .data_type
                .filter(|data_type| *data_type != DataType::Unknown)
                .map(|data_type| data_type.to_string())
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let nullable = inferred.saw_null || inferred.seen < document_count;
            column_record(
                Arc::clone(&schema),
                collection,
                &name,
                data_type,
                nullable,
                None,
                false,
                false,
            )
        })
        .collect()
}
