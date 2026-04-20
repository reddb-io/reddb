//! Type-Aware Metadata Storage
//!
//! Provides efficient, normalized metadata storage inspired by ChromaDB's
//! approach of using separate columns for each data type.
//!
//! # Type-Aware Pattern
//!
//! Instead of a single variant column that wastes space:
//! ```text
//! | id | key | string_val | int_val | float_val | bool_val |
//! | 1  | "name" | "Alice" | NULL | NULL | NULL |
//! | 1  | "age" | NULL | 25 | NULL | NULL |
//! ```
//!
//! We store in type-specific B-trees:
//! ```text
//! string_values: (entity_id, key) → String
//! int_values: (entity_id, key) → i64
//! float_values: (entity_id, key) → f64
//! bool_values: (entity_id, key) → bool
//! ```
//!
//! Benefits:
//! - No NULL storage overhead
//! - Type-specific indexing (range queries on ints)
//! - Efficient filtering

use std::collections::{BTreeMap, HashMap, HashSet};

use super::entity::EntityId;
use crate::storage::schema::Value;

/// Reference target for metadata cross-links
#[derive(Debug, Clone, PartialEq)]
pub enum RefTarget {
    /// Reference to a table row
    TableRow { table: String, row_id: u64 },
    /// Reference to a graph node
    Node {
        collection: String,
        node_id: EntityId,
    },
    /// Reference to a graph edge
    Edge {
        collection: String,
        edge_id: EntityId,
    },
    /// Reference to a vector
    Vector {
        collection: String,
        vector_id: EntityId,
    },
    /// Generic entity reference
    Entity {
        collection: String,
        entity_id: EntityId,
    },
}

impl RefTarget {
    /// Create a table row reference
    pub fn table(table: impl Into<String>, row_id: u64) -> Self {
        Self::TableRow {
            table: table.into(),
            row_id,
        }
    }

    /// Create a node reference
    pub fn node(collection: impl Into<String>, node_id: EntityId) -> Self {
        Self::Node {
            collection: collection.into(),
            node_id,
        }
    }

    /// Create a vector reference
    pub fn vector(collection: impl Into<String>, vector_id: EntityId) -> Self {
        Self::Vector {
            collection: collection.into(),
            vector_id,
        }
    }

    /// Get the collection/table name
    pub fn collection(&self) -> &str {
        match self {
            Self::TableRow { table, .. } => table,
            Self::Node { collection, .. }
            | Self::Edge { collection, .. }
            | Self::Vector { collection, .. }
            | Self::Entity { collection, .. } => collection,
        }
    }

    /// Get the entity ID (or row_id as EntityId)
    pub fn entity_id(&self) -> EntityId {
        match self {
            Self::TableRow { row_id, .. } => EntityId(*row_id),
            Self::Node { node_id, .. } => *node_id,
            Self::Edge { edge_id, .. } => *edge_id,
            Self::Vector { vector_id, .. } => *vector_id,
            Self::Entity { entity_id, .. } => *entity_id,
        }
    }
}

/// Metadata value types
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<MetadataValue>),
    Object(HashMap<String, MetadataValue>),
    Timestamp(u64),
    Geo {
        lat: f64,
        lon: f64,
    },
    /// Reference to another entity (enables cross-links from metadata)
    Reference(RefTarget),
    /// Multiple references (for one-to-many relationships)
    References(Vec<RefTarget>),
}

impl MetadataValue {
    /// Get the type of this value
    pub fn metadata_type(&self) -> MetadataType {
        match self {
            Self::Null => MetadataType::Null,
            Self::Bool(_) => MetadataType::Bool,
            Self::Int(_) => MetadataType::Int,
            Self::Float(_) => MetadataType::Float,
            Self::String(_) => MetadataType::String,
            Self::Bytes(_) => MetadataType::Bytes,
            Self::Array(_) => MetadataType::Array,
            Self::Object(_) => MetadataType::Object,
            Self::Timestamp(_) => MetadataType::Timestamp,
            Self::Geo { .. } => MetadataType::Geo,
            Self::Reference(_) => MetadataType::Reference,
            Self::References(_) => MetadataType::References,
        }
    }

    /// Check if this value is a reference
    pub fn is_reference(&self) -> bool {
        matches!(self, Self::Reference(_) | Self::References(_))
    }

