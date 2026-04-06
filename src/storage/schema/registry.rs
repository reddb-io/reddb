//! Schema Registry
//!
//! Manages table definitions and schema versioning.
//! The registry stores table metadata in the database and handles migrations.

use super::table::{TableDef, TableDefError};
use std::collections::HashMap;
use std::fmt;

/// Schema registry for managing table definitions
pub struct SchemaRegistry {
    /// Table definitions by name
    tables: HashMap<String, TableDef>,
    /// Schema version
    version: u32,
    /// Migration history
    migrations: Vec<Migration>,
}

impl SchemaRegistry {
    /// Create a new empty schema registry
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
            version: 1,
            migrations: Vec::new(),
        }
    }

    /// Get the current schema version
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Create a new table
    pub fn create_table(&mut self, table: TableDef) -> Result<(), SchemaError> {
        // Validate table definition
        table.validate().map_err(SchemaError::TableDef)?;

        // Check if table already exists
        if self.tables.contains_key(&table.name) {
            return Err(SchemaError::TableExists(table.name.clone()));
        }

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::CreateTable(table.name.clone()),
            timestamp: current_timestamp(),
        });

        self.tables.insert(table.name.clone(), table);
        self.version += 1;

        Ok(())
    }

    /// Drop a table
    pub fn drop_table(&mut self, name: &str) -> Result<TableDef, SchemaError> {
        let table = self
            .tables
            .remove(name)
            .ok_or_else(|| SchemaError::TableNotFound(name.to_string()))?;

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::DropTable(name.to_string()),
            timestamp: current_timestamp(),
        });

        self.version += 1;

        Ok(table)
    }

    /// Get a table definition by name
    pub fn get_table(&self, name: &str) -> Option<&TableDef> {
        self.tables.get(name)
    }

    /// Get a mutable table definition by name
    pub fn get_table_mut(&mut self, name: &str) -> Option<&mut TableDef> {
        self.tables.get_mut(name)
    }

    /// List all table names
    pub fn list_tables(&self) -> Vec<&str> {
        self.tables.keys().map(|s| s.as_str()).collect()
    }

    /// Get number of tables
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Check if a table exists
    pub fn table_exists(&self, name: &str) -> bool {
        self.tables.contains_key(name)
    }

    /// Add a column to an existing table
    pub fn add_column(
        &mut self,
        table_name: &str,
        column: super::table::ColumnDef,
    ) -> Result<(), SchemaError> {
        let table = self
            .tables
            .get_mut(table_name)
            .ok_or_else(|| SchemaError::TableNotFound(table_name.to_string()))?;

        // Check if column already exists
        if table.get_column(&column.name).is_some() {
            return Err(SchemaError::ColumnExists(column.name.clone()));
        }

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::AddColumn {
                table: table_name.to_string(),
                column: column.name.clone(),
            },
            timestamp: current_timestamp(),
        });

        table.columns.push(column);
        table.updated_at = current_timestamp();
        self.version += 1;

        Ok(())
    }

    /// Drop a column from an existing table
    pub fn drop_column(&mut self, table_name: &str, column_name: &str) -> Result<(), SchemaError> {
        let table = self
            .tables
            .get_mut(table_name)
            .ok_or_else(|| SchemaError::TableNotFound(table_name.to_string()))?;

        // Check if column exists
        let idx = table
            .column_index(column_name)
            .ok_or_else(|| SchemaError::ColumnNotFound(column_name.to_string()))?;

        // Can't drop primary key columns
        if table.is_primary_key_column(column_name) {
            return Err(SchemaError::CannotDropPrimaryKey(column_name.to_string()));
        }

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::DropColumn {
                table: table_name.to_string(),
                column: column_name.to_string(),
            },
            timestamp: current_timestamp(),
        });

        table.columns.remove(idx);
        table.updated_at = current_timestamp();
        self.version += 1;

        Ok(())
    }

    /// Create an index on a table
    pub fn create_index(
        &mut self,
        table_name: &str,
        index: super::table::IndexDef,
    ) -> Result<(), SchemaError> {
        let table = self
            .tables
            .get_mut(table_name)
            .ok_or_else(|| SchemaError::TableNotFound(table_name.to_string()))?;

        // Check if index name already exists
        if table.indexes.iter().any(|i| i.name == index.name) {
            return Err(SchemaError::IndexExists(index.name.clone()));
        }

        // Validate index columns exist
        for col in &index.columns {
            if table.get_column(col).is_none() {
                return Err(SchemaError::ColumnNotFound(col.clone()));
            }
        }

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::CreateIndex {
                table: table_name.to_string(),
                index: index.name.clone(),
            },
            timestamp: current_timestamp(),
        });

        table.indexes.push(index);
        table.updated_at = current_timestamp();
        self.version += 1;

        Ok(())
    }

    /// Drop an index from a table
    pub fn drop_index(&mut self, table_name: &str, index_name: &str) -> Result<(), SchemaError> {
        let table = self
            .tables
            .get_mut(table_name)
            .ok_or_else(|| SchemaError::TableNotFound(table_name.to_string()))?;

        let idx = table
            .indexes
            .iter()
            .position(|i| i.name == index_name)
            .ok_or_else(|| SchemaError::IndexNotFound(index_name.to_string()))?;

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::DropIndex {
                table: table_name.to_string(),
                index: index_name.to_string(),
            },
            timestamp: current_timestamp(),
        });

        table.indexes.remove(idx);
        table.updated_at = current_timestamp();
        self.version += 1;

        Ok(())
    }

    /// Rename a table
    pub fn rename_table(&mut self, old_name: &str, new_name: &str) -> Result<(), SchemaError> {
        if !self.tables.contains_key(old_name) {
            return Err(SchemaError::TableNotFound(old_name.to_string()));
        }

        if self.tables.contains_key(new_name) {
            return Err(SchemaError::TableExists(new_name.to_string()));
        }

        let mut table = self.tables.remove(old_name).unwrap();
        table.name = new_name.to_string();
        table.updated_at = current_timestamp();

        // Record migration
        self.migrations.push(Migration {
            version: self.version,
            operation: MigrationOp::RenameTable {
                old_name: old_name.to_string(),
                new_name: new_name.to_string(),
            },
            timestamp: current_timestamp(),
        });

        self.tables.insert(new_name.to_string(), table);
        self.version += 1;

        Ok(())
    }

    /// Get migration history
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// Serialize the schema registry to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Magic bytes
        buf.extend_from_slice(b"RSCH");

        // Version
        buf.extend_from_slice(&self.version.to_le_bytes());

        // Number of tables
        write_varint(&mut buf, self.tables.len() as u64);

        // Serialize each table
        for table in self.tables.values() {
            let table_bytes = table.to_bytes();
            write_varint(&mut buf, table_bytes.len() as u64);
            buf.extend_from_slice(&table_bytes);
        }

        // Number of migrations
        write_varint(&mut buf, self.migrations.len() as u64);

        // Serialize migrations
        for migration in &self.migrations {
            migration.write_to(&mut buf);
        }

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, SchemaError> {
        if data.len() < 4 {
            return Err(SchemaError::TruncatedData);
        }

        // Check magic
        if &data[0..4] != b"RSCH" {
            return Err(SchemaError::InvalidMagic);
        }

        let mut offset = 4;

        // Version
        if data.len() < offset + 4 {
            return Err(SchemaError::TruncatedData);
        }
        let version = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Number of tables
        let (table_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;

        let mut tables = HashMap::with_capacity(table_count as usize);

        for _ in 0..table_count {
            let (table_len, varint_len) = read_varint(&data[offset..])?;
            offset += varint_len;

            if data.len() < offset + table_len as usize {
                return Err(SchemaError::TruncatedData);
            }

            let table = TableDef::from_bytes(&data[offset..offset + table_len as usize])
                .map_err(SchemaError::TableDef)?;
            offset += table_len as usize;

            tables.insert(table.name.clone(), table);
        }

        // Number of migrations
        let (migration_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;

        let mut migrations = Vec::with_capacity(migration_count as usize);

        for _ in 0..migration_count {
            let (migration, migration_len) = Migration::read_from(&data[offset..])?;
            offset += migration_len;
            migrations.push(migration);
        }

        Ok(Self {
            tables,
            version,
            migrations,
        })
    }

    /// Clear all tables (for testing)
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.tables.clear();
        self.version = 1;
        self.migrations.clear();
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SchemaRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Schema Registry v{}", self.version)?;
        writeln!(f, "Tables: {}", self.tables.len())?;
        for table in self.tables.values() {
            writeln!(f, "  - {} ({} columns)", table.name, table.columns.len())?;
        }
        Ok(())
    }
}

