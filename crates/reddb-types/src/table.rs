//! Logical SQL table definitions.
//!
//! The file artifact crate owns the durable byte layout for these types. This
//! module owns only the logical vocabulary and validation rules.

use crate::DataType;
use std::collections::HashMap;
use std::fmt;

/// Table definition containing all metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDef {
    /// Table name (unique within database).
    pub name: String,
    /// Column definitions in order.
    pub columns: Vec<ColumnDef>,
    /// Primary key column names (can be composite).
    pub primary_key: Vec<String>,
    /// Index definitions.
    pub indexes: Vec<IndexDef>,
    /// Table-level constraints.
    pub constraints: Vec<Constraint>,
    /// Schema version (for migrations).
    pub version: u32,
    /// Creation timestamp.
    pub created_at: u64,
    /// Last modification timestamp.
    pub updated_at: u64,
}

impl TableDef {
    /// Create a new table definition.
    pub fn new(name: impl Into<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            name: name.into(),
            columns: Vec::new(),
            primary_key: Vec::new(),
            indexes: Vec::new(),
            constraints: Vec::new(),
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a column to the table.
    pub fn add_column(mut self, column: ColumnDef) -> Self {
        self.columns.push(column);
        self
    }

    /// Set primary key columns.
    pub fn primary_key(mut self, columns: Vec<String>) -> Self {
        self.primary_key = columns;
        self
    }

    /// Add an index.
    pub fn add_index(mut self, index: IndexDef) -> Self {
        self.indexes.push(index);
        self
    }

    /// Add a constraint.
    pub fn add_constraint(mut self, constraint: Constraint) -> Self {
        self.constraints.push(constraint);
        self
    }

    /// Get a column by name.
    pub fn get_column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// Get a column's ordinal by name.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }

    /// Check whether a column is part of the primary key.
    pub fn is_primary_key_column(&self, name: &str) -> bool {
        self.primary_key.iter().any(|column| column == name)
    }

    /// Validate references within the table definition.
    pub fn validate(&self) -> Result<(), TableDefError> {
        if self.name.is_empty() {
            return Err(TableDefError::EmptyTableName);
        }

        let mut seen = HashMap::new();
        for column in &self.columns {
            if seen.insert(&column.name, true).is_some() {
                return Err(TableDefError::DuplicateColumn(column.name.clone()));
            }
        }

        for primary_key_column in &self.primary_key {
            if self.get_column(primary_key_column).is_none() {
                return Err(TableDefError::InvalidPrimaryKey(primary_key_column.clone()));
            }
        }

        for index in &self.indexes {
            for column in &index.columns {
                if self.get_column(column).is_none() {
                    return Err(TableDefError::InvalidIndexColumn(column.clone()));
                }
            }
        }

        for constraint in &self.constraints {
            for column in &constraint.columns {
                if self.get_column(column).is_none() {
                    return Err(TableDefError::InvalidConstraintColumn(column.clone()));
                }
            }
        }

        Ok(())
    }
}

impl fmt::Display for TableDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "TABLE {} (version {})", self.name, self.version)?;
        writeln!(f, "  Columns:")?;
        for column in &self.columns {
            writeln!(f, "    {column}")?;
        }
        if !self.primary_key.is_empty() {
            writeln!(f, "  Primary Key: ({})", self.primary_key.join(", "))?;
        }
        if !self.indexes.is_empty() {
            writeln!(f, "  Indexes:")?;
            for index in &self.indexes {
                writeln!(f, "    {index}")?;
            }
        }
        Ok(())
    }
}

/// Column definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    /// Column name.
    pub name: String,
    /// Data type.
    pub data_type: DataType,
    /// Whether NULL values are allowed.
    pub nullable: bool,
    /// Default value (serialized).
    pub default: Option<Vec<u8>>,
    /// Vector dimension (for vector columns).
    pub vector_dim: Option<u32>,
    /// Whether to compress this column's data.
    pub compress: bool,
    /// Valid enum variants.
    pub enum_variants: Vec<String>,
    /// Number of decimal places.
    pub decimal_precision: u8,
    /// Array element data type.
    pub element_type: Option<DataType>,
    /// Additional column metadata.
    pub metadata: HashMap<String, String>,
}

impl ColumnDef {
    /// Create a new column definition.
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: true,
            default: None,
            vector_dim: None,
            compress: false,
            enum_variants: Vec::new(),
            decimal_precision: 4,
            element_type: None,
            metadata: HashMap::new(),
        }
    }

    /// Make the column non-nullable.
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Set the serialized default value.
    pub fn with_default(mut self, default: Vec<u8>) -> Self {
        self.default = Some(default);
        self
    }

    /// Set the vector dimension.
    pub fn with_vector_dim(mut self, dimension: u32) -> Self {
        self.vector_dim = Some(dimension);
        self
    }

    /// Add a metadata entry.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Enable per-column compression.
    pub fn compressed(mut self) -> Self {
        self.compress = true;
        self
    }

    /// Set enum variants.
    pub fn with_variants(mut self, variants: Vec<String>) -> Self {
        self.enum_variants = variants;
        self
    }

    /// Set decimal precision.
    pub fn with_precision(mut self, precision: u8) -> Self {
        self.decimal_precision = precision;
        self
    }

    /// Set the array element type.
    pub fn with_element_type(mut self, data_type: DataType) -> Self {
        self.element_type = Some(data_type);
        self
    }
}

