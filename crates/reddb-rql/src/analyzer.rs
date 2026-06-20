use std::collections::HashSet;

use crate::ast::CreateTableQuery;
use reddb_types::types::{DataType, SqlTypeName};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{CreateColumnDef, CreateTableQuery};
    use reddb_types::catalog::CollectionModel;

    fn column(name: &str, declared: &str) -> CreateColumnDef {
        CreateColumnDef {
            name: name.to_string(),
            data_type: declared.to_string(),
            sql_type: SqlTypeName::parse_declared(declared),
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        }
    }

    fn create_table(columns: Vec<CreateColumnDef>) -> CreateTableQuery {
        CreateTableQuery {
            collection_model: CollectionModel::Table,
            name: "orders".to_string(),
            columns,
            if_not_exists: true,
            default_ttl_ms: Some(60_000),
            metrics_rollup_policies: Vec::new(),
            context_index_fields: vec!["description".to_string()],
            context_index_enabled: true,
            timestamps: true,
            partition_by: None,
            tenant_by: None,
            append_only: false,
            subscriptions: Vec::new(),
            analytics_config: Vec::new(),
            vault_own_master_key: false,
            ai_policy: None,
        }
    }

    #[test]
    fn analyze_create_table_resolves_columns_and_preserves_options() {
        let mut id = column("id", "INTEGER");
        id.primary_key = true;
        id.not_null = true;

        let mut description = column("description", "VARCHAR");
        description.default = Some("'new'".to_string());
        description.unique = true;

        let query = create_table(vec![id, description]);
        let analyzed = analyze_create_table(&query).unwrap();

        assert_eq!(analyzed.name, "orders");
        assert!(analyzed.if_not_exists);
        assert_eq!(analyzed.default_ttl_ms, Some(60_000));
        assert_eq!(analyzed.context_index_fields, ["description"]);
        assert!(analyzed.timestamps);
        assert_eq!(analyzed.columns.len(), 2);
        assert_eq!(analyzed.columns[0].name, "id");
        assert_eq!(analyzed.columns[0].storage_type, DataType::Integer);
        assert!(analyzed.columns[0].not_null);
        assert!(analyzed.columns[0].primary_key);
        assert_eq!(analyzed.columns[1].declared_type.base_name(), "VARCHAR");
        assert_eq!(analyzed.columns[1].storage_type, DataType::Text);
        assert_eq!(analyzed.columns[1].default.as_deref(), Some("'new'"));
        assert!(analyzed.columns[1].unique);
    }

    #[test]
    fn duplicate_columns_are_case_insensitive() {
        let query = create_table(vec![column("Id", "INT"), column("id", "INT")]);
        let err = analyze_create_table(&query).unwrap_err();

        assert!(matches!(err, AnalysisError::DuplicateColumn(ref name) if name == "id"));
        assert_eq!(err.to_string(), "duplicate column name: id");
    }

    #[test]
    fn unsupported_type_is_reported_with_normalized_name() {
        let query = create_table(vec![column("mystery", "not_a_real_type")]);
        let err = analyze_create_table(&query).unwrap_err();

        assert!(
            matches!(err, AnalysisError::UnsupportedType(ref name) if name == "NOT_A_REAL_TYPE")
        );
        assert_eq!(err.to_string(), "unsupported SQL type: NOT_A_REAL_TYPE");
    }

    #[test]
    fn resolve_declared_data_type_accepts_sql_aliases() {
        assert_eq!(
            resolve_declared_data_type("varchar").unwrap(),
            DataType::Text
        );
        assert_eq!(
            resolve_declared_data_type("numeric(10)").unwrap(),
            DataType::Decimal
        );
        assert_eq!(
            resolve_declared_data_type("timestamptz").unwrap(),
            DataType::TimestampMs
        );
        assert!(resolve_declared_data_type("definitely_not_sql").is_err());
    }
}
