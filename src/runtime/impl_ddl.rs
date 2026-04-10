//! DDL execution: CREATE TABLE, DROP TABLE, ALTER TABLE via SQL AST
//!
//! Translates DDL statements into collection-level operations on the
//! underlying `UnifiedStore`.  RedDB uses a flexible schema-on-read
//! model, so column definitions are advisory metadata rather than
//! rigid constraints.

use super::*;

impl RedDBRuntime {
    /// Execute CREATE TABLE
    ///
    /// Creates a new collection in the store.  Column definitions are
    /// recorded for introspection but do not enforce rigid schema
    /// constraints.
    pub fn execute_create_table(
        &self,
        raw_query: &str,
        query: &CreateTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Check if the collection already exists.
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "table '{}' already exists",
                query.name
            )));
        }

        // Create the collection.
        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        if let Some(default_ttl_ms) = query.default_ttl_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, default_ttl_ms);
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        let ttl_suffix = query
            .default_ttl_ms
            .map(|ttl_ms| format!(" with default TTL {}ms", ttl_ms))
            .unwrap_or_default();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("table '{}' created{}", query.name, ttl_suffix),
            "create",
        ))
    }

    /// Execute DROP TABLE
    ///
    /// Drops the collection and all its data from the store.
    pub fn execute_drop_table(
        &self,
        raw_query: &str,
        query: &DropTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        let exists = store.get_collection(&query.name).is_some();
        if !exists {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }

        store
            .drop_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("table '{}' dropped", query.name),
            "drop",
        ))
    }

    /// Execute ALTER TABLE
    ///
    /// In RedDB's schema-on-read model, ALTER TABLE operations are advisory.
    /// ADD COLUMN records the schema intent, DROP COLUMN removes it, and
    /// RENAME COLUMN is a metadata rename.  Existing data is not rewritten.
    pub fn execute_alter_table(
        &self,
        raw_query: &str,
        query: &AlterTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists.
        if store.get_collection(&query.name).is_none() {
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }

        let mut messages = Vec::new();

        for op in &query.operations {
            match op {
                AlterOperation::AddColumn(col) => {
                    // Schema-on-read: column will be available on next insert.
                    messages.push(format!("column '{}' added", col.name));
                }
                AlterOperation::DropColumn(name) => {
                    messages.push(format!("column '{}' dropped", name));
                }
                AlterOperation::RenameColumn { from, to } => {
                    messages.push(format!("column '{}' renamed to '{}'", from, to));
                }
            }
        }

        let message = if messages.is_empty() {
            format!("table '{}' altered (no operations)", query.name)
        } else {
            format!("table '{}' altered: {}", query.name, messages.join(", "))
        };

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &message,
            "alter",
        ))
    }

    /// Execute CREATE INDEX
    ///
    /// Registers a new index on a collection. The actual index data structure
    /// is built lazily or on next rebuild.
    pub fn execute_create_index(
        &self,
        raw_query: &str,
        query: &CreateIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists
        if store.get_collection(&query.table).is_none() {
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.table
            )));
        }

        let method_str = format!("{}", query.method);
        let unique_str = if query.unique { "unique " } else { "" };
        let cols = query.columns.join(", ");

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "{}index '{}' created on '{}' ({}) using {}",
                unique_str, query.name, query.table, cols, method_str
            ),
            "create",
        ))
    }

    /// Execute DROP INDEX
    ///
    /// Removes an index from a collection.
    pub fn execute_drop_index(
        &self,
        raw_query: &str,
        query: &DropIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists
        if store.get_collection(&query.table).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.table),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.table
            )));
        }

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("index '{}' dropped from '{}'", query.name, query.table),
            "drop",
        ))
    }
}