/// Migration record
#[derive(Debug, Clone)]
pub struct Migration {
    /// Schema version at migration time
    pub version: u32,
    /// Migration operation
    pub operation: MigrationOp,
    /// Timestamp
    pub timestamp: u64,
}

impl Migration {
    fn write_to(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        self.operation.write_to(buf);
    }

    fn read_from(data: &[u8]) -> Result<(Self, usize), SchemaError> {
        if data.len() < 12 {
            return Err(SchemaError::TruncatedData);
        }

        let version = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let timestamp = u64::from_le_bytes(data[4..12].try_into().unwrap());

        let (operation, op_len) = MigrationOp::read_from(&data[12..])?;

        Ok((
            Self {
                version,
                operation,
                timestamp,
            },
            12 + op_len,
        ))
    }
}

/// Migration operation
#[derive(Debug, Clone)]
pub enum MigrationOp {
    /// Create a new table
    CreateTable(String),
    /// Drop a table
    DropTable(String),
    /// Add a column
    AddColumn { table: String, column: String },
    /// Drop a column
    DropColumn { table: String, column: String },
    /// Create an index
    CreateIndex { table: String, index: String },
    /// Drop an index
    DropIndex { table: String, index: String },
    /// Rename a table
    RenameTable { old_name: String, new_name: String },
}

