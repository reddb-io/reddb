//! Table Definition
//!
//! Defines table structure including columns, primary keys, indexes, and constraints.
//! Tables are the primary data organization unit in RedDB.

use super::types::DataType;
use std::collections::HashMap;
use std::fmt;

/// Table definition containing all metadata
#[derive(Debug, Clone)]
pub struct TableDef {
    /// Table name (unique within database)
    pub name: String,
    /// Column definitions in order
    pub columns: Vec<ColumnDef>,
    /// Primary key column names (can be composite)
    pub primary_key: Vec<String>,
    /// Index definitions
    pub indexes: Vec<IndexDef>,
    /// Table-level constraints
    pub constraints: Vec<Constraint>,
    /// Schema version (for migrations)
    pub version: u32,
    /// Creation timestamp
    pub created_at: u64,
    /// Last modification timestamp
    pub updated_at: u64,
}

impl TableDef {
    /// Create a new table definition
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

    /// Add a column to the table
    pub fn add_column(mut self, column: ColumnDef) -> Self {
        self.columns.push(column);
        self
    }

    /// Set primary key columns
    pub fn primary_key(mut self, columns: Vec<String>) -> Self {
        self.primary_key = columns;
        self
    }

    /// Add an index
    pub fn add_index(mut self, index: IndexDef) -> Self {
        self.indexes.push(index);
        self
    }

    /// Add a constraint
    pub fn add_constraint(mut self, constraint: Constraint) -> Self {
        self.constraints.push(constraint);
        self
    }

    /// Get column by name
    pub fn get_column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Get column index by name
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// Check if a column is part of the primary key
    pub fn is_primary_key_column(&self, name: &str) -> bool {
        self.primary_key.iter().any(|pk| pk == name)
    }

    /// Validate table definition
    pub fn validate(&self) -> Result<(), TableDefError> {
        // Check table name
        if self.name.is_empty() {
            return Err(TableDefError::EmptyTableName);
        }

        // Check for duplicate column names
        let mut seen = HashMap::new();
        for col in &self.columns {
            if seen.insert(&col.name, true).is_some() {
                return Err(TableDefError::DuplicateColumn(col.name.clone()));
            }
        }

        // Validate primary key columns exist
        for pk_col in &self.primary_key {
            if self.get_column(pk_col).is_none() {
                return Err(TableDefError::InvalidPrimaryKey(pk_col.clone()));
            }
        }

        // Validate index columns exist
        for index in &self.indexes {
            for col in &index.columns {
                if self.get_column(col).is_none() {
                    return Err(TableDefError::InvalidIndexColumn(col.clone()));
                }
            }
        }

        // Validate constraints reference existing columns
        for constraint in &self.constraints {
            for col in &constraint.columns {
                if self.get_column(col).is_none() {
                    return Err(TableDefError::InvalidConstraintColumn(col.clone()));
                }
            }
        }

        Ok(())
    }

