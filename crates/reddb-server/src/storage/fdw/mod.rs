//! Foreign Data Wrapper framework (Phase 3.2 PG parity).
//!
//! Allows RedDB to expose external data sources (CSV files, remote
//! PostgreSQL / MySQL databases, S3 Parquet, etc.) as "foreign tables"
//! that queries can reference in SELECT / JOIN / subquery positions
//! alongside native RedDB collections.
//!
//! # Architecture
//!
//! 1. `ForeignDataWrapper` — a trait implemented per data-source kind.
//!    Each wrapper exposes `scan(options, filter) -> Vec<UnifiedRecord>`
//!    plus (optional) `insert` / `delete` for writable wrappers.
//! 2. `ForeignServer` — a named instance of a wrapper, configured with
//!    options (connection string, base path, credentials). Created via
//!    `CREATE SERVER name FOREIGN DATA WRAPPER kind OPTIONS (...)`.
//! 3. `ForeignTable` — a named logical table backed by a server. Created
//!    via `CREATE FOREIGN TABLE name (cols) SERVER srv OPTIONS (...)`.
//!    When a query references the table, the runtime's rewriter swaps
//!    the reference for the wrapper's scan result.
//! 4. `ForeignTableRegistry` — in-memory catalog of servers + tables.
//!    Phase 3.2 keeps definitions in memory only; persistence across
//!    restarts is a 3.2.2 follow-up that mirrors the view registry.
//!
//! # Wrappers shipped in Phase 3.2
//!
//! * `csv` — read a CSV file on local disk. Reuses the RFC-4180 parser
//!   from `storage::import::csv`.
//!
//! Additional wrappers (`postgres_fdw`, `mysql_fdw`, `s3_parquet_fdw`)
//! live behind feature flags to avoid pulling their client libraries
//! into every build.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::query::unified::UnifiedRecord;

pub mod csv;

pub use csv::CsvForeignWrapper;

/// Options bag for `CREATE SERVER ... OPTIONS (key 'value', ...)` and
/// `CREATE FOREIGN TABLE ... OPTIONS (key 'value', ...)`. Key-value
/// strings mirror PG's generic-options model.
#[derive(Debug, Clone, Default)]
pub struct FdwOptions {
    pub values: HashMap<String, String>,
}

impl FdwOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.values.insert(key.to_string(), value.to_string());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    /// Fetch an option, returning a structured error when absent.
    /// Wrappers call this at scan time so callers get a clear message
    /// instead of a silent `None`.
    pub fn require(&self, key: &str) -> Result<&str, FdwError> {
        self.get(key)
            .ok_or_else(|| FdwError::MissingOption(key.to_string()))
    }
}

/// Errors raised by wrappers. Wrapper-specific subclasses collapse into
/// `Custom` so the runtime can surface them uniformly.
#[derive(Debug)]
pub enum FdwError {
    /// A required option was not set on the server or foreign table.
    MissingOption(String),
    /// The wrapper kind referenced by `CREATE SERVER` is not registered.
    UnknownWrapper(String),
    /// The foreign table / server referenced by a statement doesn't exist.
    NotFound(String),
    /// I/O or transport failure (file read, HTTP fetch, database call).
    Io(String),
    /// Arbitrary wrapper-specific error.
    Custom(String),
}

impl std::fmt::Display for FdwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FdwError::MissingOption(k) => write!(f, "FDW option '{k}' is required"),
            FdwError::UnknownWrapper(k) => write!(f, "FDW wrapper '{k}' is not registered"),
            FdwError::NotFound(n) => write!(f, "FDW object '{n}' not found"),
            FdwError::Io(m) => write!(f, "FDW I/O error: {m}"),
            FdwError::Custom(m) => write!(f, "FDW error: {m}"),
        }
    }
}

impl std::error::Error for FdwError {}