impl MigrationOp {
    fn write_to(&self, buf: &mut Vec<u8>) {
        match self {
            MigrationOp::CreateTable(name) => {
                buf.push(1);
                write_string(buf, name);
            }
            MigrationOp::DropTable(name) => {
                buf.push(2);
                write_string(buf, name);
            }
            MigrationOp::AddColumn { table, column } => {
                buf.push(3);
                write_string(buf, table);
                write_string(buf, column);
            }
            MigrationOp::DropColumn { table, column } => {
                buf.push(4);
                write_string(buf, table);
                write_string(buf, column);
            }
            MigrationOp::CreateIndex { table, index } => {
                buf.push(5);
                write_string(buf, table);
                write_string(buf, index);
            }
            MigrationOp::DropIndex { table, index } => {
                buf.push(6);
                write_string(buf, table);
                write_string(buf, index);
            }
            MigrationOp::RenameTable { old_name, new_name } => {
                buf.push(7);
                write_string(buf, old_name);
                write_string(buf, new_name);
            }
        }
    }

    fn read_from(data: &[u8]) -> Result<(Self, usize), SchemaError> {
        if data.is_empty() {
            return Err(SchemaError::TruncatedData);
        }

        let op_type = data[0];
        let mut offset = 1;

        let op = match op_type {
            1 => {
                let (name, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::CreateTable(name)
            }
            2 => {
                let (name, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::DropTable(name)
            }
            3 => {
                let (table, len) = read_string(&data[offset..])?;
                offset += len;
                let (column, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::AddColumn { table, column }
            }
            4 => {
                let (table, len) = read_string(&data[offset..])?;
                offset += len;
                let (column, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::DropColumn { table, column }
            }
            5 => {
                let (table, len) = read_string(&data[offset..])?;
                offset += len;
                let (index, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::CreateIndex { table, index }
            }
            6 => {
                let (table, len) = read_string(&data[offset..])?;
                offset += len;
                let (index, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::DropIndex { table, index }
            }
            7 => {
                let (old_name, len) = read_string(&data[offset..])?;
                offset += len;
                let (new_name, len) = read_string(&data[offset..])?;
                offset += len;
                MigrationOp::RenameTable { old_name, new_name }
            }
            _ => return Err(SchemaError::InvalidMigrationOp),
        };

        Ok((op, offset))
    }
}

/// Schema errors
#[derive(Debug)]
pub enum SchemaError {
    /// Table already exists
    TableExists(String),
    /// Table not found
    TableNotFound(String),
    /// Column already exists
    ColumnExists(String),
    /// Column not found
    ColumnNotFound(String),
    /// Index already exists
    IndexExists(String),
    /// Index not found
    IndexNotFound(String),
    /// Cannot drop primary key column
    CannotDropPrimaryKey(String),
    /// Table definition error
    TableDef(TableDefError),
    /// Truncated data
    TruncatedData,
    /// Invalid magic bytes
    InvalidMagic,
    /// Invalid migration operation
    InvalidMigrationOp,
    /// Varint overflow
    VarintOverflow,
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::TableExists(name) => write!(f, "table already exists: {}", name),
            SchemaError::TableNotFound(name) => write!(f, "table not found: {}", name),
            SchemaError::ColumnExists(name) => write!(f, "column already exists: {}", name),
            SchemaError::ColumnNotFound(name) => write!(f, "column not found: {}", name),
            SchemaError::IndexExists(name) => write!(f, "index already exists: {}", name),
            SchemaError::IndexNotFound(name) => write!(f, "index not found: {}", name),
            SchemaError::CannotDropPrimaryKey(name) => {
                write!(f, "cannot drop primary key column: {}", name)
            }
            SchemaError::TableDef(e) => write!(f, "table definition error: {}", e),
            SchemaError::TruncatedData => write!(f, "truncated data"),
            SchemaError::InvalidMagic => write!(f, "invalid magic bytes"),
            SchemaError::InvalidMigrationOp => write!(f, "invalid migration operation"),
            SchemaError::VarintOverflow => write!(f, "varint overflow"),
        }
    }
}

impl std::error::Error for SchemaError {}

impl From<TableDefError> for SchemaError {
    fn from(e: TableDefError) -> Self {
        SchemaError::TableDef(e)
    }
}

/// Get current timestamp
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

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
fn read_varint(data: &[u8]) -> Result<(u64, usize), SchemaError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(SchemaError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(SchemaError::VarintOverflow);
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
fn read_string(data: &[u8]) -> Result<(String, usize), SchemaError> {
    let (len, varint_len) = read_varint(data)?;
    let offset = varint_len;
    if data.len() < offset + len as usize {
        return Err(SchemaError::TruncatedData);
    }
    let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
        .map_err(|_| SchemaError::TruncatedData)?;
    Ok((s, offset + len as usize))
}

#[cfg(test)]
mod tests {
    use super::super::table::{ColumnDef, IndexDef, IndexType};
    use super::super::types::DataType;
    use super::*;

    #[test]
    fn test_create_table() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr).not_null())
            .primary_key(vec!["id".to_string()]);

        assert!(registry.create_table(table).is_ok());
        assert!(registry.table_exists("hosts"));
        assert_eq!(registry.table_count(), 1);
    }

    #[test]
    fn test_duplicate_table() {
        let mut registry = SchemaRegistry::new();

        let table1 = TableDef::new("hosts").add_column(ColumnDef::new("id", DataType::Integer));

        let table2 = TableDef::new("hosts").add_column(ColumnDef::new("id", DataType::Integer));

        assert!(registry.create_table(table1).is_ok());
        assert!(matches!(
            registry.create_table(table2),
            Err(SchemaError::TableExists(_))
        ));
    }

    #[test]
    fn test_drop_table() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts").add_column(ColumnDef::new("id", DataType::Integer));

        registry.create_table(table).unwrap();
        assert!(registry.table_exists("hosts"));

        let dropped = registry.drop_table("hosts").unwrap();
        assert_eq!(dropped.name, "hosts");
        assert!(!registry.table_exists("hosts"));
    }

    #[test]
    fn test_add_column() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts").add_column(ColumnDef::new("id", DataType::Integer));

        registry.create_table(table).unwrap();

        let new_col = ColumnDef::new("hostname", DataType::Text);
        assert!(registry.add_column("hosts", new_col).is_ok());

        let table = registry.get_table("hosts").unwrap();
        assert_eq!(table.columns.len(), 2);
        assert!(table.get_column("hostname").is_some());
    }