    /// Get reference target if this is a Reference
    pub fn as_reference(&self) -> Option<&RefTarget> {
        match self {
            Self::Reference(r) => Some(r),
            _ => None,
        }
    }

    /// Get reference targets if this is a References
    pub fn as_references(&self) -> Option<&[RefTarget]> {
        match self {
            Self::References(refs) => Some(refs),
            _ => None,
        }
    }

    /// Convert to Value (schema type)
    pub fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Boolean(*b),
            Self::Int(i) => Value::Integer(*i),
            Self::Float(f) => Value::Float(*f),
            Self::String(s) => Value::text(s.clone()),
            Self::Bytes(b) => Value::Blob(b.clone()),
            Self::Array(_) | Self::Object(_) => {
                // Arrays and Objects are serialized as JSON bytes
                Value::Json(Vec::new())
            }
            Self::Timestamp(t) => Value::Timestamp(*t as i64),
            Self::Geo { lat, lon } => {
                // Geo is stored as JSON
                Value::Json(format!("{{\"lat\":{},\"lon\":{}}}", lat, lon).into_bytes())
            }
            Self::Reference(r) => {
                // Store reference as collection:id string
                Value::text(format!("{}:{}", r.collection(), r.entity_id().0))
            }
            Self::References(refs) => {
                // Store multiple references as comma-separated string
                let parts: Vec<String> = refs
                    .iter()
                    .map(|r| format!("{}:{}", r.collection(), r.entity_id().0))
                    .collect();
                Value::text(parts.join(","))
            }
        }
    }

    /// Create from Value (schema type)
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Boolean(b) => Self::Bool(*b),
            Value::Integer(i) => Self::Int(*i),
            Value::Float(f) => Self::Float(*f),
            Value::Text(s) => Self::String(s.clone()),
            Value::Blob(b) => Self::Bytes(b.clone()),
            Value::Timestamp(t) => Self::Timestamp(*t as u64),
            Value::Json(_) => Self::Object(HashMap::new()), // Simplified
            _ => Self::Null,                                // Other types map to null
        }
    }

    /// Check if value matches a filter
    pub fn matches(&self, filter: &MetadataFilter) -> bool {
        match filter {
            MetadataFilter::Eq(v) => self == v,
            MetadataFilter::Ne(v) => self != v,
            MetadataFilter::Lt(v) => self.compare(v) == Some(std::cmp::Ordering::Less),
            MetadataFilter::Le(v) => matches!(
                self.compare(v),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ),
            MetadataFilter::Gt(v) => self.compare(v) == Some(std::cmp::Ordering::Greater),
            MetadataFilter::Ge(v) => matches!(
                self.compare(v),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            ),
            MetadataFilter::In(values) => values.contains(self),
            MetadataFilter::NotIn(values) => !values.contains(self),
            MetadataFilter::Contains(s) => {
                if let Self::String(str_val) = self {
                    str_val.contains(s)
                } else {
                    false
                }
            }
            MetadataFilter::StartsWith(s) => {
                if let Self::String(str_val) = self {
                    str_val.starts_with(s)
                } else {
                    false
                }
            }
            MetadataFilter::EndsWith(s) => {
                if let Self::String(str_val) = self {
                    str_val.ends_with(s)
                } else {
                    false
                }
            }
            MetadataFilter::IsNull => matches!(self, Self::Null),
            MetadataFilter::IsNotNull => !matches!(self, Self::Null),
            MetadataFilter::Between(low, high) => {
                matches!(
                    self.compare(low),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                ) && matches!(
                    self.compare(high),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
        }
    }

    /// Compare with another value
    fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::String(a), Self::String(b)) => Some(a.cmp(b)),
            (Self::Timestamp(a), Self::Timestamp(b)) => Some(a.cmp(b)),
            // Cross-type numeric comparison
            (Self::Int(a), Self::Float(b)) => (*a as f64).partial_cmp(b),
            (Self::Float(a), Self::Int(b)) => a.partial_cmp(&(*b as f64)),
            _ => None,
        }
    }
}

/// Metadata type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataType {
    Null,
    Bool,
    Int,
    Float,
    String,
    Bytes,
    Array,
    Object,
    Timestamp,
    Geo,
    /// Single reference to another entity
    Reference,
    /// Multiple references to other entities
    References,
}