/// Immutable per-instance state a wrapper produces once when the server
/// is created. Stored on `ForeignServer::wrapper_state` so concurrent
/// scans don't re-initialise (e.g. re-open file handles, re-parse certs).
pub trait WrapperState: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Core trait every FDW implements.
///
/// Phase 3.2 is read-only — `scan` is mandatory; `insert` / `delete`
/// are defaulted to "not supported" so wrappers can opt-in. Filter
/// pushdown is also opt-in via `supports_pushdown`; the runtime
/// checks that before handing the wrapper the predicate AST.
pub trait ForeignDataWrapper: Send + Sync {
    /// Unique identifier for the wrapper kind (e.g. "csv", "postgres").
    /// Matches the `FOREIGN DATA WRAPPER <name>` clause in DDL.
    fn kind(&self) -> &'static str;

    /// Validate + materialise a server's options. Called once on
    /// `CREATE SERVER` and cached on `ForeignServer`. Wrappers that
    /// don't need per-server state return `Ok(None)`.
    fn build_server_state(
        &self,
        _options: &FdwOptions,
    ) -> Result<Option<Arc<dyn WrapperState>>, FdwError> {
        Ok(None)
    }

    /// Stream rows from the foreign table. `table_options` merges the
    /// server's options with the table's options (table takes priority
    /// on conflicts). `filter` is opaque to the wrapper unless it
    /// advertises pushdown via `supports_pushdown`.
    fn scan(
        &self,
        server_state: Option<&Arc<dyn WrapperState>>,
        table_options: &FdwOptions,
    ) -> Result<Vec<UnifiedRecord>, FdwError>;

    /// Whether the wrapper can evaluate a SQL WHERE predicate natively.
    /// When true, the planner will hand the AST to `scan_with_filter`;
    /// when false, the runtime applies the filter after the scan.
    fn supports_pushdown(&self) -> bool {
        false
    }

    /// Estimated row count — drives planner cost. `None` means the
    /// wrapper has no cheap way to estimate and the planner falls back
    /// to its default assumption.
    fn estimated_row_count(
        &self,
        _server_state: Option<&Arc<dyn WrapperState>>,
        _table_options: &FdwOptions,
    ) -> Option<usize> {
        None
    }
}

/// Registered CREATE SERVER instance.
#[derive(Clone)]
pub struct ForeignServer {
    pub name: String,
    pub wrapper_kind: String,
    pub options: FdwOptions,
    pub wrapper: Arc<dyn ForeignDataWrapper>,
    pub wrapper_state: Option<Arc<dyn WrapperState>>,
}

/// Registered CREATE FOREIGN TABLE instance.
#[derive(Clone)]
pub struct ForeignTable {
    pub name: String,
    pub server_name: String,
    pub columns: Vec<ForeignColumn>,
    pub options: FdwOptions,
}

#[derive(Debug, Clone)]
pub struct ForeignColumn {
    pub name: String,
    /// Declared SQL type as a string (coerced opportunistically at scan time).
    pub data_type: String,
    pub not_null: bool,
}

/// Central registry: maps wrapper kinds to implementations, server names
/// to server records, foreign table names to their definitions.
pub struct ForeignTableRegistry {
    wrappers: parking_lot::RwLock<HashMap<String, Arc<dyn ForeignDataWrapper>>>,
    servers: parking_lot::RwLock<HashMap<String, ForeignServer>>,
    tables: parking_lot::RwLock<HashMap<String, ForeignTable>>,
}

impl ForeignTableRegistry {
    /// Create an empty registry populated with Phase 3.2 built-in wrappers.
    pub fn with_builtins() -> Self {
        let reg = Self {
            wrappers: parking_lot::RwLock::new(HashMap::new()),
            servers: parking_lot::RwLock::new(HashMap::new()),
            tables: parking_lot::RwLock::new(HashMap::new()),
        };
        // CSV is the only built-in shipped in Phase 3.2 — external
        // wrappers (postgres_fdw, mysql_fdw, s3_parquet_fdw) live behind
        // optional cargo features to avoid pulling client libraries in.
        reg.register_wrapper(Arc::new(CsvForeignWrapper));
        reg
    }

    pub fn register_wrapper(&self, wrapper: Arc<dyn ForeignDataWrapper>) {
        self.wrappers
            .write()
            .insert(wrapper.kind().to_string(), wrapper);
    }

