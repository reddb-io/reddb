//! Runtime-backed virtual `red.*` schema tables.
//!
//! The SQL parser does not currently accept schema-qualified table
//! identifiers in `FROM`, so the runtime rewrites the small virtual
//! surface it owns (`red.collections`, `red.columns`, `red.indices`,
//! `red.policies`, `red.stats`) to internal identifiers before normal parsing.
//! Execution then intercepts that identifier and materializes rows from the live catalog snapshot.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};
use crate::catalog::{CollectionModel, SchemaMode};
use crate::storage::query::ast::{Expr, FieldRef, Filter, PolicyAction, UnaryOp};
use crate::storage::query::sql_lowering::{effective_table_filter, effective_table_projections};
use crate::storage::schema::DataType;
use crate::storage::unified::EntityData;
use crate::storage::unified::UnifiedStore;

pub(super) const COLLECTIONS: &str = "red.collections";
pub(super) const COLLECTIONS_INTERNAL: &str = "__red_schema_collections";
pub(super) const COLUMNS: &str = "red.columns";
pub(super) const COLUMNS_INTERNAL: &str = "__red_schema_columns";
pub(super) const INDICES: &str = "red.indices";
pub(super) const INDICES_INTERNAL: &str = "__red_schema_indices";
pub(super) const POLICIES: &str = "red.policies";
pub(super) const POLICIES_INTERNAL: &str = "__red_schema_policies";
pub(super) const STATS: &str = "red.stats";
pub(super) const STATS_INTERNAL: &str = "__red_schema_stats";
pub(super) const READ_ONLY_ERROR: &str = "system schema is read-only";

const COLLECTION_COLUMNS: [&str; 9] = [
    "name",
    "model",
    "schema_mode",
    "entities",
    "segments",
    "indices",
    "in_memory_bytes",
    "internal",
    "tenant_id",
];

const COLUMN_COLUMNS: [&str; 7] = [
    "collection",
    "name",
    "type",
    "nullable",
    "default_value",
    "is_primary_key",
    "is_unique",
];

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

const STATS_COLUMNS: [&str; 10] = [
    "collection",
    "entities",
    "segments",
    "growing_count",
    "sealed_count",
    "archived_count",
    "seal_ops",
    "compact_ops",
    "last_write_ms",
    "attention_score",
];

pub(super) fn rewrite_virtual_names(query: &str) -> Option<String> {
    let mut rewritten = query.to_string();
    let mut changed = false;

    for (public, internal) in [
        (COLLECTIONS, COLLECTIONS_INTERNAL),
        (COLUMNS, COLUMNS_INTERNAL),
        (INDICES, INDICES_INTERNAL),
        (POLICIES, POLICIES_INTERNAL),
        (STATS, STATS_INTERNAL),
    ] {
        if let Some(next) = replace_case_insensitive_outside_quotes(&rewritten, public, internal) {
            rewritten = next;
            changed = true;
        }
    }

    changed.then_some(rewritten)
}

pub(super) fn references_system_schema(query: &str) -> bool {
    contains_case_insensitive_outside_quotes(query, "red.")
}

pub(super) fn is_system_schema_write(query: &str) -> bool {
    let trimmed = query.trim_start();
    let first = trimmed
        .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .next()
        .unwrap_or("");
    matches_ignore_ascii_case(first, &["INSERT", "UPDATE", "DELETE"])
        && references_system_schema(query)
}

pub(super) fn is_virtual_table(table: &str) -> bool {
    table.eq_ignore_ascii_case(COLLECTIONS_INTERNAL)
        || table.eq_ignore_ascii_case(COLLECTIONS)
        || table.eq_ignore_ascii_case(COLUMNS_INTERNAL)
        || table.eq_ignore_ascii_case(COLUMNS)
        || table.eq_ignore_ascii_case(INDICES_INTERNAL)
        || table.eq_ignore_ascii_case(INDICES)
        || table.eq_ignore_ascii_case(POLICIES_INTERNAL)
        || table.eq_ignore_ascii_case(POLICIES)
        || table.eq_ignore_ascii_case(STATS_INTERNAL)
        || table.eq_ignore_ascii_case(STATS)
}

