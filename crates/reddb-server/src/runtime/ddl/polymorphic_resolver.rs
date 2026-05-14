use crate::catalog::{CatalogModelSnapshot, CollectionModel};
use crate::{RedDBError, RedDBResult};

pub(crate) fn resolve(name: &str, snapshot: &CatalogModelSnapshot) -> RedDBResult<CollectionModel> {
    snapshot
        .collections
        .iter()
        .find(|collection| collection.name == name)
        .map(|collection| collection.declared_model.unwrap_or(collection.model))
        .ok_or_else(|| RedDBError::NotFound(format!("collection '{name}' not found")))
}

pub(crate) fn model_name(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Hll => "hll",
        CollectionModel::Sketch => "sketch",
        CollectionModel::Filter => "filter",
        CollectionModel::Kv => "kv",
        CollectionModel::Config => "config",
        CollectionModel::Vault => "vault",
        CollectionModel::Mixed => "mixed",
        CollectionModel::TimeSeries => "timeseries",
        CollectionModel::Queue => "queue",
        CollectionModel::Metrics => "metrics",
    }
}

pub(crate) fn ensure_model_match(
    expected: CollectionModel,
    actual: CollectionModel,
) -> RedDBResult<()> {
    if actual == expected {
        return Ok(());
    }
    Err(RedDBError::InvalidOperation(format!(
        "model mismatch: expected {}, got {}",
        model_name(expected),
        model_name(actual)
    )))
}