    #[test]
    fn test_drop_column() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::Integer).not_null())
            .add_column(ColumnDef::new("hostname", DataType::Text))
            .primary_key(vec!["id".to_string()]);

        registry.create_table(table).unwrap();

        // Can drop non-PK column
        assert!(registry.drop_column("hosts", "hostname").is_ok());

        // Cannot drop PK column
        assert!(matches!(
            registry.drop_column("hosts", "id"),
            Err(SchemaError::CannotDropPrimaryKey(_))
        ));
    }

    #[test]
    fn test_create_index() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::Integer))
            .add_column(ColumnDef::new("ip", DataType::IpAddr));

        registry.create_table(table).unwrap();

        let index = IndexDef::new("idx_ip", vec!["ip".to_string()]).unique();
        assert!(registry.create_index("hosts", index).is_ok());

        let table = registry.get_table("hosts").unwrap();
        assert_eq!(table.indexes.len(), 1);
        assert!(table.indexes[0].unique);
    }

    #[test]
    fn test_rename_table() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("old_name").add_column(ColumnDef::new("id", DataType::Integer));

        registry.create_table(table).unwrap();
        assert!(registry.rename_table("old_name", "new_name").is_ok());

        assert!(!registry.table_exists("old_name"));
        assert!(registry.table_exists("new_name"));

        let table = registry.get_table("new_name").unwrap();
        assert_eq!(table.name, "new_name");
    }

    #[test]
    fn test_migration_history() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts").add_column(ColumnDef::new("id", DataType::Integer));

        registry.create_table(table).unwrap();
        registry
            .add_column("hosts", ColumnDef::new("ip", DataType::IpAddr))
            .unwrap();
        registry
            .create_index("hosts", IndexDef::new("idx_ip", vec!["ip".to_string()]))
            .unwrap();

        assert_eq!(registry.migrations().len(), 3);
        assert!(matches!(
            &registry.migrations()[0].operation,
            MigrationOp::CreateTable(_)
        ));
        assert!(matches!(
            &registry.migrations()[1].operation,
            MigrationOp::AddColumn { .. }
        ));
        assert!(matches!(
            &registry.migrations()[2].operation,
            MigrationOp::CreateIndex { .. }
        ));
    }

    #[test]
    fn test_registry_roundtrip() {
        let mut registry = SchemaRegistry::new();

        let table1 = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::UnsignedInteger).not_null())
            .add_column(ColumnDef::new("ip", DataType::IpAddr).not_null())
            .add_column(ColumnDef::new("embedding", DataType::Vector).with_vector_dim(128))
            .primary_key(vec!["id".to_string()])
            .add_index(IndexDef::new("idx_ip", vec!["ip".to_string()]).unique())
            .add_index(
                IndexDef::new("idx_embedding", vec!["embedding".to_string()])
                    .with_type(IndexType::IvfFlat),
            );

        let table2 = TableDef::new("ports")
            .add_column(ColumnDef::new("host_id", DataType::UnsignedInteger))
            .add_column(ColumnDef::new("port", DataType::UnsignedInteger))
            .add_column(ColumnDef::new("status", DataType::Text));

        registry.create_table(table1).unwrap();
        registry.create_table(table2).unwrap();

        let bytes = registry.to_bytes();
        let recovered = SchemaRegistry::from_bytes(&bytes).unwrap();

        assert_eq!(registry.version(), recovered.version());
        assert_eq!(registry.table_count(), recovered.table_count());

        for name in registry.list_tables() {
            assert!(recovered.table_exists(name));
            let orig = registry.get_table(name).unwrap();
            let rec = recovered.get_table(name).unwrap();
            assert_eq!(orig.columns.len(), rec.columns.len());
            assert_eq!(orig.indexes.len(), rec.indexes.len());
        }
    }

    #[test]
    fn test_registry_display() {
        let mut registry = SchemaRegistry::new();

        let table = TableDef::new("hosts")
            .add_column(ColumnDef::new("id", DataType::Integer))
            .add_column(ColumnDef::new("ip", DataType::IpAddr));

        registry.create_table(table).unwrap();

        let display = format!("{}", registry);
        assert!(display.contains("Schema Registry"));
        assert!(display.contains("hosts"));
    }
}
