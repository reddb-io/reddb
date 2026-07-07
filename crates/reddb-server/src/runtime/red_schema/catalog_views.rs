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
                    Value::Array(index.columns.into_iter().map(Value::text).collect()),
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
            status.collection.map(Value::text).unwrap_or(Value::Null),
            Value::text(status.name),
            Value::text(status.kind),
            Value::Boolean(status.declared),
            Value::Boolean(status.operational),
            Value::Boolean(status.enabled),
            status.build_state.map(Value::text).unwrap_or(Value::Null),
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
        super::index_store::IndexMethodKind::Spatial => "spatial.rtree",
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
        super::index_store::IndexMethodKind::Spatial => "RTREE",
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

/// Long-format `red.stats` profiling view (issue #1787). This is the
/// **computed** freshness tier: every read runs an on-demand profiling
/// scan over the target row tables rather than serving a cached
/// snapshot. Emitted rows are `(collection, entity, metric, value)`:
///
/// * `row_count` — one row per collection, `entity` is `NULL`.
/// * `null_count` / `distinct_count` / `most_common_values` — one row
///   per column, `entity` is the column name.
///
/// Document collections reuse the row-table metric set for their top-level
/// body fields and add document-only `coverage` / `type_histogram` rows.
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

        let analyzed = match collection.model {
            CollectionModel::Table => {
                crate::storage::query::planner::stats_catalog::analyze_collection(
                    store.as_ref(),
                    &collection.name,
                )
            }
            CollectionModel::Document => analyze_document_collection(runtime, &collection.name)
                .map(|stats| {
                    emit_document_metric_rows(
                        &mut rows,
                        &schema,
                        &collection.name,
                        stats.document_count,
                        stats.fields,
                    );
                    stats.analyzed
                }),
            _ => None,
        };
        let Some(analyzed) = analyzed else {
            continue;
        };

        rows.push(stats_row(
            &schema,
            &collection.name,
            Value::Null,
            "row_count",
            Value::UnsignedInteger(analyzed.row_count),
        ));
        for column in &analyzed.columns {
            rows.push(stats_row(
                &schema,
                &collection.name,
                Value::text(column.name.clone()),
                "null_count",
                Value::UnsignedInteger(column.null_count),
            ));
            rows.push(stats_row(
                &schema,
                &collection.name,
                Value::text(column.name.clone()),
                "distinct_count",
                Value::UnsignedInteger(column.distinct_count),
            ));
            rows.push(stats_row(
                &schema,
                &collection.name,
                Value::text(column.name.clone()),
                "most_common_values",
                Value::Array(most_common_values(column.mcv.as_ref())),
            ));
        }
    }
    rows
}

struct DocumentStats {
    document_count: u64,
    analyzed: crate::storage::query::planner::stats_catalog::AnalyzedTableStats,
    fields: BTreeMap<String, DocumentFieldStats>,
}

#[derive(Default)]
struct DocumentFieldStats {
    present_count: u64,
    type_counts: BTreeMap<String, u64>,
}

fn analyze_document_collection(runtime: &RedDBRuntime, collection: &str) -> Option<DocumentStats> {
    let mut fields = BTreeMap::<String, DocumentFieldStats>::new();
    let mut entities = Vec::new();

    for (_, entity) in runtime
        .db()
        .store()
        .query_all(|entity| entity.kind.collection() == collection)
    {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };

        let Some(body) = row.get_field("body") else {
            continue;
        };
        let Some(body_fields) = document_body_fields(body) else {
            continue;
        };

        let mut analyzed_fields = vec![("body".to_string(), body.clone())];
        for (name, value) in body_fields {
            let field = fields.entry(name.clone()).or_default();
            field.present_count += 1;
            *field
                .type_counts
                .entry(value.data_type().to_string())
                .or_insert(0) += 1;
            analyzed_fields.push((name, value));
        }
        entities.push((entity.id, analyzed_fields));
    }

    if entities.is_empty() {
        return None;
    }

    Some(DocumentStats {
        document_count: entities.len() as u64,
        analyzed: crate::storage::query::planner::stats_catalog::analyze_entity_fields(
            collection, &entities,
        ),
        fields,
    })
}

fn document_body_fields(body: &Value) -> Option<Vec<(String, Value)>> {
    match body {
        Value::Json(bytes) => crate::document_body::body_fields(bytes)
            .or_else(|| json_object_fields(crate::json::from_slice(bytes).ok()?)),
        Value::Text(text) => json_object_fields(crate::json::from_str(text.as_ref()).ok()?),
        _ => None,
    }
}

fn json_object_fields(value: crate::json::Value) -> Option<Vec<(String, Value)>> {
    let crate::json::Value::Object(map) = value else {
        return None;
    };
    Some(
        map.iter()
            .map(|(key, value)| {
                crate::application::entity::json_to_storage_value(value)
                    .ok()
                    .map(|value| (key.clone(), value))
            })
            .collect::<Option<Vec<_>>>()?,
    )
}

fn emit_document_metric_rows(
    rows: &mut Vec<UnifiedRecord>,
    schema: &Arc<Vec<Arc<str>>>,
    collection: &str,
    document_count: u64,
    fields: BTreeMap<String, DocumentFieldStats>,
) {
    for (name, field) in fields {
        rows.push(stats_row(
            schema,
            collection,
            Value::text(name.clone()),
            "coverage",
            Value::Float(field.present_count as f64 / document_count as f64),
        ));
        rows.push(stats_row(
            schema,
            collection,
            Value::text(name),
            "type_histogram",
            Value::Array(
                field
                    .type_counts
                    .into_iter()
                    .map(|(data_type, count)| {
                        Value::Array(vec![Value::text(data_type), Value::UnsignedInteger(count)])
                    })
                    .collect(),
            ),
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