    pub fn create_server(
        &self,
        name: &str,
        wrapper_kind: &str,
        options: FdwOptions,
    ) -> Result<(), FdwError> {
        let wrapper = self
            .wrappers
            .read()
            .get(wrapper_kind)
            .cloned()
            .ok_or_else(|| FdwError::UnknownWrapper(wrapper_kind.to_string()))?;

        let wrapper_state = wrapper.build_server_state(&options)?;
        let server = ForeignServer {
            name: name.to_string(),
            wrapper_kind: wrapper_kind.to_string(),
            options,
            wrapper,
            wrapper_state,
        };
        self.servers.write().insert(name.to_string(), server);
        Ok(())
    }

    pub fn drop_server(&self, name: &str) -> bool {
        // Cascade: drop every foreign table pointing at this server.
        let mut tables = self.tables.write();
        tables.retain(|_, t| t.server_name != name);
        drop(tables);
        self.servers.write().remove(name).is_some()
    }

    pub fn create_foreign_table(&self, table: ForeignTable) -> Result<(), FdwError> {
        // Validate server exists.
        if !self.servers.read().contains_key(&table.server_name) {
            return Err(FdwError::NotFound(format!(
                "server '{}'",
                table.server_name
            )));
        }
        self.tables.write().insert(table.name.clone(), table);
        Ok(())
    }

    pub fn drop_foreign_table(&self, name: &str) -> bool {
        self.tables.write().remove(name).is_some()
    }

    pub fn is_foreign_table(&self, name: &str) -> bool {
        self.tables.read().contains_key(name)
    }

    pub fn foreign_table(&self, name: &str) -> Option<ForeignTable> {
        self.tables.read().get(name).cloned()
    }

    pub fn server(&self, name: &str) -> Option<ForeignServer> {
        self.servers.read().get(name).cloned()
    }

    /// Scan a foreign table — called by the runtime when a SELECT
    /// references the table by name.
    pub fn scan(&self, table_name: &str) -> Result<Vec<UnifiedRecord>, FdwError> {
        let table = self
            .foreign_table(table_name)
            .ok_or_else(|| FdwError::NotFound(format!("foreign table '{table_name}'")))?;
        let server = self
            .server(&table.server_name)
            .ok_or_else(|| FdwError::NotFound(format!("server '{}'", table.server_name)))?;

        // Merge server options with table options (table wins).
        let mut merged = server.options.clone();
        for (k, v) in &table.options.values {
            merged.values.insert(k.clone(), v.clone());
        }

        server.wrapper.scan(server.wrapper_state.as_ref(), &merged)
    }

    pub fn list_servers(&self) -> Vec<String> {
        self.servers.read().keys().cloned().collect()
    }

    pub fn list_foreign_tables(&self) -> Vec<String> {
        self.tables.read().keys().cloned().collect()
    }
}

impl Default for ForeignTableRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_register_csv() {
        let reg = ForeignTableRegistry::with_builtins();
        reg.create_server("s1", "csv", FdwOptions::new().with("base_path", "/tmp"))
            .expect("csv server create");
        assert!(reg.server("s1").is_some());
    }

    #[test]
    fn unknown_wrapper_rejected() {
        let reg = ForeignTableRegistry::with_builtins();
        let err = reg
            .create_server("s1", "imaginary", FdwOptions::new())
            .unwrap_err();
        assert!(matches!(err, FdwError::UnknownWrapper(_)));
    }

    #[test]
    fn foreign_table_needs_existing_server() {
        let reg = ForeignTableRegistry::with_builtins();
        let table = ForeignTable {
            name: "t1".to_string(),
            server_name: "nonexistent".to_string(),
            columns: Vec::new(),
            options: FdwOptions::new(),
        };
        let err = reg.create_foreign_table(table).unwrap_err();
        assert!(matches!(err, FdwError::NotFound(_)));
    }

    #[test]
    fn drop_server_cascades_tables() {
        let reg = ForeignTableRegistry::with_builtins();
        reg.create_server("s1", "csv", FdwOptions::new())
            .expect("server create");
        reg.create_foreign_table(ForeignTable {
            name: "t1".to_string(),
            server_name: "s1".to_string(),
            columns: Vec::new(),
            options: FdwOptions::new(),
        })
        .expect("table create");
        assert!(reg.is_foreign_table("t1"));
        assert!(reg.drop_server("s1"));
        assert!(!reg.is_foreign_table("t1"));
    }
}