    /// Serialize table definition to bytes.
    ///
    /// The on-disk byte layout lives in [`reddb_file::table_def`]; this method
    /// only projects the schema into the codec frame, lowering each enum
    /// discriminant (`DataType` / `IndexType` / `ConstraintType`) to its byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let frame = reddb_file::TableDefFrame {
            name: self.name.clone(),
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
            columns: self
                .columns
                .iter()
                .map(|col| reddb_file::ColumnDefFrame {
                    name: col.name.clone(),
                    data_type: col.data_type.to_byte(),
                    nullable: col.nullable,
                    default: col.default.clone(),
                    vector_dim: col.vector_dim,
                    compress: col.compress,
                    enum_variants: col.enum_variants.clone(),
                    decimal_precision: col.decimal_precision,
                    element_type: col.element_type.map(|et| et.to_byte()),
                    metadata: col
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                })
                .collect(),
            primary_key: self.primary_key.clone(),
            indexes: self
                .indexes
                .iter()
                .map(|idx| reddb_file::IndexDefFrame {
                    name: idx.name.clone(),
                    index_type: idx.index_type as u8,
                    unique: idx.unique,
                    columns: idx.columns.clone(),
                })
                .collect(),
            constraints: self
                .constraints
                .iter()
                .map(|c| reddb_file::ConstraintFrame {
                    name: c.name.clone(),
                    constraint_type: c.constraint_type as u8,
                    columns: c.columns.clone(),
                    ref_table: c.ref_table.clone(),
                    ref_columns: c.ref_columns.clone(),
                })
                .collect(),
        };
        reddb_file::encode_table_def_frame(&frame)
    }

    /// Deserialize table definition from bytes.
    ///
    /// Byte parsing lives in [`reddb_file::table_def`]; this method maps each
    /// opaque discriminant byte back to its schema enum, rejecting unknown
    /// values.
    pub fn from_bytes(data: &[u8]) -> Result<Self, TableDefError> {
        let frame = reddb_file::decode_table_def_frame(data)?;

        let mut columns = Vec::with_capacity(frame.columns.len());
        for col in frame.columns {
            let data_type =
                DataType::from_byte(col.data_type).ok_or(TableDefError::InvalidDataType)?;
            let element_type = match col.element_type {
                Some(byte) => {
                    Some(DataType::from_byte(byte).ok_or(TableDefError::InvalidDataType)?)
                }
                None => None,
            };
            columns.push(ColumnDef {
                name: col.name,
                data_type,
                nullable: col.nullable,
                default: col.default,
                vector_dim: col.vector_dim,
                compress: col.compress,
                enum_variants: col.enum_variants,
                decimal_precision: col.decimal_precision,
                element_type,
                metadata: col.metadata.into_iter().collect(),
            });
        }

        let mut indexes = Vec::with_capacity(frame.indexes.len());
        for idx in frame.indexes {
            let index_type =
                IndexType::from_byte(idx.index_type).ok_or(TableDefError::InvalidIndexType)?;
            indexes.push(IndexDef {
                name: idx.name,
                columns: idx.columns,
                index_type,
                unique: idx.unique,
            });
        }

        let mut constraints = Vec::with_capacity(frame.constraints.len());
        for constraint in frame.constraints {
            let constraint_type = ConstraintType::from_byte(constraint.constraint_type)
                .ok_or(TableDefError::InvalidConstraintType)?;
            constraints.push(Constraint {
                name: constraint.name,
                constraint_type,
                columns: constraint.columns,
                ref_table: constraint.ref_table,
                ref_columns: constraint.ref_columns,
            });
        }

        Ok(Self {
            name: frame.name,
            columns,
            primary_key: frame.primary_key,
            indexes,
            constraints,
            version: frame.version,
            created_at: frame.created_at,
            updated_at: frame.updated_at,
        })
    }
}

impl fmt::Display for TableDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "TABLE {} (version {})", self.name, self.version)?;
        writeln!(f, "  Columns:")?;
        for col in &self.columns {
            writeln!(f, "    {}", col)?;
        }
        if !self.primary_key.is_empty() {
            writeln!(f, "  Primary Key: ({})", self.primary_key.join(", "))?;
        }
        if !self.indexes.is_empty() {
            writeln!(f, "  Indexes:")?;
            for idx in &self.indexes {
                writeln!(f, "    {}", idx)?;
            }
        }
        Ok(())
    }
}

/// Column definition
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// Column name
    pub name: String,
    /// Data type
    pub data_type: DataType,
    /// Whether NULL values are allowed
    pub nullable: bool,
    /// Default value (serialized)
    pub default: Option<Vec<u8>>,
    /// Vector dimension (for Vector type)
    pub vector_dim: Option<u32>,
    /// Whether to compress this column's data (e.g., brotli for text)
    pub compress: bool,
    /// For Enum type: list of valid variants
    pub enum_variants: Vec<String>,
    /// For Decimal type: number of decimal places (default 4)
    pub decimal_precision: u8,
    /// For Array type: element data type
    pub element_type: Option<DataType>,
    /// Additional column metadata
    pub metadata: HashMap<String, String>,
}