/// Metadata filter operations
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataFilter {
    Eq(MetadataValue),
    Ne(MetadataValue),
    Lt(MetadataValue),
    Le(MetadataValue),
    Gt(MetadataValue),
    Ge(MetadataValue),
    In(Vec<MetadataValue>),
    NotIn(Vec<MetadataValue>),
    Contains(String),
    StartsWith(String),
    EndsWith(String),
    IsNull,
    IsNotNull,
    Between(MetadataValue, MetadataValue),
}

/// Metadata for an entity (key-value pairs)
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// The metadata fields
    pub fields: HashMap<String, MetadataValue>,
}

impl Metadata {
    /// Create empty metadata
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with fields
    pub fn with_fields(fields: HashMap<String, MetadataValue>) -> Self {
        Self { fields }
    }

    /// Set a field
    pub fn set(&mut self, key: impl Into<String>, value: MetadataValue) {
        self.fields.insert(key.into(), value);
    }

    /// Get a field
    pub fn get(&self, key: &str) -> Option<&MetadataValue> {
        self.fields.get(key)
    }

    /// Remove a field
    pub fn remove(&mut self, key: &str) -> Option<MetadataValue> {
        self.fields.remove(key)
    }

    /// Check if field exists
    pub fn has(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// Number of fields
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Iterate over fields
    pub fn iter(&self) -> impl Iterator<Item = (&String, &MetadataValue)> {
        self.fields.iter()
    }

    /// Check if metadata matches all filters
    pub fn matches_all(&self, filters: &[(String, MetadataFilter)]) -> bool {
        filters.iter().all(|(key, filter)| {
            if let Some(value) = self.get(key) {
                value.matches(filter)
            } else {
                matches!(filter, MetadataFilter::IsNull)
            }
        })
    }

    /// Check if metadata matches any filter
    pub fn matches_any(&self, filters: &[(String, MetadataFilter)]) -> bool {
        filters.iter().any(|(key, filter)| {
            if let Some(value) = self.get(key) {
                value.matches(filter)
            } else {
                matches!(filter, MetadataFilter::IsNull)
            }
        })
    }

    /// Merge another metadata into this one
    pub fn merge(&mut self, other: Metadata) {
        for (key, value) in other.fields {
            self.fields.insert(key, value);
        }
    }
}

/// Type-specific column for efficient storage
#[derive(Debug, Clone)]
pub enum TypedColumn {
    Bool(BTreeMap<(EntityId, String), bool>),
    Int(BTreeMap<(EntityId, String), i64>),
    Float(BTreeMap<(EntityId, String), f64>),
    String(BTreeMap<(EntityId, String), String>),
    Bytes(BTreeMap<(EntityId, String), Vec<u8>>),
    Timestamp(BTreeMap<(EntityId, String), u64>),
}

/// Type-aware metadata storage
///
/// Uses separate B-trees for each type, enabling:
/// - Efficient range queries on numeric types
/// - No NULL storage overhead
/// - Type-specific indexing
#[derive(Debug, Default)]
pub struct MetadataStorage {
    /// Boolean values
    bool_values: BTreeMap<(EntityId, String), bool>,
    /// Integer values
    int_values: BTreeMap<(EntityId, String), i64>,
    /// Float values
    float_values: BTreeMap<(EntityId, String), f64>,
    /// String values
    string_values: BTreeMap<(EntityId, String), String>,
    /// Bytes values
    bytes_values: BTreeMap<(EntityId, String), Vec<u8>>,
    /// Timestamp values
    timestamp_values: BTreeMap<(EntityId, String), u64>,
    /// Complex values (arrays, objects, geo) - less common
    complex_values: HashMap<(EntityId, String), MetadataValue>,
    /// Track which keys exist for each entity
    entity_keys: HashMap<EntityId, HashSet<String>>,
}

impl MetadataStorage {
    /// Create new metadata storage
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a metadata value for an entity
    pub fn set(&mut self, entity_id: EntityId, key: impl Into<String>, value: MetadataValue) {
        let key = key.into();

        // Remove old value if exists (might be different type)
        self.remove_value(&entity_id, &key);

        // Track key for entity
        self.entity_keys
            .entry(entity_id)
            .or_default()
            .insert(key.clone());

        // Store in appropriate type-specific map
        match value {
            MetadataValue::Null => {
                // Don't store nulls, just track the key
            }
            MetadataValue::Bool(b) => {
                self.bool_values.insert((entity_id, key), b);
            }
            MetadataValue::Int(i) => {
                self.int_values.insert((entity_id, key), i);
            }
            MetadataValue::Float(f) => {
                self.float_values.insert((entity_id, key), f);
            }
            MetadataValue::String(s) => {
                self.string_values.insert((entity_id, key), s);
            }
            MetadataValue::Bytes(b) => {
                self.bytes_values.insert((entity_id, key), b);
            }
            MetadataValue::Timestamp(t) => {
                self.timestamp_values.insert((entity_id, key), t);
            }
            complex @ (MetadataValue::Array(_)
            | MetadataValue::Object(_)
            | MetadataValue::Geo { .. }
            | MetadataValue::Reference(_)
            | MetadataValue::References(_)) => {
                self.complex_values.insert((entity_id, key), complex);
            }
        }
    }