pub(super) fn red_query(
    runtime: &RedDBRuntime,
    virtual_name: &str,
    query: &TableQuery,
    frame: &dyn super::statement_frame::ReadFrame,
) -> RedDBResult<UnifiedResult> {
    if !is_virtual_table(virtual_name) {
        return Err(RedDBError::Query(format!(
            "unknown system schema relation `{virtual_name}`"
        )));
    }
    let virtual_kind = virtual_table_kind(virtual_name)?;

    let caller_is_admin = frame.identity().is_some_and(|(_, role)| role.can_admin())
        || (frame.identity().is_none() && frame.effective_scope().is_none());
    if !caller_is_admin && frame.effective_scope().is_none() {
        return Err(RedDBError::Query(format!(
            "{} requires an active tenant",
            virtual_kind.public_name()
        )));
    }

    let tenant = frame.effective_scope();
    let visible_collections = if caller_is_admin {
        None
    } else {
        frame.visible_collections()
    };
    let db = runtime.db();
    let mut records = match virtual_kind {
        VirtualTableKind::Collections => collections_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Columns => columns_snapshot(runtime, visible_collections),
        VirtualTableKind::Indices => indices_snapshot(runtime, visible_collections),
        VirtualTableKind::Policies => policies_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Stats => stats_snapshot(runtime, visible_collections),
    };

    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref();
    if let Some(filter) = effective_table_filter(query) {
        records.retain(|record| {
            super::join_filter::evaluate_runtime_filter_with_db(
                Some(db.as_ref()),
                record,
                &filter,
                Some(table_name),
                table_alias,
            )
        });
    }

    if !query.order_by.is_empty() {
        super::join_filter::sort_records_by_order_by_with_db(
            Some(db.as_ref()),
            &mut records,
            &query.order_by,
            Some(table_name),
            table_alias,
        );
    }

    if let Some(offset) = query.offset {
        let offset = offset as usize;
        if offset >= records.len() {
            records.clear();
        } else {
            records.drain(..offset);
        }
    }
    if let Some(limit) = query.limit {
        records.truncate(limit as usize);
    }

    let projections = effective_table_projections(query);
    if !projections.is_empty()
        && !projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
    {
        records = records
            .iter()
            .map(|record| {
                super::join_filter::project_runtime_record_with_db(
                    Some(db.as_ref()),
                    record,
                    &projections,
                    Some(table_name),
                    table_alias,
                    false,
                    false,
                )
            })
            .collect();
    }

    let columns = if projections.is_empty()
        || projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
    {
        virtual_kind
            .columns()
            .iter()
            .map(|name| name.to_string())
            .collect()
    } else {
        super::join_filter::projected_columns(&records, &projections)
    };

    Ok(UnifiedResult {
        columns,
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: records.len() as u64,
            ..Default::default()
        },
        records,
        pre_serialized_json: None,
    })
}

#[derive(Debug, Clone, Copy)]
enum VirtualTableKind {
    Collections,
    Columns,
    Indices,
    Policies,
    Stats,
}

impl VirtualTableKind {
    fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Collections => &COLLECTION_COLUMNS,
            Self::Columns => &COLUMN_COLUMNS,
            Self::Indices => &INDEX_COLUMNS,
            Self::Policies => &POLICY_COLUMNS,
            Self::Stats => &STATS_COLUMNS,
        }
    }

    fn public_name(self) -> &'static str {
        match self {
            Self::Collections => COLLECTIONS,
            Self::Columns => COLUMNS,
            Self::Indices => INDICES,
            Self::Policies => POLICIES,
            Self::Stats => STATS,
        }
    }
}

fn virtual_table_kind(name: &str) -> RedDBResult<VirtualTableKind> {
    if name.eq_ignore_ascii_case(COLLECTIONS_INTERNAL) || name.eq_ignore_ascii_case(COLLECTIONS) {
        return Ok(VirtualTableKind::Collections);
    }
    if name.eq_ignore_ascii_case(COLUMNS_INTERNAL) || name.eq_ignore_ascii_case(COLUMNS) {
        return Ok(VirtualTableKind::Columns);
    }
    if name.eq_ignore_ascii_case(INDICES_INTERNAL) || name.eq_ignore_ascii_case(INDICES) {
        return Ok(VirtualTableKind::Indices);
    }
    if name.eq_ignore_ascii_case(POLICIES_INTERNAL) || name.eq_ignore_ascii_case(POLICIES) {
        return Ok(VirtualTableKind::Policies);
    }
    if name.eq_ignore_ascii_case(STATS_INTERNAL) || name.eq_ignore_ascii_case(STATS) {
        return Ok(VirtualTableKind::Stats);
    }
    Err(RedDBError::Query(format!(
        "unknown system schema relation `{name}`"
    )))
}

