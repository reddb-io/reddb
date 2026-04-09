//! Key-Value query builder
//!
//! Provides a fluent API for key-value operations on a collection.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityId};
use crate::storage::unified::store::UnifiedStore;

use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::{MatchComponents, QueryResult, ScoredMatch};

/// Builder for key-value operations on a collection.
///
/// Supports get, set, delete, list, and filtered scans.
///
/// # Examples
///
/// ```ignore
/// // Get a single value
/// let result = Q::kv("config").get("theme").execute(&store)?;
///
/// // List all KV pairs
/// let result = Q::kv("config").list().execute(&store)?;
///
/// // Filtered scan
/// let result = Q::kv("config").where_("value").equals("dark").execute(&store)?;
/// ```
#[derive(Debug, Clone)]
pub struct KvQueryBuilder {
    pub(crate) collection: String,
    pub(crate) operation: KvOperation,
    pub(crate) filters: Vec<Filter>,
}

#[derive(Debug, Clone)]
pub enum KvOperation {
    /// Get a single key
    Get(String),
    /// Set a key to a value (upsert)
    Set(String, Value),
    /// Delete a key
    Delete(String),
    /// List/scan all KV pairs
    List,
}

impl KvQueryBuilder {
    pub fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            operation: KvOperation::List,
            filters: Vec::new(),
        }
    }

    /// Get a single value by key
    pub fn get(mut self, key: impl Into<String>) -> Self {
        self.operation = KvOperation::Get(key.into());
        self
    }

    /// Set a key-value pair (creates or updates)
    pub fn set(mut self, key: impl Into<String>, value: Value) -> Self {
        self.operation = KvOperation::Set(key.into(), value);
        self
    }

    /// Delete a key
    pub fn delete(mut self, key: impl Into<String>) -> Self {
        self.operation = KvOperation::Delete(key.into());
        self
    }

    /// List all KV pairs in the collection
    pub fn list(mut self) -> Self {
        self.operation = KvOperation::List;
        self
    }

    /// Add a filter condition for list/scan operations
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Execute the KV operation
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_kv_query(self, store)
    }
}

impl FilterAcceptor for KvQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}

/// Execute a KV query operation
fn execute_kv_query(
    query: KvQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = std::time::Instant::now();

    match query.operation {
        KvOperation::Get(key) => {
            let mut matches = Vec::new();
            let mut scanned = 0;
            if let Some(manager) = store.get_collection(&query.collection) {
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    scanned += 1;
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            if let Some(Value::Text(ref k)) = named.get("key") {
                                if k == &key {
                                    matches.push(ScoredMatch {
                                        entity,
                                        score: 1.0,
                                        components: MatchComponents {
                                            structured_match: Some(1.0),
                                            filter_match: true,
                                            final_score: Some(1.0),
                                            ..Default::default()
                                        },
                                        path: None,
                                    });
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Ok(QueryResult {
                matches,
                scanned,
                execution_time_us: start.elapsed().as_micros() as u64,
                explanation: format!("KV get '{}' in {}", key, query.collection),
            })
        }
        KvOperation::Set(key, value) => {
            // Delete existing key if present, then insert new
            if let Some(manager) = store.get_collection(&query.collection) {
                let entities = manager.query_all(|_| true);
                for entity in &entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            if let Some(Value::Text(ref k)) = named.get("key") {
                                if k == &key {
                                    let _ = store.delete(&query.collection, entity.id);
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // Insert the new KV pair as a row
            use crate::storage::unified::entity::{EntityKind, RowData, UnifiedEntity};
            use std::collections::HashMap;

            let id = store.next_entity_id();
            let kind = EntityKind::TableRow {
                table: query.collection.clone(),
                row_id: id.0,
            };
            let key_val = Value::Text(key.clone());
            let columns = vec![key_val.clone(), value.clone()];
            let mut named = HashMap::new();
            named.insert("key".to_string(), key_val);
            named.insert("value".to_string(), value);
            let mut row_data = RowData::new(columns);
            row_data.named = Some(named);

            let entity = UnifiedEntity::new(id, kind, EntityData::Row(row_data));
            let inserted_id = store
                .insert_auto(&query.collection, entity)
                .map_err(|e| ExecutionError::new(format!("{e:?}")))?;

            let inserted_entity = store.get_any(inserted_id).map(|(_, e)| e);
            let mut matches = Vec::new();
            if let Some(entity) = inserted_entity {
                matches.push(ScoredMatch {
                    entity,
                    score: 1.0,
                    components: MatchComponents {
                        structured_match: Some(1.0),
                        filter_match: true,
                        final_score: Some(1.0),
                        ..Default::default()
                    },
                    path: None,
                });
            }

            Ok(QueryResult {
                matches,
                scanned: 0,
                execution_time_us: start.elapsed().as_micros() as u64,
                explanation: format!("KV set '{}' in {}", key, query.collection),
            })
        }
        KvOperation::Delete(key) => {
            let mut scanned = 0;
            let mut deleted = false;
            if let Some(manager) = store.get_collection(&query.collection) {
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    scanned += 1;
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            if let Some(Value::Text(ref k)) = named.get("key") {
                                if k == &key {
                                    let _ = store.delete(&query.collection, entity.id);
                                    deleted = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Ok(QueryResult {
                matches: Vec::new(),
                scanned,
                execution_time_us: start.elapsed().as_micros() as u64,
                explanation: format!(
                    "KV delete '{}' in {} (deleted={})",
                    key, query.collection, deleted
                ),
            })
        }
        KvOperation::List => {
            let mut matches = Vec::new();
            let mut scanned = 0;
            if let Some(manager) = store.get_collection(&query.collection) {
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    scanned += 1;
                    // Only include entities that look like KV pairs (have key+value named fields)
                    let is_kv = matches!(
                        &entity.data,
                        EntityData::Row(ref row) if row.named.as_ref().is_some_and(|n| n.contains_key("key") && n.contains_key("value"))
                    );
                    if is_kv && super::super::helpers::apply_filters(&entity, &query.filters) {
                        matches.push(ScoredMatch {
                            entity,
                            score: 1.0,
                            components: MatchComponents {
                                structured_match: Some(1.0),
                                filter_match: true,
                                final_score: Some(1.0),
                                ..Default::default()
                            },
                            path: None,
                        });
                    }
                }
            }
            Ok(QueryResult {
                matches,
                scanned,
                execution_time_us: start.elapsed().as_micros() as u64,
                explanation: format!("KV list in {}", query.collection),
            })
        }
    }
}
