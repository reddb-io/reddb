use std::collections::HashSet;

use super::CreateTableQuery;
use crate::storage::schema::{DataType, SqlTypeName};

#[derive(Debug, Clone)]
pub enum AnalysisError {
    DuplicateColumn(String),
    UnsupportedType(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateColumn(name) => write!(f, "duplicate column name: {name}"),
            Self::UnsupportedType(name) => write!(f, "unsupported SQL type: {name}"),
        }
    }
}

impl std::error::Error for AnalysisError {}

#[derive(Debug, Clone)]
pub struct AnalyzedCreateTableQuery {
    pub name: String,
    pub columns: Vec<AnalyzedColumnDef>,
    pub if_not_exists: bool,
    pub default_ttl_ms: Option<u64>,
    pub context_index_fields: Vec<String>,
    pub timestamps: bool,
}

#[derive(Debug, Clone)]
pub struct AnalyzedColumnDef {
    pub name: String,
    pub declared_type: SqlTypeName,
    pub storage_type: DataType,
    pub not_null: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub unique: bool,
}

pub fn analyze_create_table(
    query: &CreateTableQuery,
) -> Result<AnalyzedCreateTableQuery, AnalysisError> {
    let mut seen = HashSet::new();
    let mut columns = Vec::with_capacity(query.columns.len());

    for column in &query.columns {
        if !seen.insert(column.name.to_ascii_lowercase()) {
            return Err(AnalysisError::DuplicateColumn(column.name.clone()));
        }

        columns.push(AnalyzedColumnDef {
            name: column.name.clone(),
            declared_type: column.sql_type.clone(),
            storage_type: resolve_sql_type_name(&column.sql_type)?,
            not_null: column.not_null,
            default: column.default.clone(),
            primary_key: column.primary_key,
            unique: column.unique,
        });
    }

    Ok(AnalyzedCreateTableQuery {
        name: query.name.clone(),
        columns,
        if_not_exists: query.if_not_exists,
        default_ttl_ms: query.default_ttl_ms,
        context_index_fields: query.context_index_fields.clone(),
        timestamps: query.timestamps,
    })
}

pub fn resolve_declared_data_type(declared: &str) -> Result<DataType, AnalysisError> {
    resolve_sql_type_name(&SqlTypeName::parse_declared(declared))
}

pub fn resolve_sql_type_name(sql_type: &SqlTypeName) -> Result<DataType, AnalysisError> {
    DataType::from_sql_type_name(sql_type)
        .ok_or_else(|| AnalysisError::UnsupportedType(sql_type.base_name()))
}