impl ColumnDef {
    /// Create a new column definition
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

    /// Create a non-nullable column
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Set default value
    pub fn with_default(mut self, default: Vec<u8>) -> Self {
        self.default = Some(default);
        self
    }

    /// Set vector dimension
    pub fn with_vector_dim(mut self, dim: u32) -> Self {
        self.vector_dim = Some(dim);
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Enable per-column compression
    pub fn compressed(mut self) -> Self {
        self.compress = true;
        self
    }

    /// Set enum variants (for Enum type columns)
    pub fn with_variants(mut self, variants: Vec<String>) -> Self {
        self.enum_variants = variants;
        self
    }

    /// Set decimal precision (for Decimal type columns)
    pub fn with_precision(mut self, precision: u8) -> Self {
        self.decimal_precision = precision;
        self
    }

    /// Set element type (for Array type columns)
    pub fn with_element_type(mut self, dt: DataType) -> Self {
        self.element_type = Some(dt);
        self
    }
}

impl fmt::Display for ColumnDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.data_type)?;
        if let Some(dim) = self.vector_dim {
            write!(f, "({})", dim)?;
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

/// Index definition
#[derive(Debug, Clone)]
pub struct IndexDef {
    /// Index name
    pub name: String,
    /// Column names in order
    pub columns: Vec<String>,
    /// Index type
    pub index_type: IndexType,
    /// Whether values must be unique
    pub unique: bool,
}

impl IndexDef {
    /// Create a new index
    pub fn new(name: impl Into<String>, columns: Vec<String>) -> Self {
        Self {
            name: name.into(),
            columns,
            index_type: IndexType::BTree,
            unique: false,
        }
    }

    /// Create a unique index
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Set index type
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

/// Index type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IndexType {
    /// B-tree index (default, good for range queries)
    BTree = 1,
    /// Hash index (exact match only, faster for point queries)
    Hash = 2,
    /// IVF index for vector similarity search
    IvfFlat = 3,
    /// HNSW index for vector similarity search
    Hnsw = 4,
}

impl IndexType {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(IndexType::BTree),
            2 => Some(IndexType::Hash),
            3 => Some(IndexType::IvfFlat),
            4 => Some(IndexType::Hnsw),
            _ => None,
        }
    }
}

/// Constraint definition
#[derive(Debug, Clone)]
pub struct Constraint {
    /// Constraint name
    pub name: String,
    /// Constraint type
    pub constraint_type: ConstraintType,
    /// Columns involved
    pub columns: Vec<String>,
    /// Reference table (for foreign keys)
    pub ref_table: Option<String>,
    /// Reference columns (for foreign keys)
    pub ref_columns: Option<Vec<String>>,
}

impl Constraint {
    /// Create a new constraint
    pub fn new(name: impl Into<String>, constraint_type: ConstraintType) -> Self {
        Self {
            name: name.into(),
            constraint_type,
            columns: Vec::new(),
            ref_table: None,
            ref_columns: None,
        }
    }

    /// Set columns
    pub fn on_columns(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
        self
    }

    /// Set foreign key reference
    pub fn references(mut self, table: String, columns: Vec<String>) -> Self {
        self.ref_table = Some(table);
        self.ref_columns = Some(columns);
        self
    }
}

/// Constraint type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConstraintType {
    /// Primary key constraint
    PrimaryKey = 1,
    /// Unique constraint
    Unique = 2,
    /// Foreign key constraint
    ForeignKey = 3,
    /// Check constraint
    Check = 4,
    /// Not null constraint
    NotNull = 5,
}

impl ConstraintType {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(ConstraintType::PrimaryKey),
            2 => Some(ConstraintType::Unique),
            3 => Some(ConstraintType::ForeignKey),
            4 => Some(ConstraintType::Check),
            5 => Some(ConstraintType::NotNull),
            _ => None,
        }
    }
}

