//! Single-owner SELECT over a committed native artifact. This is a local file-owner
//! API, not a server principal/tenant session or an application transaction engine.
use crate::catalog::enforcement::{
    INTERNAL_EXPIRES_AT_KEY, INTERNAL_TTL_MILLIS_KEY, INTERNAL_TTL_SECONDS_KEY,
};
use crate::catalog::{
    codec::deserialize_collection_contracts, CollectionContract, CollectionModel,
};
use crate::storage::{
    entity::EntityData, error::StoreError, manager::ManagerConfig,
    resident_file::EmbeddedResidentStore,
};
use reddb_file::VfsFile;
use std::{collections::HashMap, path::PathBuf};

use super::{
    evaluator::EvalError,
    planning::build_table_plan_with_host,
    runtime::{projection::projected_columns, table_plan::execute_table_plan_with_host},
    scalar::ScalarResult,
};

pub(crate) mod admission;
pub(crate) mod host;

/// Bound materialization before cloning records, independently of SQL LIMIT.
pub const LOCAL_READ_ROWS_MAX: usize = 100_000;

#[derive(Debug)]
pub enum LocalReadError {
    Storage(StoreError),
    Catalog(String),
    Parse(reddb_rql::ParseError),
    Unsupported(&'static str),
    Evaluation(EvalError),
    Query(String),
    Parameters(super::parameters::UserParamError),
    ResourceLimit,
}

impl LocalReadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Storage(_) => "DATABASE_ERROR",
            Self::Catalog(_) => "CATALOG_ERROR",
            Self::Parse(_) => "QUERY_PARSE_ERROR",
            Self::Unsupported(_) => "QUERY_UNSUPPORTED",
            Self::Evaluation(_) | Self::Query(_) => "QUERY_EVALUATION_ERROR",
            Self::ResourceLimit => "QUERY_RESOURCE_LIMIT",
            Self::Parameters(_) => "INVALID_PARAMS",
        }
    }
}

impl std::fmt::Display for LocalReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::Catalog(message) | Self::Query(message) => formatter.write_str(message),
            Self::Parse(error) => error.fmt(formatter),
            Self::Unsupported(message) => formatter.write_str(message),
            Self::Evaluation(error) => error.fmt(formatter),
            Self::Parameters(error) => error.fmt(formatter),
            Self::ResourceLimit => {
                formatter.write_str("local SELECT materialization exceeds 100000 rows")
            }
        }
    }
}
impl std::error::Error for LocalReadError {}
impl From<StoreError> for LocalReadError {
    fn from(error: StoreError) -> Self {
        Self::Storage(error)
    }
}
impl From<EvalError> for LocalReadError {
    fn from(error: EvalError) -> Self {
        Self::Evaluation(error)
    }
}

/// Holds the exclusively owned file and reconstructs snapshot + committed WAL once.
/// Callers retain the file lock until this value is dropped. No mutator is exposed.
pub struct LocalReadDatabase<F: VfsFile> {
    store: EmbeddedResidentStore<F>,
    contracts: HashMap<String, CollectionContract>,
}

impl<F: VfsFile> LocalReadDatabase<F> {
    pub fn open(file: F, name: impl Into<PathBuf>) -> Result<Self, LocalReadError> {
        let store = EmbeddedResidentStore::open(file, name, ManagerConfig::default(), false, 0)?;
        let metadata = store.committed_resident()?.aux_metadata();
        let mut contracts = HashMap::new();
        if !metadata.is_empty() {
            for contract in deserialize_collection_contracts(&metadata)
                .map_err(|error| LocalReadError::Catalog(error.to_string()))?
            {
                let name = contract.name.clone();
                if contracts.insert(name.clone(), contract).is_some() {
                    return Err(LocalReadError::Catalog(format!(
                        "duplicate collection contract: {name}"
                    )));
                }
            }
        }
        Ok(Self { store, contracts })
    }

    pub fn query(&self, sql: &str) -> Result<ScalarResult, LocalReadError> {
        query_resident(self.store.committed_resident()?, &self.contracts, sql)
    }
}

pub(crate) fn query_resident(
    resident: &crate::storage::resident_store::ResidentStore,
    contracts: &HashMap<String, CollectionContract>,
    sql: &str,
) -> Result<ScalarResult, LocalReadError> {
    let query = admission::query(sql)?;
    execute_resident_query(resident, contracts, query)
}

pub(crate) fn query_resident_bound(
    resident: &crate::storage::resident_store::ResidentStore,
    contracts: &HashMap<String, CollectionContract>,
    query: reddb_rql::ast::TableQuery,
) -> Result<ScalarResult, LocalReadError> {
    execute_resident_query(resident, contracts, admission::table(query)?)
}

fn execute_resident_query(
    resident: &crate::storage::resident_store::ResidentStore,
    contracts: &HashMap<String, CollectionContract>,
    query: reddb_rql::ast::TableQuery,
) -> Result<ScalarResult, LocalReadError> {
    let contract = contracts.get(&query.table).ok_or_else(|| {
        LocalReadError::Catalog(format!(
            "no persisted collection contract for {}",
            query.table
        ))
    })?;
    if contract.declared_model != CollectionModel::Table
        || contract.default_ttl_ms.is_some()
        || contract.retention_duration_ms.is_some()
    {
        return Err(LocalReadError::Unsupported(
            "local SELECT currently requires a table without TTL or retention",
        ));
    }
    let columns = crate::catalog::schema::resolved_contract_columns(contract)
        .map_err(|error| LocalReadError::Catalog(error.to_string()))?;
    admission::expressions(&query, &columns)?;
    let manager = resident
        .get_collection(&query.table)
        .ok_or_else(|| LocalReadError::Catalog(format!("collection {} is missing", query.table)))?;
    let mut entities = Vec::new();
    let mut exceeded = false;
    manager.scan_for_each(None, |entity| {
        if entities.len() == LOCAL_READ_ROWS_MAX {
            exceeded = true;
            return false;
        }
        entities.push(entity.clone());
        true
    });
    if exceeded {
        return Err(LocalReadError::ResourceLimit);
    }
    let schema = std::sync::Arc::new(columns.iter().map(|column| column.name.clone()).collect());
    let mut records = Vec::with_capacity(entities.len());
    for entity in entities {
        if !matches!(entity.data, EntityData::Row(_)) {
            return Err(LocalReadError::Unsupported(
                "local table contains non-row entities",
            ));
        }
        if manager.get_metadata(entity.id).is_some_and(|metadata| {
            metadata.has(INTERNAL_TTL_SECONDS_KEY)
                || metadata.has(INTERNAL_TTL_MILLIS_KEY)
                || metadata.has(INTERNAL_EXPIRES_AT_KEY)
        }) {
            return Err(LocalReadError::Unsupported(
                "local SELECT does not yet admit row TTL metadata",
            ));
        }
        if let Some(record) =
            super::records::runtime_table_record_from_entity_ref_with_schema(&entity, Some(&schema))
        {
            records.push(record);
        }
    }
    let projections = reddb_rql::sql_lowering::effective_table_projections(&query);
    let host = host::ReadHost::new(&query, &columns, &projections, records);
    let plan = build_table_plan_with_host(&host, &query);
    let records = execute_table_plan_with_host(&host, &plan, &query)?;
    let columns = projected_columns(&records, &projections);
    let rows = records
        .iter()
        .map(|record| {
            columns
                .iter()
                .map(|column| {
                    record
                        .get(column)
                        .cloned()
                        .unwrap_or(reddb_types::Value::Null)
                })
                .collect()
        })
        .collect();
    Ok(ScalarResult { columns, rows })
}
