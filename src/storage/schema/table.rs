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

    /// Serialize table definition to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Magic bytes for table definition
        buf.extend_from_slice(b"RTBL");

        // Version
        buf.extend_from_slice(&self.version.to_le_bytes());

        // Table name
        write_string(&mut buf, &self.name);

        // Timestamps
        buf.extend_from_slice(&self.created_at.to_le_bytes());
        buf.extend_from_slice(&self.updated_at.to_le_bytes());

        // Columns
        write_varint(&mut buf, self.columns.len() as u64);
        for col in &self.columns {
            col.write_to(&mut buf);
        }

        // Primary key
        write_varint(&mut buf, self.primary_key.len() as u64);
        for pk in &self.primary_key {
            write_string(&mut buf, pk);
        }

        // Indexes
        write_varint(&mut buf, self.indexes.len() as u64);
        for idx in &self.indexes {
            idx.write_to(&mut buf);
        }

        // Constraints
        write_varint(&mut buf, self.constraints.len() as u64);
        for constraint in &self.constraints {
            constraint.write_to(&mut buf);
        }

        buf
    }

    /// Deserialize table definition from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, TableDefError> {
        if data.len() < 4 {
            return Err(TableDefError::TruncatedData);
        }

        // Check magic
        if &data[0..4] != b"RTBL" {
            return Err(TableDefError::InvalidMagic);
        }

        let mut offset = 4;

        // Version
        if data.len() < offset + 4 {
            return Err(TableDefError::TruncatedData);
        }
        let version = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Table name
        let (name, name_len) = read_string(&data[offset..])?;
        offset += name_len;

        // Timestamps
        if data.len() < offset + 16 {
            return Err(TableDefError::TruncatedData);
        }
        let created_at = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let updated_at = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Columns
        let (col_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut columns = Vec::with_capacity(col_count as usize);
        for _ in 0..col_count {
            let (col, col_len) = ColumnDef::read_from(&data[offset..])?;
            offset += col_len;
            columns.push(col);
        }

        // Primary key
        let (pk_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut primary_key = Vec::with_capacity(pk_count as usize);
        for _ in 0..pk_count {
            let (pk, pk_len) = read_string(&data[offset..])?;
            offset += pk_len;
            primary_key.push(pk);
        }

        // Indexes
        let (idx_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut indexes = Vec::with_capacity(idx_count as usize);
        for _ in 0..idx_count {
            let (idx, idx_len) = IndexDef::read_from(&data[offset..])?;
            offset += idx_len;
            indexes.push(idx);
        }

        // Constraints
        let (constraint_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut constraints = Vec::with_capacity(constraint_count as usize);
        for _ in 0..constraint_count {
            let (constraint, constraint_len) = Constraint::read_from(&data[offset..])?;
            offset += constraint_len;
            constraints.push(constraint);
        }

        Ok(Self {
            name,
            columns,
            primary_key,
            indexes,
            constraints,
            version,
            created_at,
            updated_at,
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

    /// Serialize column definition
    fn write_to(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.name);
        buf.push(self.data_type.to_byte());
        buf.push(if self.nullable { 1 } else { 0 });

        // Default value
        if let Some(ref default) = self.default {
            buf.push(1);
            write_varint(buf, default.len() as u64);
            buf.extend_from_slice(default);
        } else {
            buf.push(0);
        }

        // Vector dimension
        if let Some(dim) = self.vector_dim {
            buf.push(1);
            buf.extend_from_slice(&dim.to_le_bytes());
        } else {
            buf.push(0);
        }

        // Metadata
        write_varint(buf, self.metadata.len() as u64);
        for (k, v) in &self.metadata {
            write_string(buf, k);
            write_string(buf, v);
        }
    }

    /// Deserialize column definition
    fn read_from(data: &[u8]) -> Result<(Self, usize), TableDefError> {
        let mut offset = 0;

        let (name, name_len) = read_string(&data[offset..])?;
        offset += name_len;

        if data.len() < offset + 2 {
            return Err(TableDefError::TruncatedData);
        }

        let data_type = DataType::from_byte(data[offset]).ok_or(TableDefError::InvalidDataType)?;
        offset += 1;

        let nullable = data[offset] != 0;
        offset += 1;

        // Default value
        if data.len() < offset + 1 {
            return Err(TableDefError::TruncatedData);
        }
        let has_default = data[offset] != 0;
        offset += 1;
        let default = if has_default {
            let (len, varint_len) = read_varint(&data[offset..])?;
            offset += varint_len;
            if data.len() < offset + len as usize {
                return Err(TableDefError::TruncatedData);
            }
            let default_data = data[offset..offset + len as usize].to_vec();
            offset += len as usize;
            Some(default_data)
        } else {
            None
        };

        // Vector dimension
        if data.len() < offset + 1 {
            return Err(TableDefError::TruncatedData);
        }
        let has_vector_dim = data[offset] != 0;
        offset += 1;
        let vector_dim = if has_vector_dim {
            if data.len() < offset + 4 {
                return Err(TableDefError::TruncatedData);
            }
            let dim = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            Some(dim)
        } else {
            None
        };

        // Metadata
        let (meta_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut metadata = HashMap::with_capacity(meta_count as usize);
        for _ in 0..meta_count {
            let (k, k_len) = read_string(&data[offset..])?;
            offset += k_len;
            let (v, v_len) = read_string(&data[offset..])?;
            offset += v_len;
            metadata.insert(k, v);
        }

        Ok((
            Self {
                name,
                data_type,
                nullable,
                default,
                vector_dim,
                metadata,
            },
            offset,
        ))
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

    /// Serialize index
    fn write_to(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.name);
        buf.push(self.index_type as u8);
        buf.push(if self.unique { 1 } else { 0 });
        write_varint(buf, self.columns.len() as u64);
        for col in &self.columns {
            write_string(buf, col);
        }
    }

    /// Deserialize index
    fn read_from(data: &[u8]) -> Result<(Self, usize), TableDefError> {
        let mut offset = 0;

        let (name, name_len) = read_string(&data[offset..])?;
        offset += name_len;

        if data.len() < offset + 2 {
            return Err(TableDefError::TruncatedData);
        }

        let index_type =
            IndexType::from_byte(data[offset]).ok_or(TableDefError::InvalidIndexType)?;
        offset += 1;

        let unique = data[offset] != 0;
        offset += 1;

        let (col_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;

        let mut columns = Vec::with_capacity(col_count as usize);
        for _ in 0..col_count {
            let (col, col_len) = read_string(&data[offset..])?;
            offset += col_len;
            columns.push(col);
        }

        Ok((
            Self {
                name,
                columns,
                index_type,
                unique,
            },
            offset,
        ))
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

    /// Serialize constraint
    fn write_to(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.name);
        buf.push(self.constraint_type as u8);

        write_varint(buf, self.columns.len() as u64);
        for col in &self.columns {
            write_string(buf, col);
        }

        if let Some(ref table) = self.ref_table {
            buf.push(1);
            write_string(buf, table);
            if let Some(ref cols) = self.ref_columns {
                write_varint(buf, cols.len() as u64);
                for col in cols {
                    write_string(buf, col);
                }
            } else {
                write_varint(buf, 0);
            }
        } else {
            buf.push(0);
        }
    }

    /// Deserialize constraint
    fn read_from(data: &[u8]) -> Result<(Self, usize), TableDefError> {
        let mut offset = 0;

        let (name, name_len) = read_string(&data[offset..])?;
        offset += name_len;

        if data.len() < offset + 1 {
            return Err(TableDefError::TruncatedData);
        }

        let constraint_type =
            ConstraintType::from_byte(data[offset]).ok_or(TableDefError::InvalidConstraintType)?;
        offset += 1;

        let (col_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;

        let mut columns = Vec::with_capacity(col_count as usize);
        for _ in 0..col_count {
            let (col, col_len) = read_string(&data[offset..])?;
            offset += col_len;
            columns.push(col);
        }

        if data.len() < offset + 1 {
            return Err(TableDefError::TruncatedData);
        }

        let has_ref = data[offset] != 0;
        offset += 1;

        let (ref_table, ref_columns) = if has_ref {
            let (table, table_len) = read_string(&data[offset..])?;
            offset += table_len;

            let (ref_col_count, varint_len) = read_varint(&data[offset..])?;
            offset += varint_len;

            let mut ref_cols = Vec::with_capacity(ref_col_count as usize);
            for _ in 0..ref_col_count {
                let (col, col_len) = read_string(&data[offset..])?;
                offset += col_len;
                ref_cols.push(col);
            }

            (Some(table), Some(ref_cols))
        } else {
            (None, None)
        };

        Ok((
            Self {
                name,
                constraint_type,
                columns,
                ref_table,
                ref_columns,
            },
            offset,
        ))
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

/// Write a variable-length integer
fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Read a variable-length integer
fn read_varint(data: &[u8]) -> Result<(u64, usize), TableDefError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(TableDefError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(TableDefError::VarintOverflow);
        }

        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            break;
        }
    }

    Ok((result, offset))
}

/// Write a length-prefixed string
fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_varint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

/// Read a length-prefixed string
fn read_string(data: &[u8]) -> Result<(String, usize), TableDefError> {
    let (len, varint_len) = read_varint(data)?;
    let offset = varint_len;
    if data.len() < offset + len as usize {
        return Err(TableDefError::TruncatedData);
    }
    let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
        .map_err(|_| TableDefError::TruncatedData)?;
    Ok((s, offset + len as usize))
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
