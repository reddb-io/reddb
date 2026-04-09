//! DML execution: INSERT, UPDATE, DELETE via SQL AST
//!
//! Implements `execute_insert`, `execute_update`, and `execute_delete` on
//! `RedDBRuntime`.  Each method translates the parsed AST into entity-level
//! operations through the existing `RuntimeEntityPort` trait so that all
//! cross-cutting concerns (WAL, indexing, replication) are automatically
//! applied.

use crate::application::entity::{
    CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput,
    CreateVectorInput, DeleteEntityInput, PatchEntityInput, PatchEntityOperation,
    PatchEntityOperationType,
};
use crate::application::ports::RuntimeEntityPort;

use super::*;

impl RedDBRuntime {
    /// Execute INSERT INTO table [entity_type] (cols) VALUES (vals), ...
    ///
    /// Each row in `query.values` is zipped with `query.columns` to produce a
    /// set of named fields, which is then dispatched based on entity_type.
    pub fn execute_insert(
        &self,
        raw_query: &str,
        query: &InsertQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let mut inserted_count: u64 = 0;

        // Ensure the collection exists (auto-create on first insert).
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(&query.table);

        for row_values in &query.values {
            if row_values.len() != query.columns.len() {
                return Err(RedDBError::Query(format!(
                    "INSERT column count ({}) does not match value count ({})",
                    query.columns.len(),
                    row_values.len()
                )));
            }

            match query.entity_type {
                InsertEntityType::Row => {
                    let fields: Vec<(String, Value)> = query
                        .columns
                        .iter()
                        .zip(row_values.iter())
                        .map(|(col, val)| (col.clone(), val.clone()))
                        .collect();

                    let input = CreateRowInput {
                        collection: query.table.clone(),
                        fields,
                        metadata: Vec::new(),
                        node_links: Vec::new(),
                        vector_links: Vec::new(),
                    };
                    self.create_row(input)?;
                }
                InsertEntityType::Node => {
                    let label = find_column_value_string(&query.columns, row_values, "label")?;
                    let node_type = find_column_value_opt_string(&query.columns, row_values, "node_type");
                    let properties = extract_remaining_properties(
                        &query.columns,
                        row_values,
                        &["label", "node_type"],
                    );
                    let input = CreateNodeInput {
                        collection: query.table.clone(),
                        label,
                        node_type,
                        properties,
                        metadata: Vec::new(),
                        embeddings: Vec::new(),
                        table_links: Vec::new(),
                        node_links: Vec::new(),
                    };
                    self.create_node(input)?;
                }
                InsertEntityType::Edge => {
                    let label = find_column_value_string(&query.columns, row_values, "label")?;
                    let from_id = find_column_value_u64(&query.columns, row_values, "from")?;
                    let to_id = find_column_value_u64(&query.columns, row_values, "to")?;
                    let weight = find_column_value_f32_opt(&query.columns, row_values, "weight");
                    let properties = extract_remaining_properties(
                        &query.columns,
                        row_values,
                        &["label", "from", "to", "weight"],
                    );
                    let input = CreateEdgeInput {
                        collection: query.table.clone(),
                        label,
                        from: EntityId::new(from_id),
                        to: EntityId::new(to_id),
                        weight,
                        properties,
                        metadata: Vec::new(),
                    };
                    self.create_edge(input)?;
                }
                InsertEntityType::Vector => {
                    let dense = find_column_value_vec_f32(&query.columns, row_values, "dense")?;
                    let content = find_column_value_opt_string(&query.columns, row_values, "content");
                    let input = CreateVectorInput {
                        collection: query.table.clone(),
                        dense,
                        content,
                        metadata: Vec::new(),
                        link_row: None,
                        link_node: None,
                    };
                    self.create_vector(input)?;
                }
                InsertEntityType::Document => {
                    let body_str = find_column_value_string(&query.columns, row_values, "body")?;
                    let body: crate::json::Value = crate::json::from_str(&body_str)
                        .map_err(|e| RedDBError::Query(format!("invalid JSON body: {e}")))?;
                    let input = CreateDocumentInput {
                        collection: query.table.clone(),
                        body,
                        metadata: Vec::new(),
                        node_links: Vec::new(),
                        vector_links: Vec::new(),
                    };
                    self.create_document(input)?;
                }
                InsertEntityType::Kv => {
                    let key = find_column_value_string(&query.columns, row_values, "key")?;
                    let value = find_column_value(&query.columns, row_values, "value")?;
                    let input = CreateKvInput {
                        collection: query.table.clone(),
                        key,
                        value,
                        metadata: Vec::new(),
                    };
                    self.create_kv(input)?;
                }
            }

            inserted_count += 1;
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            inserted_count,
            "insert",
            "runtime-dml",
        ))
    }

    /// Execute UPDATE table SET col=val, ... WHERE filter
    ///
    /// Scans the target collection, evaluates the WHERE filter against each
    /// record, and patches every matching entity.
    pub fn execute_update(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        // Scan all entities and convert to runtime records for filter evaluation.
        let entities = manager.query_all(|_| true);
        let mut affected: u64 = 0;

        for entity in entities {
            // Convert entity to a UnifiedRecord for filter evaluation.
            let record = match record_search::runtime_table_record_from_entity(entity.clone()) {
                Some(r) => r,
                None => continue,
            };

            // Evaluate WHERE filter.
            let matches = match &query.filter {
                Some(filter) => join_filter::evaluate_runtime_filter(
                    &record,
                    filter,
                    Some(&query.table),
                    Some(&query.table),
                ),
                None => true, // No filter means update all rows.
            };

            if !matches {
                continue;
            }

            // Build patch operations from SET assignments.
            let operations: Vec<PatchEntityOperation> = query
                .assignments
                .iter()
                .map(|(col, val)| {
                    let json_val = storage_value_to_json(val);
                    PatchEntityOperation {
                        op: PatchEntityOperationType::Set,
                        path: vec!["fields".to_string(), col.clone()],
                        value: Some(json_val),
                    }
                })
                .collect();

            let input = PatchEntityInput {
                collection: query.table.clone(),
                id: entity.id,
                payload: crate::json::Value::Null,
                operations,
            };

            self.patch_entity(input)?;
            affected += 1;
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "update",
            "runtime-dml",
        ))
    }

    /// Execute DELETE FROM table WHERE filter
    ///
    /// Scans the target collection, evaluates the WHERE filter against each
    /// record, and deletes every matching entity.
    pub fn execute_delete(
        &self,
        raw_query: &str,
        query: &DeleteQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        let entities = manager.query_all(|_| true);
        let mut affected: u64 = 0;

        // Collect IDs to delete first to avoid borrowing issues.
        let mut ids_to_delete = Vec::new();

        for entity in entities {
            let record = match record_search::runtime_table_record_from_entity(entity.clone()) {
                Some(r) => r,
                None => continue,
            };

            let matches = match &query.filter {
                Some(filter) => join_filter::evaluate_runtime_filter(
                    &record,
                    filter,
                    Some(&query.table),
                    Some(&query.table),
                ),
                None => true, // No filter means delete all rows.
            };

            if matches {
                ids_to_delete.push(entity.id);
            }
        }

        for id in ids_to_delete {
            let input = DeleteEntityInput {
                collection: query.table.clone(),
                id,
            };
            self.delete_entity(input)?;
            affected += 1;
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "delete",
            "runtime-dml",
        ))
    }
}

