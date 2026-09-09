use std::collections::HashSet;

use crate::ast::CreateTableQuery;
use reddb_types::types::{DataType, SqlTypeName};

#[derive(Debug, Clone)]
pub enum AnalysisError {
    DuplicateColumn(String),
    UnsupportedType(String),
    InvalidUniqueConstraint(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateColumn(name) => write!(f, "duplicate column name: {name}"),
            Self::UnsupportedType(name) => write!(f, "unsupported SQL type: {name}"),
            Self::InvalidUniqueConstraint(message) => {
                write!(f, "invalid UNIQUE constraint: {message}")
            }
        }
    }
}

impl std::error::Error for AnalysisError {}

#[derive(Debug, Clone)]
pub struct AnalyzedCreateTableQuery {
    pub name: String,
    pub columns: Vec<AnalyzedColumnDef>,
    pub unique_constraints: Vec<reddb_types::Constraint>,
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
        unique_constraints: analyze_unique_constraints(query)?,
        if_not_exists: query.if_not_exists,
        default_ttl_ms: query.default_ttl_ms,
        context_index_fields: query.context_index_fields.clone(),
        timestamps: query.timestamps,
    })
}

fn analyze_unique_constraints(
    query: &CreateTableQuery,
) -> Result<Vec<reddb_types::Constraint>, AnalysisError> {
    let invalid = AnalysisError::InvalidUniqueConstraint;
    let mut names = HashSet::new();
    for column in &query.columns {
        for (enabled, prefix) in [
            (column.primary_key, "pk"),
            (column.unique, "uniq"),
            (column.not_null, "not_null"),
        ] {
            if enabled {
                names.insert(format!("{prefix}_{}", column.name).to_ascii_lowercase());
            }
        }
    }
    if query.timestamps {
        names.extend([
            "not_null_created_at".to_string(),
            "not_null_updated_at".to_string(),
        ]);
    }
    // Reserve every explicit name before generating anonymous names, so declaration
    // order cannot make an otherwise valid user-chosen name collide.
    for constraint in &query.unique_constraints {
        if let Some(name) = &constraint.name {
            if name.is_empty() || !names.insert(name.to_ascii_lowercase()) {
                return Err(invalid(format!(
                    "duplicate or empty constraint name '{name}'"
                )));
            }
        }
    }
    let mut resolved = Vec::with_capacity(query.unique_constraints.len());
    for constraint in &query.unique_constraints {
        if constraint.columns.is_empty() {
            return Err(invalid("at least one column is required".to_string()));
        }
        let mut seen = HashSet::new();
        let mut columns = Vec::with_capacity(constraint.columns.len());
        for name in &constraint.columns {
            let Some(column) = query
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
            else {
                return Err(invalid(format!("unknown column '{name}'")));
            };
            if !seen.insert(column.name.to_ascii_lowercase()) {
                return Err(invalid(format!("repeated column '{name}'")));
            }
            columns.push(column.name.clone());
        }
        let name = match &constraint.name {
            Some(name) => name.clone(),
            None => {
                let base = format!("uniq_{}", columns.join("_"));
                let mut name = base.clone();
                let mut suffix = 2;
                while !names.insert(name.to_ascii_lowercase()) {
                    name = format!("{base}_{suffix}");
                    suffix += 1;
                }
                name
            }
        };
        resolved.push(
            reddb_types::Constraint::new(name, reddb_types::ConstraintType::Unique)
                .on_columns(columns),
        );
    }
    Ok(resolved)
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
            generated: None,
            check: None,
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
            unique_constraints: Vec::new(),
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
    fn table_unique_constraints_validate_references_and_generate_stable_names() {
        use crate::ast::CreateUniqueConstraint;
        let mut query = create_table(vec![column("LeftKey", "INT"), column("RightKey", "TEXT")]);
        query.unique_constraints = vec![
            CreateUniqueConstraint {
                name: None,
                columns: vec!["leftkey".into(), "RIGHTKEY".into()],
            },
            CreateUniqueConstraint {
                name: Some("uniq_LeftKey_RightKey".into()),
                columns: vec!["RightKey".into(), "LeftKey".into()],
            },
        ];
        let analyzed = analyze_create_table(&query).expect("constraints");
        assert_eq!(
            analyzed.unique_constraints[0].name,
            "uniq_LeftKey_RightKey_2"
        );
        assert_eq!(
            analyzed.unique_constraints[0].columns,
            ["LeftKey", "RightKey"]
        );
        for columns in [
            vec![],
            vec!["absent".into()],
            vec!["LeftKey".into(), "leftkey".into()],
        ] {
            query.unique_constraints[0].columns = columns;
            assert!(matches!(
                analyze_create_table(&query),
                Err(AnalysisError::InvalidUniqueConstraint(_))
            ));
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