    /// Get a metadata value for an entity
    pub fn get(&self, entity_id: EntityId, key: &str) -> Option<MetadataValue> {
        let key_tuple = (entity_id, key.to_string());

        // Check each type-specific map
        if let Some(b) = self.bool_values.get(&key_tuple) {
            return Some(MetadataValue::Bool(*b));
        }
        if let Some(i) = self.int_values.get(&key_tuple) {
            return Some(MetadataValue::Int(*i));
        }
        if let Some(f) = self.float_values.get(&key_tuple) {
            return Some(MetadataValue::Float(*f));
        }
        if let Some(s) = self.string_values.get(&key_tuple) {
            return Some(MetadataValue::String(s.clone()));
        }
        if let Some(b) = self.bytes_values.get(&key_tuple) {
            return Some(MetadataValue::Bytes(b.clone()));
        }
        if let Some(t) = self.timestamp_values.get(&key_tuple) {
            return Some(MetadataValue::Timestamp(*t));
        }
        if let Some(c) = self.complex_values.get(&key_tuple) {
            return Some(c.clone());
        }

        // Check if key exists but value is null
        if self
            .entity_keys
            .get(&entity_id)
            .is_some_and(|keys| keys.contains(key))
        {
            return Some(MetadataValue::Null);
        }

        None
    }

    /// Get all metadata for an entity
    pub fn get_all(&self, entity_id: EntityId) -> Metadata {
        let mut metadata = Metadata::new();

        if let Some(keys) = self.entity_keys.get(&entity_id) {
            for key in keys {
                if let Some(value) = self.get(entity_id, key) {
                    metadata.set(key.clone(), value);
                }
            }
        }

        metadata
    }

    /// Set all metadata for an entity
    pub fn set_all(&mut self, entity_id: EntityId, metadata: &Metadata) {
        // Clear existing
        self.remove_all(entity_id);

        // Set new values
        for (key, value) in metadata.iter() {
            self.set(entity_id, key.clone(), value.clone());
        }
    }

    /// Remove all metadata for an entity
    pub fn remove_all(&mut self, entity_id: EntityId) {
        if let Some(keys) = self.entity_keys.remove(&entity_id) {
            for key in keys {
                self.remove_value(&entity_id, &key);
            }
        }
    }

    /// Remove a specific value
    fn remove_value(&mut self, entity_id: &EntityId, key: &str) {
        let key_tuple = (*entity_id, key.to_string());
        self.bool_values.remove(&key_tuple);
        self.int_values.remove(&key_tuple);
        self.float_values.remove(&key_tuple);
        self.string_values.remove(&key_tuple);
        self.bytes_values.remove(&key_tuple);
        self.timestamp_values.remove(&key_tuple);
        self.complex_values.remove(&key_tuple);
    }

    /// Find entities matching int range
    pub fn filter_int_range(&self, key: &str, min: Option<i64>, max: Option<i64>) -> Vec<EntityId> {
        let mut results = Vec::new();

        for ((entity_id, k), value) in &self.int_values {
            if k == key {
                let in_range = match (min, max) {
                    (Some(lo), Some(hi)) => *value >= lo && *value <= hi,
                    (Some(lo), None) => *value >= lo,
                    (None, Some(hi)) => *value <= hi,
                    (None, None) => true,
                };
                if in_range {
                    results.push(*entity_id);
                }
            }
        }

        results
    }

    /// Find entities matching string prefix
    pub fn filter_string_prefix(&self, key: &str, prefix: &str) -> Vec<EntityId> {
        let mut results = Vec::new();

        for ((entity_id, k), value) in &self.string_values {
            if k == key && value.starts_with(prefix) {
                results.push(*entity_id);
            }
        }

        results
    }

