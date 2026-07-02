//! DML UPDATE/INSERT target-contract guards extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1633); `pub(super)` visibility keeps the
//! sibling `impl_dml` call sites unchanged.

use super::impl_dml::{CompiledUpdatePlan, MaterializedUpdateAssignments};
use super::impl_dml_update_analysis::expr_references_update_column;
use super::*;
use crate::storage::query::ast::UpdateTarget;

/// Reject UPDATE … NODES/EDGES that assign to graph identity/topology
/// columns regardless of whether any row matches the WHERE clause. The
/// per-entity guard below covers only the matched-rows case, but ADR 0019
/// declares these columns immutable on the surface itself, so a zero-row
/// UPDATE should still surface the same error to operators and SDKs.
pub(super) fn ensure_graph_identity_update_target_allowed(query: &UpdateQuery) -> RedDBResult<()> {
    if !matches!(query.target, UpdateTarget::Nodes | UpdateTarget::Edges) {
        return Ok(());
    }
    for (column, _) in &query.assignment_exprs {
        if is_immutable_graph_identity_field(column) {
            return Err(RedDBError::Query(format!(
                "immutable graph field '{column}' cannot be updated"
            )));
        }
    }
    Ok(())
}

pub(super) fn ensure_kv_key_update_target_allowed(query: &UpdateQuery) -> RedDBResult<()> {
    if !matches!(query.target, UpdateTarget::Kv) {
        return Ok(());
    }
    for (column, _) in &query.assignment_exprs {
        if column.eq_ignore_ascii_case("key") {
            return Err(RedDBError::Query(
                "KV key cannot be updated; delete and insert a new key instead".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn ensure_graph_identity_update_allowed(
    entity: &UnifiedEntity,
    compiled_plan: &CompiledUpdatePlan,
    assignments: &MaterializedUpdateAssignments,
) -> RedDBResult<()> {
    if !matches!(entity.data, EntityData::Node(_) | EntityData::Edge(_)) {
        return Ok(());
    }

    for (column, _) in compiled_plan
        .static_field_assignments
        .iter()
        .chain(assignments.dynamic_field_assignments.iter())
    {
        if is_immutable_graph_identity_field(column) {
            return Err(RedDBError::Query(format!(
                "immutable graph field '{column}' cannot be updated"
            )));
        }
    }

    Ok(())
}

pub(super) fn is_immutable_graph_identity_field(column: &str) -> bool {
    ["rid", "label", "from_rid", "to_rid", "from", "to"]
        .iter()
        .any(|reserved| column.eq_ignore_ascii_case(reserved))
}

pub(super) fn ensure_update_target_contract(
    runtime: &RedDBRuntime,
    collection: &str,
    target: UpdateTarget,
) -> RedDBResult<()> {
    let Some(contract) = runtime.db().collection_contract(collection) else {
        return Ok(());
    };
    if update_target_contract_is_advisory(&contract)
        || update_target_allows_model(contract.declared_model, update_target_model(target))
    {
        return Ok(());
    }
    Err(RedDBError::InvalidOperation(format!(
        "collection '{}' is declared as '{}' and does not allow '{}' updates",
        collection,
        update_model_name(contract.declared_model),
        update_model_name(update_target_model(target))
    )))
}

pub(super) fn update_target_contract_is_advisory(
    contract: &crate::physical::CollectionContract,
) -> bool {
    matches!(
        (&contract.origin, &contract.schema_mode),
        (
            crate::physical::ContractOrigin::Implicit,
            crate::catalog::SchemaMode::Dynamic,
        )
    )
}

pub(super) fn update_target_model(target: UpdateTarget) -> crate::catalog::CollectionModel {
    match target {
        UpdateTarget::Rows => crate::catalog::CollectionModel::Table,
        UpdateTarget::Documents => crate::catalog::CollectionModel::Document,
        UpdateTarget::Kv => crate::catalog::CollectionModel::Kv,
        UpdateTarget::Nodes | UpdateTarget::Edges => crate::catalog::CollectionModel::Graph,
    }
}

pub(super) fn update_target_allows_model(
    declared_model: crate::catalog::CollectionModel,
    requested_model: crate::catalog::CollectionModel,
) -> bool {
    declared_model == requested_model || declared_model == crate::catalog::CollectionModel::Mixed
}

pub(super) fn update_model_name(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Table => "table",
        crate::catalog::CollectionModel::Document => "document",
        crate::catalog::CollectionModel::Graph => "graph",
        crate::catalog::CollectionModel::Vector => "vector",
        crate::catalog::CollectionModel::Hll => "hll",
        crate::catalog::CollectionModel::Sketch => "sketch",
        crate::catalog::CollectionModel::Filter => "filter",
        crate::catalog::CollectionModel::Kv => "kv",
        crate::catalog::CollectionModel::Config => "config",
        crate::catalog::CollectionModel::Vault => "vault",
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
        crate::catalog::CollectionModel::Metrics => "metrics",
    }
}

pub(super) fn ensure_graph_insert_contract(
    runtime: &RedDBRuntime,
    collection: &str,
) -> RedDBResult<()> {
    let db = runtime.db();
    if let Some(contract) = db.collection_contract(collection) {
        let advisory_implicit_dynamic = matches!(
            (&contract.origin, &contract.schema_mode),
            (
                crate::physical::ContractOrigin::Implicit,
                crate::catalog::SchemaMode::Dynamic,
            )
        );
        if advisory_implicit_dynamic
            || matches!(
                contract.declared_model,
                crate::catalog::CollectionModel::Graph | crate::catalog::CollectionModel::Mixed
            )
        {
            return Ok(());
        }
        return Err(RedDBError::InvalidOperation(format!(
            "collection '{}' is declared as '{:?}' and does not allow 'Graph' writes",
            collection, contract.declared_model
        )));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    db.save_collection_contract(crate::physical::CollectionContract {
        name: collection.to_string(),
        declared_model: crate::catalog::CollectionModel::Graph,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: db.collection_default_ttl_ms(collection),
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
    })
    .map(|_| ())
    .map_err(|err| RedDBError::Internal(err.to_string()))
}

pub(super) fn update_needs_rmw_lock(query: &UpdateQuery) -> bool {
    query
        .assignment_exprs
        .iter()
        .enumerate()
        .any(|(idx, (column, expr))| {
            query
                .compound_assignment_ops
                .get(idx)
                .is_some_and(|op| op.is_some())
                || expr_references_update_column(expr, &query.table, column)
        })
}