/// Errors that can occur with table definitions
#[derive(Debug, Clone, PartialEq)]
pub enum TableDefError {
    /// Empty table name
    EmptyTableName,
    /// Duplicate column name
    DuplicateColumn(String),
    /// Invalid primary key column
    InvalidPrimaryKey(String),
    /// Invalid index column
    InvalidIndexColumn(String),
    /// Invalid constraint column
    InvalidConstraintColumn(String),
    /// Truncated data
    TruncatedData,
    /// Invalid magic bytes
    InvalidMagic,
    /// Invalid data type
    InvalidDataType,
    /// Invalid index type
    InvalidIndexType,
    /// Invalid constraint type
    InvalidConstraintType,
    /// Varint overflow
    VarintOverflow,
}

impl fmt::Display for TableDefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableDefError::EmptyTableName => write!(f, "empty table name"),
            TableDefError::DuplicateColumn(name) => write!(f, "duplicate column: {}", name),
            TableDefError::InvalidPrimaryKey(name) => {
                write!(f, "invalid primary key column: {}", name)
            }
            TableDefError::InvalidIndexColumn(name) => write!(f, "invalid index column: {}", name),
            TableDefError::InvalidConstraintColumn(name) => {
                write!(f, "invalid constraint column: {}", name)
            }
            TableDefError::TruncatedData => write!(f, "truncated data"),
            TableDefError::InvalidMagic => write!(f, "invalid magic bytes"),
            TableDefError::InvalidDataType => write!(f, "invalid data type"),
            TableDefError::InvalidIndexType => write!(f, "invalid index type"),
            TableDefError::InvalidConstraintType => write!(f, "invalid constraint type"),
            TableDefError::VarintOverflow => write!(f, "varint overflow"),
        }
    }
}

impl std::error::Error for TableDefError {}

impl From<reddb_file::TableDefFrameError> for TableDefError {
    fn from(err: reddb_file::TableDefFrameError) -> Self {
        match err {
            reddb_file::TableDefFrameError::TruncatedData => TableDefError::TruncatedData,
            reddb_file::TableDefFrameError::InvalidMagic => TableDefError::InvalidMagic,
            reddb_file::TableDefFrameError::VarintOverflow => TableDefError::VarintOverflow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_def_basic() {
        let table = TableDef::new("port_scans")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr).not_null())
            .add_column(ColumnDef::new("port", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("status", DataType::Text))
            .add_column(ColumnDef::new("timestamp", DataType::Timestamp).not_null())
            .primary_key(vec!["id".to_string()]);

        assert_eq!(table.name, "port_scans");
        assert_eq!(table.columns.len(), 5);
        assert_eq!(table.primary_key, vec!["id"]);
        assert!(table.validate().is_ok());
    }

    #[test]
    fn test_table_def_with_indexes() {
        let table = TableDef::new("subdomains")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("domain", DataType::Text).not_null())
            .add_column(ColumnDef::new("subdomain", DataType::Text).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr))
            .primary_key(vec!["id".to_string()])
            .add_index(IndexDef::new("idx_domain", vec!["domain".to_string()]))
            .add_index(IndexDef::new("idx_subdomain", vec!["subdomain".to_string()]).unique());