impl fmt::Display for ColumnDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.data_type)?;
        if let Some(dimension) = self.vector_dim {
            write!(f, "({dimension})")?;
        }
        if !self.nullable {
            write!(f, " NOT NULL")?;
        }
        if self.default.is_some() {
            write!(f, " DEFAULT <value>")?;
        }
        Ok(())
    }
}

/// Index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub index_type: IndexType,
    pub unique: bool,
}

impl IndexDef {
    pub fn new(name: impl Into<String>, columns: Vec<String>) -> Self {
        Self {
            name: name.into(),
            columns,
            index_type: IndexType::BTree,
            unique: false,
        }
    }

    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    pub fn with_type(mut self, index_type: IndexType) -> Self {
        self.index_type = index_type;
        self
    }
}

impl fmt::Display for IndexDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.unique {
            write!(f, "UNIQUE ")?;
        }
        write!(
            f,
            "INDEX {} ({}) USING {:?}",
            self.name,
            self.columns.join(", "),
            self.index_type
        )
    }
}

/// Index type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IndexType {
    BTree = 1,
    Hash = 2,
    IvfFlat = 3,
    Hnsw = 4,
}

impl IndexType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::BTree),
            2 => Some(Self::Hash),
            3 => Some(Self::IvfFlat),
            4 => Some(Self::Hnsw),
            _ => None,
        }
    }
}

/// Constraint definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub name: String,
    pub constraint_type: ConstraintType,
    pub columns: Vec<String>,
    pub ref_table: Option<String>,
    pub ref_columns: Option<Vec<String>>,
}

impl Constraint {
    pub fn new(name: impl Into<String>, constraint_type: ConstraintType) -> Self {
        Self {
            name: name.into(),
            constraint_type,
            columns: Vec::new(),
            ref_table: None,
            ref_columns: None,
        }
    }

    pub fn on_columns(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
        self
    }

    pub fn references(mut self, table: String, columns: Vec<String>) -> Self {
        self.ref_table = Some(table);
        self.ref_columns = Some(columns);
        self
    }
}

/// Constraint type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConstraintType {
    PrimaryKey = 1,
    Unique = 2,
    ForeignKey = 3,
    Check = 4,
    NotNull = 5,
}

impl ConstraintType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::PrimaryKey),
            2 => Some(Self::Unique),
            3 => Some(Self::ForeignKey),
            4 => Some(Self::Check),
            5 => Some(Self::NotNull),
            _ => None,
        }
    }
}

/// Errors produced by table validation or table-definition decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDefError {
    EmptyTableName,
    DuplicateColumn(String),
    InvalidPrimaryKey(String),
    InvalidIndexColumn(String),
    InvalidConstraintColumn(String),
    TruncatedData,
    InvalidMagic,
    InvalidDataType,
    InvalidIndexType,
    InvalidConstraintType,
    VarintOverflow,
}

impl fmt::Display for TableDefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTableName => write!(f, "empty table name"),
            Self::DuplicateColumn(name) => write!(f, "duplicate column: {name}"),
            Self::InvalidPrimaryKey(name) => write!(f, "invalid primary key column: {name}"),
            Self::InvalidIndexColumn(name) => write!(f, "invalid index column: {name}"),
            Self::InvalidConstraintColumn(name) => {
                write!(f, "invalid constraint column: {name}")
            }
            Self::TruncatedData => write!(f, "truncated data"),
            Self::InvalidMagic => write!(f, "invalid magic bytes"),
            Self::InvalidDataType => write!(f, "invalid data type"),
            Self::InvalidIndexType => write!(f, "invalid index type"),
            Self::InvalidConstraintType => write!(f, "invalid constraint type"),
            Self::VarintOverflow => write!(f, "varint overflow"),
        }
    }
}

impl std::error::Error for TableDefError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_validation_accepts_valid_references() {
        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr))
            .primary_key(vec!["id".into()])
            .add_index(IndexDef::new("idx_ip", vec!["ip".into()]).unique());

        assert!(table.validate().is_ok());
        assert!(table.is_primary_key_column("id"));
        assert_eq!(table.column_index("ip"), Some(1));
    }

    #[test]
    fn table_validation_rejects_invalid_references() {
        let duplicate = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::Integer))
            .add_column(ColumnDef::new("id", DataType::Text));
        assert_eq!(
            duplicate.validate(),
            Err(TableDefError::DuplicateColumn("id".into()))
        );

        let missing_key = TableDef::new("hosts").primary_key(vec!["id".into()]);
        assert_eq!(
            missing_key.validate(),
            Err(TableDefError::InvalidPrimaryKey("id".into()))
        );
    }

    #[test]
    fn builders_preserve_column_and_constraint_details() {
        let column = ColumnDef::new("embedding", DataType::Vector)
            .not_null()
            .with_default(vec![1, 2, 3])
            .with_vector_dim(384)
            .compressed()
            .with_variants(vec!["a".into(), "b".into()])
            .with_precision(6)
            .with_element_type(DataType::Float)
            .with_metadata("unit", "f32");
        assert_eq!(column.vector_dim, Some(384));
        assert_eq!(column.metadata.get("unit").map(String::as_str), Some("f32"));

        let constraint = Constraint::new("fk_host", ConstraintType::ForeignKey)
            .on_columns(vec!["host_id".into()])
            .references("hosts".into(), vec!["id".into()]);
        assert_eq!(constraint.ref_table.as_deref(), Some("hosts"));
    }
}
