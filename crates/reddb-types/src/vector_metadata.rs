//! Vector-metadata AST leaves (ADR 0053, RQL Phase 2 S4b).
//!
//! [`MetadataFilter`] is referenced by the canonical SQL AST
//! (`VectorQuery.filter`). It and its data dependencies — [`MetadataValue`] and
//! [`MetadataEntry`] — are re-homed here so the AST resolves entirely against
//! `reddb-io-types`. Their inherent comparison methods depend only on [`Value`],
//! the [`canonical_key`](crate::canonical_key) ordering, and
//! [`partial_compare_values`](crate::value_compare) — all already neutral — so
//! the move is byte-faithful and does **not** drag the vector engine across.
//!
//! The server-side inverted index (`KeyIndex` / `MetadataStore`) stays in
//! `storage::engine::vector_metadata`, which keeps a re-export shim and
//! consumes [`metadata_value_to_canonical_key`] from here.

use crate::canonical_key::{value_to_canonical_key, CanonicalKey};
use crate::types::Value;
use crate::value_compare::partial_compare_values;

/// A metadata value that can be one of several types
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// String value
    String(String),
    /// Integer value
    Integer(i64),
    /// Floating point value
    Float(f64),
    /// Boolean value
    Bool(bool),
    /// Null value
    Null,
}

impl MetadataValue {
    /// Check if this value matches another for equality
    pub fn matches_eq(&self, other: &MetadataValue) -> bool {
        compare_metadata_values(self, other)
            .map(|ord| ord == std::cmp::Ordering::Equal)
            .unwrap_or(false)
    }

    /// Compare for ordering (returns None for incompatible types)
    pub fn compare(&self, other: &MetadataValue) -> Option<std::cmp::Ordering> {
        compare_metadata_values(self, other)
    }

    /// Check if this string value contains a substring
    pub fn contains_str(&self, needle: &str) -> bool {
        match self {
            MetadataValue::String(s) => s.contains(needle),
            _ => false,
        }
    }

    /// Check if this string value starts with a prefix
    pub fn starts_with(&self, prefix: &str) -> bool {
        match self {
            MetadataValue::String(s) => s.starts_with(prefix),
            _ => false,
        }
    }

    /// Check if this string value ends with a suffix
    pub fn ends_with(&self, suffix: &str) -> bool {
        match self {
            MetadataValue::String(s) => s.ends_with(suffix),
            _ => false,
        }
    }
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<i64> for MetadataValue {
    fn from(i: i64) -> Self {
        MetadataValue::Integer(i)
    }
}

impl From<i32> for MetadataValue {
    fn from(i: i32) -> Self {
        MetadataValue::Integer(i as i64)
    }
}

impl From<f64> for MetadataValue {
    fn from(f: f64) -> Self {
        MetadataValue::Float(f)
    }
}

impl From<f32> for MetadataValue {
    fn from(f: f32) -> Self {
        MetadataValue::Float(f as f64)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Bool(b)
    }
}

fn metadata_value_to_storage_value(value: &MetadataValue) -> Value {
    match value {
        MetadataValue::String(s) => Value::text(s.clone()),
        MetadataValue::Integer(i) => Value::Integer(*i),
        MetadataValue::Float(f) => Value::Float(*f),
        MetadataValue::Bool(b) => Value::Boolean(*b),
        MetadataValue::Null => Value::Null,
    }
}

/// Map a [`MetadataValue`] to its canonical secondary-index key, when the
/// value participates in the ordered index. Consumed by the server-side
/// inverted index (`KeyIndex`).
pub fn metadata_value_to_canonical_key(value: &MetadataValue) -> Option<CanonicalKey> {
    let storage_value = metadata_value_to_storage_value(value);
    value_to_canonical_key(&storage_value)
}

fn compare_metadata_values(
    left: &MetadataValue,
    right: &MetadataValue,
) -> Option<std::cmp::Ordering> {
    let left_value = metadata_value_to_storage_value(left);
    let right_value = metadata_value_to_storage_value(right);
    partial_compare_values(&left_value, &right_value).or_else(|| {
        let left_key = value_to_canonical_key(&left_value)?;
        let right_key = value_to_canonical_key(&right_value)?;
        (left_key.family() == right_key.family()).then(|| left_key.cmp(&right_key))
    })
}

/// A metadata entry containing key-value pairs organized by type
#[derive(Debug, Clone, Default)]
pub struct MetadataEntry {
    /// String metadata values
    pub strings: std::collections::HashMap<String, String>,
    /// Integer metadata values
    pub integers: std::collections::HashMap<String, i64>,
    /// Float metadata values
    pub floats: std::collections::HashMap<String, f64>,
    /// Boolean metadata values
    pub bools: std::collections::HashMap<String, bool>,
}

impl MetadataEntry {
    /// Create a new empty metadata entry
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a metadata value
    pub fn insert(&mut self, key: impl Into<String>, value: MetadataValue) {
        let key = key.into();
        match value {
            MetadataValue::String(s) => {
                self.strings.insert(key, s);
            }
            MetadataValue::Integer(i) => {
                self.integers.insert(key, i);
            }
            MetadataValue::Float(f) => {
                self.floats.insert(key, f);
            }
            MetadataValue::Bool(b) => {
                self.bools.insert(key, b);
            }
            MetadataValue::Null => {
                // Remove from all maps
                self.strings.remove(&key);
                self.integers.remove(&key);
                self.floats.remove(&key);
                self.bools.remove(&key);
            }
        }
    }