    /// Find entities where key equals value
    pub fn filter_eq(&self, key: &str, value: &MetadataValue) -> Vec<EntityId> {
        let mut results = Vec::new();

        match value {
            MetadataValue::Bool(target) => {
                for ((entity_id, k), v) in &self.bool_values {
                    if k == key && v == target {
                        results.push(*entity_id);
                    }
                }
            }
            MetadataValue::Int(target) => {
                for ((entity_id, k), v) in &self.int_values {
                    if k == key && v == target {
                        results.push(*entity_id);
                    }
                }
            }
            MetadataValue::Float(target) => {
                for ((entity_id, k), v) in &self.float_values {
                    if k == key && (v - target).abs() < f64::EPSILON {
                        results.push(*entity_id);
                    }
                }
            }
            MetadataValue::String(target) => {
                for ((entity_id, k), v) in &self.string_values {
                    if k == key && v == target {
                        results.push(*entity_id);
                    }
                }
            }
            _ => {
                // For complex types, do full scan
                for ((entity_id, k), v) in &self.complex_values {
                    if k == key && v == value {
                        results.push(*entity_id);
                    }
                }
            }
        }

        results
    }

    /// Number of entities with metadata
    pub fn entity_count(&self) -> usize {
        self.entity_keys.len()
    }

    /// Total number of key-value pairs
    pub fn value_count(&self) -> usize {
        self.bool_values.len()
            + self.int_values.len()
            + self.float_values.len()
            + self.string_values.len()
            + self.bytes_values.len()
            + self.timestamp_values.len()
            + self.complex_values.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_storage() {
        let mut storage = MetadataStorage::new();
        let entity_id = EntityId::new(1);

        storage.set(
            entity_id,
            "name",
            MetadataValue::String("Alice".to_string()),
        );
        storage.set(entity_id, "age", MetadataValue::Int(25));
        storage.set(entity_id, "active", MetadataValue::Bool(true));
        storage.set(entity_id, "score", MetadataValue::Float(95.5));

        assert_eq!(
            storage.get(entity_id, "name"),
            Some(MetadataValue::String("Alice".to_string()))
        );
        assert_eq!(storage.get(entity_id, "age"), Some(MetadataValue::Int(25)));
        assert_eq!(
            storage.get(entity_id, "active"),
            Some(MetadataValue::Bool(true))
        );
    }

    #[test]
    fn test_int_range_filter() {
        let mut storage = MetadataStorage::new();

        for i in 0..10 {
            storage.set(EntityId::new(i), "value", MetadataValue::Int(i as i64 * 10));
        }

        let results = storage.filter_int_range("value", Some(30), Some(70));
        assert_eq!(results.len(), 5); // 30, 40, 50, 60, 70
    }

    #[test]
    fn test_string_prefix_filter() {
        let mut storage = MetadataStorage::new();

        storage.set(
            EntityId::new(1),
            "name",
            MetadataValue::String("Alice".to_string()),
        );
        storage.set(
            EntityId::new(2),
            "name",
            MetadataValue::String("Bob".to_string()),
        );
        storage.set(
            EntityId::new(3),
            "name",
            MetadataValue::String("Alicia".to_string()),
        );

        let results = storage.filter_string_prefix("name", "Ali");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_metadata_matches() {
        let mut meta = Metadata::new();
        meta.set("status", MetadataValue::String("active".to_string()));
        meta.set("count", MetadataValue::Int(5));

        let filters = vec![
            (
                "status".to_string(),
                MetadataFilter::Eq(MetadataValue::String("active".to_string())),
            ),
            (
                "count".to_string(),
                MetadataFilter::Gt(MetadataValue::Int(3)),
            ),
        ];

        assert!(meta.matches_all(&filters));
    }

    #[test]
    fn test_get_all_metadata() {
        let mut storage = MetadataStorage::new();
        let entity_id = EntityId::new(1);

        storage.set(entity_id, "a", MetadataValue::Int(1));
        storage.set(entity_id, "b", MetadataValue::String("hello".to_string()));
        storage.set(entity_id, "c", MetadataValue::Bool(true));

        let metadata = storage.get_all(entity_id);
        assert_eq!(metadata.len(), 3);
        assert!(metadata.has("a"));
        assert!(metadata.has("b"));
        assert!(metadata.has("c"));
    }
}
