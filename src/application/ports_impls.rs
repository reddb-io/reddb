pub(crate) use super::*;

/// Returns `true` when a collection contract should enforce its declared model.
///
/// Implicit + Dynamic contracts are placeholders stamped by the first write — they
/// represent a best-effort observation, not a user commitment, so the model stays
/// advisory until someone promotes the collection via explicit DDL or a stricter
/// schema mode.
pub(super) fn contract_enforces_model(contract: &crate::physical::CollectionContract) -> bool {
    !matches!(
        (&contract.origin, &contract.schema_mode),
        (
            crate::physical::ContractOrigin::Implicit,
            crate::catalog::SchemaMode::Dynamic,
        )
    )
}

pub(super) fn collection_model_allows_shared(
    declared_model: crate::catalog::CollectionModel,
    requested_model: crate::catalog::CollectionModel,
) -> bool {
    declared_model == requested_model || declared_model == crate::catalog::CollectionModel::Mixed
}

pub(super) fn collection_model_name_shared(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Table => "table",
        crate::catalog::CollectionModel::Document => "document",
        crate::catalog::CollectionModel::Graph => "graph",
        crate::catalog::CollectionModel::Vector => "vector",
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
    }
}

/// Enforces the declared collection model on the read path.
///
/// Unlike the write-path sibling, this never creates a contract — a missing contract
/// means there is nothing to enforce yet. Callers should pass the model the read
/// surface represents (e.g. `Vector` for `search_similar`, `Table` for `get_kv`).
pub(super) fn ensure_collection_model_read(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    requested_model: crate::catalog::CollectionModel,
) -> crate::RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    if !contract_enforces_model(&contract) {
        return Ok(());
    }
    if collection_model_allows_shared(contract.declared_model, requested_model) {
        return Ok(());
    }
    Err(crate::RedDBError::Query(format!(
        "collection '{}' is declared as '{}' and does not allow '{}' reads",
        collection,
        collection_model_name_shared(contract.declared_model),
        collection_model_name_shared(requested_model)
    )))
}

#[path = "ports_impls_admin.rs"]
mod admin;
#[path = "ports_impls_catalog.rs"]
mod catalog;
#[path = "ports_impls_entity.rs"]
mod entity;
pub(crate) use entity::build_row_update_contract_plan;
pub(crate) use entity::normalize_row_update_assignment_with_plan;
pub(crate) use entity::normalize_row_update_value_for_rule;
#[path = "ports_impls_graph.rs"]
mod graph;
#[path = "ports_impls_native.rs"]
mod native;
#[path = "ports_impls_query.rs"]
mod query;
#[path = "ports_impls_schema.rs"]
mod schema;
#[path = "ports_impls_tree.rs"]
mod tree;
#[path = "ports_impls_vcs.rs"]
mod vcs;