    /// Get a metadata value by key
    pub fn get(&self, key: &str) -> Option<MetadataValue> {
        if let Some(s) = self.strings.get(key) {
            return Some(MetadataValue::String(s.clone()));
        }
        if let Some(i) = self.integers.get(key) {
            return Some(MetadataValue::Integer(*i));
        }
        if let Some(f) = self.floats.get(key) {
            return Some(MetadataValue::Float(*f));
        }
        if let Some(b) = self.bools.get(key) {
            return Some(MetadataValue::Bool(*b));
        }
        None
    }

    /// Check if a key exists
    pub fn contains_key(&self, key: &str) -> bool {
        self.strings.contains_key(key)
            || self.integers.contains_key(key)
            || self.floats.contains_key(key)
            || self.bools.contains_key(key)
    }

    /// Get all keys
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        keys.extend(self.strings.keys().cloned());
        keys.extend(self.integers.keys().cloned());
        keys.extend(self.floats.keys().cloned());
        keys.extend(self.bools.keys().cloned());
        keys
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
            && self.integers.is_empty()
            && self.floats.is_empty()
            && self.bools.is_empty()
    }
}

/// Metadata filter operators
#[derive(Debug, Clone)]
pub enum MetadataFilter {
    /// Equal: key == value
    Eq(String, MetadataValue),
    /// Not equal: key != value
    Ne(String, MetadataValue),
    /// Greater than: key > value
    Gt(String, MetadataValue),
    /// Greater than or equal: key >= value
    Gte(String, MetadataValue),
    /// Less than: key < value
    Lt(String, MetadataValue),
    /// Less than or equal: key <= value
    Lte(String, MetadataValue),
    /// In set: key in [values]
    In(String, Vec<MetadataValue>),
    /// Not in set: key not in [values]
    NotIn(String, Vec<MetadataValue>),
    /// String contains: key contains substring
    Contains(String, String),
    /// String starts with: key starts with prefix
    StartsWith(String, String),
    /// String ends with: key ends with suffix
    EndsWith(String, String),
    /// Key exists
    Exists(String),
    /// Key does not exist
    NotExists(String),
    /// Logical AND of filters
    And(Vec<MetadataFilter>),
    /// Logical OR of filters
    Or(Vec<MetadataFilter>),
    /// Logical NOT of filter
    Not(Box<MetadataFilter>),
}

impl MetadataFilter {
    /// Create an equality filter
    pub fn eq(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Eq(key.into(), value.into())
    }

    /// Create a not-equal filter
    pub fn ne(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Ne(key.into(), value.into())
    }

    /// Create a greater-than filter
    pub fn gt(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Gt(key.into(), value.into())
    }

    /// Create a greater-than-or-equal filter
    pub fn gte(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Gte(key.into(), value.into())
    }

    /// Create a less-than filter
    pub fn lt(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Lt(key.into(), value.into())
    }

    /// Create a less-than-or-equal filter
    pub fn lte(key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        MetadataFilter::Lte(key.into(), value.into())
    }

    /// Create an AND filter
    pub fn and(filters: Vec<MetadataFilter>) -> Self {
        MetadataFilter::And(filters)
    }

    /// Create an OR filter
    pub fn or(filters: Vec<MetadataFilter>) -> Self {
        MetadataFilter::Or(filters)
    }

    /// Create a NOT filter
    pub fn not(filter: MetadataFilter) -> Self {
        MetadataFilter::Not(Box::new(filter))
    }

    /// Check if a metadata entry matches this filter
    pub fn matches(&self, entry: &MetadataEntry) -> bool {
        match self {
            MetadataFilter::Eq(key, value) => {
                entry.get(key).map(|v| v.matches_eq(value)).unwrap_or(false)
            }
            MetadataFilter::Ne(key, value) => {
                entry.get(key).map(|v| !v.matches_eq(value)).unwrap_or(true)
            }
            MetadataFilter::Gt(key, value) => entry
                .get(key)
                .and_then(|v| v.compare(value))
                .map(|ord| ord == std::cmp::Ordering::Greater)
                .unwrap_or(false),
            MetadataFilter::Gte(key, value) => entry
                .get(key)
                .and_then(|v| v.compare(value))
                .map(|ord| ord != std::cmp::Ordering::Less)
                .unwrap_or(false),
            MetadataFilter::Lt(key, value) => entry
                .get(key)
                .and_then(|v| v.compare(value))
                .map(|ord| ord == std::cmp::Ordering::Less)
                .unwrap_or(false),
            MetadataFilter::Lte(key, value) => entry
                .get(key)
                .and_then(|v| v.compare(value))
                .map(|ord| ord != std::cmp::Ordering::Greater)
                .unwrap_or(false),
            MetadataFilter::In(key, values) => entry
                .get(key)
                .map(|v| values.iter().any(|val| v.matches_eq(val)))
                .unwrap_or(false),
            MetadataFilter::NotIn(key, values) => entry
                .get(key)
                .map(|v| !values.iter().any(|val| v.matches_eq(val)))
                .unwrap_or(true),
            MetadataFilter::Contains(key, needle) => entry
                .get(key)
                .map(|v| v.contains_str(needle))
                .unwrap_or(false),
            MetadataFilter::StartsWith(key, prefix) => entry
                .get(key)
                .map(|v| v.starts_with(prefix))
                .unwrap_or(false),
            MetadataFilter::EndsWith(key, suffix) => {
                entry.get(key).map(|v| v.ends_with(suffix)).unwrap_or(false)
            }
            MetadataFilter::Exists(key) => entry.contains_key(key),
            MetadataFilter::NotExists(key) => !entry.contains_key(key),
            MetadataFilter::And(filters) => filters.iter().all(|f| f.matches(entry)),
            MetadataFilter::Or(filters) => filters.iter().any(|f| f.matches(entry)),
            MetadataFilter::Not(filter) => !filter.matches(entry),
        }
    }
}