fn indices_snapshot(
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
    }
}

fn policies_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        POLICY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut records = Vec::new();

    let enabled = runtime.inner.rls_enabled_tables.read().clone();
    let rls_policies = runtime.inner.rls_policies.read();
    let mut rls_entries: Vec<_> = rls_policies.iter().collect();
    rls_entries.sort_by(
        |((left_collection, left_name), _), ((right_collection, right_name), _)| {
            left_collection
                .cmp(right_collection)
                .then_with(|| left_name.cmp(right_name))
        },
    );
    for ((collection, _), policy) in rls_entries {
        if !collection_is_visible(collection, visible_collections) {
            continue;
        }
        records.push(policy_record(
            &schema,
            policy.name.clone(),
            Some(collection.clone()),
            "rls",
            "allow",
            rls_actions(policy.action),
            rls_principals(policy.role.as_deref()),
            Value::text(render_filter_for_catalog(&policy.using)),
            Value::Boolean(enabled.contains(collection)),
        ));
    }
    drop(rls_policies);

    let auth_store = runtime.inner.auth_store.read().clone();
    if let Some(auth_store) = auth_store {
        for policy in auth_store.list_policies() {
            if !iam_policy_visible_to_tenant(&policy, tenant) {
                continue;
            }
            for (statement_index, statement) in policy.statements.iter().enumerate() {
                let collection_names = iam_statement_collections(statement);
                if collection_names.is_empty() {
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        None,
                    ));
                    continue;
                }
                for collection in collection_names {
                    if !collection_is_visible(&collection, visible_collections) {
                        continue;
                    }
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        Some(collection),
                    ));
                }
            }
        }
    }

    records
}

fn collection_is_visible(collection: &str, visible_collections: Option<&HashSet<String>>) -> bool {
    visible_collections.is_none_or(|visible| visible.contains(collection))
}

fn iam_policy_visible_to_tenant(policy: &Policy, tenant: Option<&str>) -> bool {
    match (tenant, policy.tenant.as_deref()) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(active), Some(policy_tenant)) => active == policy_tenant,
    }
}

fn policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    name: String,
    collection: Option<String>,
    kind: &'static str,
    effect: &'static str,
    actions: Vec<String>,
    principals: Vec<String>,
    predicate: Value,
    enabled: Value,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        Arc::clone(schema),
        vec![
            Value::text(name),
            collection.map(Value::text).unwrap_or(Value::Null),
            Value::text(kind),
            Value::text(effect),
            Value::Array(actions.into_iter().map(Value::text).collect()),
            Value::Array(principals.into_iter().map(Value::text).collect()),
            predicate,
            enabled,
        ],
    )
}

fn iam_policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    policy: &Policy,
    statement_index: usize,
    statement: &Statement,
    collection: Option<String>,
) -> UnifiedRecord {
    let name = statement
        .sid
        .as_ref()
        .map(|sid| format!("{}:{sid}", policy.id))
        .unwrap_or_else(|| {
            if policy.statements.len() > 1 {
                format!("{}#{}", policy.id, statement_index)
            } else {
                policy.id.clone()
            }
        });
    policy_record(
        schema,
        name,
        collection,
        "iam",
        iam_effect(statement.effect),
        iam_actions(&statement.actions),
        Vec::new(),
        Value::Null,
        Value::Boolean(true),
    )
}

fn rls_actions(action: Option<PolicyAction>) -> Vec<String> {
    match action {
        Some(PolicyAction::Select) => vec!["select".to_string()],
        Some(PolicyAction::Insert) => vec!["insert".to_string()],
        Some(PolicyAction::Update) => vec!["update".to_string()],
        Some(PolicyAction::Delete) => vec!["delete".to_string()],
        None => vec!["*".to_string()],
    }
}

fn rls_principals(role: Option<&str>) -> Vec<String> {
    role.map(|role| vec![role.to_string()])
        .unwrap_or_else(|| vec!["*".to_string()])
}

fn iam_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::Allow => "allow",
        Effect::Deny => "deny",
    }
}

fn iam_actions(actions: &[ActionPattern]) -> Vec<String> {
    actions.iter().map(render_action_pattern).collect()
}