// =============================================================================
// Helper functions for extracting typed values from column/value pairs
// =============================================================================

/// Find a required column value and return it as-is.
fn find_column_value(columns: &[String], values: &[Value], name: &str) -> RedDBResult<Value> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return Ok(values[i].clone());
        }
    }
    Err(RedDBError::Query(format!(
        "required column '{name}' not found in INSERT"
    )))
}

/// Find a required column value and coerce to String.
fn find_column_value_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<String> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Text(s) => Ok(s),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected text, got {other:?}"
        ))),
    }
}

/// Find an optional column value as String.
fn find_column_value_opt_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> Option<String> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Null => None,
                Value::Text(s) => Some(s.clone()),
                Value::Integer(n) => Some(n.to_string()),
                Value::Float(n) => Some(n.to_string()),
                _ => None,
            };
        }
    }
    None
}

/// Find a required column value and coerce to u64.
fn find_column_value_u64(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<u64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Integer(n) => Ok(n as u64),
        Value::UnsignedInteger(n) => Ok(n),
        Value::Text(s) => s
            .parse::<u64>()
            .map_err(|_| RedDBError::Query(format!("column '{name}' expected integer, got '{s}'"))),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected integer, got {other:?}"
        ))),
    }
}

/// Find an optional column value as f32.
fn find_column_value_f32_opt(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> Option<f32> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Float(n) => Some(*n as f32),
                Value::Integer(n) => Some(*n as f32),
                Value::Null => None,
                _ => None,
            };
        }
    }
    None
}

/// Find a required column value and coerce to Vec<f32> (from Value::Vector).
fn find_column_value_vec_f32(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<Vec<f32>> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Vector(v) => Ok(v),
        Value::Json(bytes) => {
            // Try to parse as JSON array of numbers
            let s = std::str::from_utf8(&bytes)
                .map_err(|_| RedDBError::Query(format!("column '{name}' contains invalid UTF-8")))?;
            let arr: Vec<f32> = crate::json::from_str(s)
                .map_err(|e| RedDBError::Query(format!("column '{name}' invalid vector JSON: {e}")))?;
            Ok(arr)
        }
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected vector, got {other:?}"
        ))),
    }
}

/// Extract remaining properties (all columns not in the exclusion list).
fn extract_remaining_properties(
    columns: &[String],
    values: &[Value],
    exclude: &[&str],
) -> Vec<(String, Value)> {
    columns
        .iter()
        .zip(values.iter())
        .filter(|(col, _)| !exclude.iter().any(|e| col.eq_ignore_ascii_case(e)))
        .map(|(col, val)| (col.clone(), val.clone()))
        .collect()
}

/// Convert a storage [`Value`] to a JSON [`crate::json::Value`] for patch
/// operations.  The mapping is straightforward for scalars; blobs are
/// hex-encoded and JSON byte slices are re-parsed.
fn storage_value_to_json(val: &Value) -> crate::json::Value {
    use crate::json::Value as JV;
    match val {
        Value::Null => JV::Null,
        Value::Boolean(b) => JV::Bool(*b),
        Value::Integer(i) => JV::Number(*i as f64),
        Value::Float(f) => JV::Number(*f),
        Value::Text(s) => JV::String(s.clone()),
        Value::Blob(bytes) => JV::String(hex::encode(bytes)),
        Value::Json(bytes) => {
            let s = std::str::from_utf8(bytes).unwrap_or("null");
            crate::json::from_str(s).unwrap_or(JV::Null)
        }
        _ => JV::Null,
    }
}