        assert_eq!(table.indexes.len(), 2);
        assert!(table.indexes[1].unique);
        assert!(table.validate().is_ok());
    }

    #[test]
    fn test_table_def_with_vector() {
        let table = TableDef::new("embeddings")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(
                ColumnDef::new("embedding", DataType::Vector)
                    .not_null()
                    .with_vector_dim(384),
            )
            .add_column(ColumnDef::new("text", DataType::Text))
            .primary_key(vec!["id".to_string()])
            .add_index(
                IndexDef::new("idx_embedding", vec!["embedding".to_string()])
                    .with_type(IndexType::IvfFlat),
            );

        let col = table.get_column("embedding").unwrap();
        assert_eq!(col.vector_dim, Some(384));
        assert!(table.validate().is_ok());
    }

    #[test]
    fn test_table_def_validation_duplicate_column() {
        let table = TableDef::new("test")
            .add_column(ColumnDef::new("id", DataType::Integer))
            .add_column(ColumnDef::new("id", DataType::Text)); // Duplicate

        assert!(matches!(
            table.validate(),
            Err(TableDefError::DuplicateColumn(_))
        ));
    }

    #[test]
    fn test_table_def_validation_invalid_pk() {
        let table = TableDef::new("test")
            .add_column(ColumnDef::new("id", DataType::Integer))
            .primary_key(vec!["nonexistent".to_string()]);

        assert!(matches!(
            table.validate(),
            Err(TableDefError::InvalidPrimaryKey(_))
        ));
    }

    #[test]
    fn test_table_def_roundtrip() {
        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr).not_null())
            .add_column(ColumnDef::new("hostname", DataType::Text))
            .add_column(ColumnDef::new("last_seen", DataType::Timestamp))
            .add_column(ColumnDef::new("fingerprint", DataType::Vector).with_vector_dim(128))
            .primary_key(vec!["id".to_string()])
            .add_index(IndexDef::new("idx_ip", vec!["ip".to_string()]).unique())
            .add_index(
                IndexDef::new("idx_fingerprint", vec!["fingerprint".to_string()])
                    .with_type(IndexType::IvfFlat),
            );

        let bytes = table.to_bytes();
        let recovered = TableDef::from_bytes(&bytes).unwrap();

        assert_eq!(table.name, recovered.name);
        assert_eq!(table.columns.len(), recovered.columns.len());
        assert_eq!(table.primary_key, recovered.primary_key);
        assert_eq!(table.indexes.len(), recovered.indexes.len());

        for (orig, rec) in table.columns.iter().zip(recovered.columns.iter()) {
            assert_eq!(orig.name, rec.name);
            assert_eq!(orig.data_type, rec.data_type);
            assert_eq!(orig.nullable, rec.nullable);
            assert_eq!(orig.vector_dim, rec.vector_dim);
        }

        for (orig, rec) in table.indexes.iter().zip(recovered.indexes.iter()) {
            assert_eq!(orig.name, rec.name);
            assert_eq!(orig.columns, rec.columns);
            assert_eq!(orig.unique, rec.unique);
            assert_eq!(orig.index_type, rec.index_type);
        }
    }

    #[test]
    fn test_column_def_metadata() {
        let col = ColumnDef::new("ip", DataType::IpAddr)
            .not_null()
            .with_metadata("description", "Target IP address")
            .with_metadata("indexed", "true");

        assert_eq!(
            col.metadata.get("description"),
            Some(&"Target IP address".to_string())
        );
        assert_eq!(col.metadata.get("indexed"), Some(&"true".to_string()));
    }

    #[test]
    fn test_constraint_foreign_key() {
        let constraint = Constraint::new("fk_host", ConstraintType::ForeignKey)
            .on_columns(vec!["host_id".to_string()])
            .references("hosts".to_string(), vec!["id".to_string()]);

        assert_eq!(constraint.constraint_type, ConstraintType::ForeignKey);
        assert_eq!(constraint.columns, vec!["host_id"]);
        assert_eq!(constraint.ref_table, Some("hosts".to_string()));
        assert_eq!(constraint.ref_columns, Some(vec!["id".to_string()]));
    }

    #[test]
    fn test_table_display() {
        let table = TableDef::new("test")
            .add_column(ColumnDef::new("id", DataType::Integer).not_null())
            .add_column(ColumnDef::new("name", DataType::Text))
            .primary_key(vec!["id".to_string()]);

        let display = format!("{}", table);
        assert!(display.contains("TABLE test"));
        assert!(display.contains("id INTEGER NOT NULL"));
        assert!(display.contains("name TEXT"));
        assert!(display.contains("Primary Key: (id)"));
    }
}