fn render_action_pattern(action: &ActionPattern) -> String {
    match action {
        ActionPattern::Exact(value) => value.clone(),
        ActionPattern::Wildcard => "*".to_string(),
        ActionPattern::Prefix(prefix) => format!("{prefix}:*"),
    }
}

fn iam_statement_collections(statement: &Statement) -> Vec<String> {
    let mut out = Vec::new();
    for resource in &statement.resources {
        match resource {
            ResourcePattern::Exact { kind, name }
                if kind.eq_ignore_ascii_case("table")
                    || kind.eq_ignore_ascii_case("collection") =>
            {
                out.push(name.clone());
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

fn render_filter_for_catalog(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(field),
                op,
                crate::storage::query::renderer::render_value_sql(value)
            )
        }
        Filter::CompareFields { left, op, right } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(left),
                op,
                render_field_for_catalog(right)
            )
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            format!(
                "{} {} {}",
                render_expr_for_catalog(lhs),
                op,
                render_expr_for_catalog(rhs)
            )
        }
        Filter::And(left, right) => format!(
            "({}) AND ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Or(left, right) => format!(
            "({}) OR ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Not(inner) => format!("NOT ({})", render_filter_for_catalog(inner)),
        Filter::IsNull(field) => format!("{} IS NULL", render_field_for_catalog(field)),
        Filter::IsNotNull(field) => format!("{} IS NOT NULL", render_field_for_catalog(field)),
        Filter::In { field, values } => format!(
            "{} IN ({})",
            render_field_for_catalog(field),
            values
                .iter()
                .map(crate::storage::query::renderer::render_value_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Filter::Between { field, low, high } => format!(
            "{} BETWEEN {} AND {}",
            render_field_for_catalog(field),
            crate::storage::query::renderer::render_value_sql(low),
            crate::storage::query::renderer::render_value_sql(high)
        ),
        Filter::Like { field, pattern } => {
            format!("{} LIKE '{}'", render_field_for_catalog(field), pattern)
        }
        Filter::StartsWith { field, prefix } => {
            format!(
                "{} STARTS WITH '{}'",
                render_field_for_catalog(field),
                prefix
            )
        }
        Filter::EndsWith { field, suffix } => {
            format!("{} ENDS WITH '{}'", render_field_for_catalog(field), suffix)
        }
        Filter::Contains { field, substring } => {
            format!(
                "{} CONTAINS '{}'",
                render_field_for_catalog(field),
                substring
            )
        }
    }
}

fn render_expr_for_catalog(expr: &Expr) -> String {
    match expr {
        Expr::Literal { value, .. } => crate::storage::query::renderer::render_value_sql(value),
        Expr::Column { field, .. } => render_field_for_catalog(field),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::BinaryOp { op, lhs, rhs, .. } => format!(
            "{} {:?} {}",
            render_expr_for_catalog(lhs),
            op,
            render_expr_for_catalog(rhs)
        ),
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Not => format!("NOT {}", render_expr_for_catalog(operand)),
            UnaryOp::Neg => format!("-{}", render_expr_for_catalog(operand)),
        },
        Expr::Cast { inner, target, .. } => {
            format!("CAST({} AS {:?})", render_expr_for_catalog(inner), target)
        }
        Expr::FunctionCall { name, args, .. } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Case { .. } => format!("{expr:?}"),
        Expr::IsNull {
            operand, negated, ..
        } => format!(
            "{} IS {}NULL",
            render_expr_for_catalog(operand),
            if *negated { "NOT " } else { "" }
        ),
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => format!(
            "{} {}IN ({})",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            values
                .iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => format!(
            "{} {}BETWEEN {} AND {}",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            render_expr_for_catalog(low),
            render_expr_for_catalog(high)
        ),
    }
}

fn render_field_for_catalog(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } if table.is_empty() => column.clone(),
        FieldRef::TableColumn { table, column } => format!("{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

fn collections_snapshot(
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
                    Value::Boolean(internal),
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                ],
            )
        })
        .collect()
}

