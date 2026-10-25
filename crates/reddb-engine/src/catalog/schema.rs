//! Native contract type resolution and stored-expression validation.
use super::{CollectionContract, DeclaredColumnContract};
use reddb_rql::analyzer::resolve_declared_data_type;
use reddb_rql::expr_typing::type_expr;
use reddb_types::DataType;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct ContractExpressionError {
    pub message: String,
}
impl ContractExpressionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ContractExpressionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "query error: {}", self.message)
    }
}
impl std::error::Error for ContractExpressionError {}
type SchemaResult<T> = Result<T, ContractExpressionError>;

pub struct ResolvedColumnRule {
    pub name: String,
    pub data_type: DataType,
    pub data_type_name: String,
    pub not_null: bool,
    pub default: Option<String>,
    pub enum_variants: Vec<String>,
}

pub fn resolved_contract_columns(
    contract: &CollectionContract,
) -> SchemaResult<Vec<ResolvedColumnRule>> {
    if let Some(table_def) = &contract.table_def {
        return Ok(table_def
            .columns
            .iter()
            .map(|column| ResolvedColumnRule {
                name: column.name.clone(),
                data_type: column.data_type,
                data_type_name: data_type_name(column.data_type).to_string(),
                not_null: !column.nullable,
                default: column
                    .default
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).to_string()),
                enum_variants: column.enum_variants.clone(),
            })
            .collect());
    }

    contract
        .declared_columns
        .iter()
        .map(|column| {
            let data_type = column
                .sql_type
                .as_ref()
                .map(reddb_rql::analyzer::resolve_sql_type_name)
                .transpose()
                .map_err(|err| ContractExpressionError::new(err.to_string()))?
                .unwrap_or(parse_declared_data_type(&column.data_type)?);
            Ok(ResolvedColumnRule {
                name: column.name.clone(),
                data_type,
                data_type_name: column.data_type.clone(),
                not_null: column.not_null,
                default: column.default.clone(),
                enum_variants: column.enum_variants.clone(),
            })
        })
        .collect()
}

fn parse_declared_data_type(value: &str) -> SchemaResult<DataType> {
    resolve_declared_data_type(value).map_err(|err| ContractExpressionError::new(err.to_string()))
}

fn data_type_name(data_type: DataType) -> &'static str {
    match data_type {
        DataType::Integer => "integer",
        DataType::UnsignedInteger => "unsigned_integer",
        DataType::Float => "float",
        DataType::Text => "text",
        DataType::Blob => "blob",
        DataType::Boolean => "boolean",
        DataType::Timestamp => "timestamp",
        DataType::Duration => "duration",
        DataType::IpAddr => "ipaddr",
        DataType::MacAddr => "macaddr",
        DataType::Vector => "vector",
        DataType::Nullable => "nullable",
        DataType::Unknown => "unknown",
        DataType::Json => "json",
        DataType::Uuid => "uuid",
        DataType::NodeRef => "noderef",
        DataType::EdgeRef => "edgeref",
        DataType::VectorRef => "vectorref",
        DataType::RowRef => "rowref",
        DataType::Color => "color",
        DataType::Email => "email",
        DataType::Url => "url",
        DataType::Phone => "phone",
        DataType::Semver => "semver",
        DataType::Cidr => "cidr",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Decimal => "decimal",
        DataType::DecimalText => "decimal_text",
        DataType::Enum => "enum",
        DataType::Array => "array",
        DataType::TimestampMs => "timestamp_ms",
        DataType::Ipv4 => "ipv4",
        DataType::Ipv6 => "ipv6",
        DataType::Subnet => "subnet",
        DataType::Port => "port",
        DataType::Latitude => "latitude",
        DataType::Longitude => "longitude",
        DataType::GeoPoint => "geopoint",
        DataType::Country2 => "country2",
        DataType::Country3 => "country3",
        DataType::Lang2 => "lang2",
        DataType::Lang5 => "lang5",
        DataType::Currency => "currency",
        DataType::AssetCode => "asset_code",
        DataType::Money => "money",
        DataType::ColorAlpha => "color_alpha",
        DataType::BigInt => "bigint",
        DataType::KeyRef => "keyref",
        DataType::DocRef => "docref",
        DataType::TableRef => "tableref",
        DataType::PageRef => "pageref",
        DataType::Secret => "secret",
        DataType::Password => "password",
        DataType::TextZstd => "text",
        DataType::BlobZstd => "blob",
    }
}

pub fn validate_contract_expressions(contract: &CollectionContract) -> SchemaResult<()> {
    let columns = resolved_contract_columns(contract)?;
    let scope = |table: &str, name: &str| {
        if !table.is_empty() {
            return None;
        }
        columns
            .iter()
            .find(|column| column.name == name)
            .map(|column| column.data_type)
    };
    for column in &contract.declared_columns {
        if column.generated.is_some() && column.default.is_some() {
            return Err(expression_error(
                &column.name,
                "GENERATED cannot have DEFAULT",
            ));
        }
        for (expression, check) in [(&column.generated, false), (&column.check, true)] {
            let Some(expression) = expression else {
                continue;
            };
            let typed = type_expr(expression.expression(), &scope)
                .map_err(|error| expression_error(&column.name, error))?;
            if check && !matches!(typed.ty, DataType::Boolean | DataType::Nullable) {
                return Err(expression_error(&column.name, "CHECK must produce BOOLEAN"));
            }
        }
    }
    generated_order(contract).map(|_| ())
}

fn expression_error(column: &str, error: impl std::fmt::Display) -> ContractExpressionError {
    ContractExpressionError::new(format!("column '{column}' expression: {error}"))
}

pub fn generated_order(
    contract: &CollectionContract,
) -> SchemaResult<Vec<&DeclaredColumnContract>> {
    let mut pending: Vec<_> = contract
        .declared_columns
        .iter()
        .filter(|column| column.generated.is_some())
        .collect();
    let mut ready: BTreeSet<&str> = contract
        .declared_columns
        .iter()
        .filter(|column| column.generated.is_none())
        .map(|column| column.name.as_str())
        .collect();
    let mut ordered = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let index = pending.iter().position(|column| {
            column.generated.as_ref().is_some_and(|expression| {
                expression
                    .dependencies()
                    .iter()
                    .all(|name| ready.contains(name.as_str()))
            })
        });
        let Some(index) = index else {
            return Err(ContractExpressionError::new(
                "generated field dependency cycle or unknown column".to_string(),
            ));
        };
        let column = pending.remove(index);
        ready.insert(&column.name);
        ordered.push(column);
    }
    Ok(ordered)
}

pub fn has_contract_expressions(contract: &CollectionContract) -> bool {
    contract
        .declared_columns
        .iter()
        .any(|column| column.generated.is_some() || column.check.is_some())
}