fn stats_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        STATS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .map(|collection| {
            let manager_stats = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats());
            let entities = manager_stats
                .as_ref()
                .map(|stats| stats.total_entities)
                .unwrap_or(collection.entities);
            let growing_count = manager_stats
                .as_ref()
                .map(|stats| stats.growing_count)
                .unwrap_or(0);
            let sealed_count = manager_stats
                .as_ref()
                .map(|stats| stats.sealed_count)
                .unwrap_or(0);
            let archived_count = manager_stats
                .as_ref()
                .map(|stats| stats.archived_count)
                .unwrap_or(0);
            let segments = manager_stats
                .as_ref()
                .map(|stats| stats.growing_count + stats.sealed_count + stats.archived_count)
                .unwrap_or(collection.segments);
            let seal_ops = manager_stats
                .as_ref()
                .map(|stats| stats.seal_ops)
                .unwrap_or(0);
            let compact_ops = manager_stats
                .as_ref()
                .map(|stats| stats.compact_ops)
                .unwrap_or(0);

            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(entities as u64),
                    Value::UnsignedInteger(segments as u64),
                    Value::UnsignedInteger(growing_count as u64),
                    Value::UnsignedInteger(sealed_count as u64),
                    Value::UnsignedInteger(archived_count as u64),
                    Value::UnsignedInteger(seal_ops),
                    Value::UnsignedInteger(compact_ops),
                    Value::Null,
                    Value::UnsignedInteger(collection.attention_score as u64),
                ],
            )
        })
        .collect()
}

struct InternalCollectionRegistry {
    dlqs: HashSet<String>,
}

impl InternalCollectionRegistry {
    fn from_store(store: &UnifiedStore) -> Self {
        Self {
            dlqs: discover_queue_dlqs(store),
        }
    }

    fn is_internal(&self, collection: &str) -> bool {
        collection.starts_with("red_")
            || collection == "audit_log"
            || collection == "__tenant_iso"
            || collection.starts_with("__tenant_")
            || collection.starts_with("__policy_")
            || self.dlqs.contains(collection)
    }
}

fn discover_queue_dlqs(store: &UnifiedStore) -> HashSet<String> {
    const QUEUE_META_COLLECTION: &str = "red_queue_meta";

    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return HashSet::new();
    };

    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_config"))
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            row_text(row, "dlq")
        })
        .collect()
}

fn columns_snapshot(
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
        for (name, value) in row.iter_fields() {
            let entry = fields.entry(name.to_string()).or_insert(InferredColumn {
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

fn row_text(row: &crate::storage::unified::entity::RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value),
        Value::EdgeRef(value) => Some(value),
        Value::TableRef(value) => Some(value),
        _ => None,
    }
}

fn collection_tenant(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
) -> Option<String> {
    match store.get_config(&format!("red.collection_tenants.{collection}")) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn collection_model_name(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Mixed => "mixed",
        CollectionModel::TimeSeries => "time_series",
        CollectionModel::Queue => "queue",
    }
}

fn schema_mode_name(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::Strict => "strict",
        SchemaMode::SemiStructured => "semi_structured",
        SchemaMode::Dynamic => "dynamic",
    }
}

fn contains_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> bool {
    find_case_insensitive_outside_quotes(haystack, needle).is_some()
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn replace_case_insensitive_outside_quotes(
    haystack: &str,
    needle: &str,
    replacement: &str,
) -> Option<String> {
    let mut out = String::new();
    let mut rest = haystack;
    let mut changed = false;

    while let Some(idx) = find_case_insensitive_outside_quotes(rest, needle) {
        out.push_str(&rest[..idx]);
        out.push_str(replacement);
        rest = &rest[idx + needle.len()..];
        changed = true;
    }

    if changed {
        out.push_str(rest);
        Some(out)
    } else {
        None
    }
}

fn find_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => {
                if in_single && bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_single = !in_single;
                i += 1;
                continue;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
                continue;
            }
            _ => {}
        }

        if !in_single
            && !in_double
            && i + needle_bytes.len() <= bytes.len()
            && bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_skips_quoted_literals() {
        let rewritten =
            rewrite_virtual_names("SELECT 'red.collections' AS x FROM red.collections").unwrap();
        assert_eq!(
            rewritten,
            "SELECT 'red.collections' AS x FROM __red_schema_collections"
        );
    }

    #[test]
    fn rewrite_handles_multiple_virtual_tables() {
        let rewritten = rewrite_virtual_names(
            "SELECT * FROM red.indices WHERE collection IN (SELECT name FROM red.collections)",
        )
        .unwrap();
        assert_eq!(
            rewritten,
            "SELECT * FROM __red_schema_indices WHERE collection IN (SELECT name FROM __red_schema_collections)"
        );
    }
}
