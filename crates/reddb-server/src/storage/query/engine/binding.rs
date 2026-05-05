//! Variable Bindings
//!
//! Maps variables to values during query execution.
//!
//! # Design
//!
//! - Immutable bindings for thread safety
//! - Parent binding support for scoped lookups
//! - Size-optimized implementations for common cases
//! - Builder pattern for construction

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::json;
use crate::serde_json::{Map as JsonMap, Value as JsonValue};

/// A variable in a query
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Var {
    name: String,
}

impl Var {
    /// Create a new variable
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }

    /// Get variable name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if this is an anonymous variable
    pub fn is_anonymous(&self) -> bool {
        self.name.starts_with('_')
    }
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "?{}", self.name)
    }
}

impl From<&str> for Var {
    fn from(s: &str) -> Self {
        // Strip leading ? if present
        let name = s.strip_prefix('?').unwrap_or(s);
        Self::new(name)
    }
}

/// A value that can be bound to a variable
#[derive(Debug, Clone)]
pub enum Value {
    /// Node/vertex ID
    Node(String),
    /// Edge ID
    Edge(String),
    /// Literal string
    String(String),
    /// Integer
    Integer(i64),
    /// Float
    Float(f64),
    /// Boolean
    Boolean(bool),
    /// URI/IRI
    Uri(String),
    /// Null/unbound
    Null,
}

impl Value {
    /// Get as string
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::Node(s) | Value::Edge(s) | Value::Uri(s) => Some(s),
            _ => None,
        }
    }

    /// Get as i64
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Get as f64
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Check if null
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Convert to JSON
    pub fn to_json(&self) -> JsonValue {
        match self {
            Value::Node(id) => json!({ "type": "node", "id": id }),
            Value::Edge(id) => json!({ "type": "edge", "id": id }),
            Value::String(s) => JsonValue::String(s.clone()),
            Value::Integer(i) => json!(i),
            Value::Float(f) => json!(f),
            Value::Boolean(b) => JsonValue::Bool(*b),
            Value::Uri(uri) => json!({ "type": "uri", "value": uri }),
            Value::Null => JsonValue::Null,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Node(a), Value::Node(b)) => a == b,
            (Value::Edge(a), Value::Edge(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => {
                (a - b).abs() < f64::EPSILON || (a.is_nan() && b.is_nan())
            }
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::Uri(a), Value::Uri(b)) => a == b,
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Value::Node(s) => {
                0u8.hash(state);
                s.hash(state);
            }
            Value::Edge(s) => {
                1u8.hash(state);
                s.hash(state);
            }
            Value::String(s) => {
                2u8.hash(state);
                s.hash(state);
            }
            Value::Integer(i) => {
                3u8.hash(state);
                i.hash(state);
            }
            Value::Boolean(b) => {
                4u8.hash(state);
                b.hash(state);
            }
            Value::Uri(s) => {
                5u8.hash(state);
                s.hash(state);
            }
            Value::Float(f) => {
                6u8.hash(state);
                f.to_bits().hash(state);
            }
            Value::Null => {
                7u8.hash(state);
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Node(id) => write!(f, "<node:{}>", id),
            Value::Edge(id) => write!(f, "<edge:{}>", id),
            Value::String(s) => write!(f, "\"{}\"", s),
            Value::Integer(i) => write!(f, "{}", i),
            Value::Float(fl) => write!(f, "{}", fl),
            Value::Boolean(b) => write!(f, "{}", b),
            Value::Uri(uri) => write!(f, "<{}>", uri),
            Value::Null => write!(f, "NULL"),
        }
    }
}

/// Immutable binding from variables to values
#[derive(Debug, Clone)]
pub struct Binding {
    /// Variable mappings
    bindings: HashMap<Var, Value>,
    /// Parent binding for scoped lookups
    parent: Option<Arc<Binding>>,
}

impl Binding {
    /// Create an empty binding
    pub fn empty() -> Self {
        Self {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    /// Create binding with single variable
    pub fn one(var: Var, value: Value) -> Self {
        let mut bindings = HashMap::new();
        bindings.insert(var, value);
        Self {
            bindings,
            parent: None,
        }
    }

    /// Create binding with two variables
    pub fn two(var1: Var, val1: Value, var2: Var, val2: Value) -> Self {
        let mut bindings = HashMap::new();
        bindings.insert(var1, val1);
        bindings.insert(var2, val2);
        Self {
            bindings,
            parent: None,
        }
    }

    /// Create binding with parent scope
    pub fn with_parent(self, parent: Arc<Binding>) -> Self {
        Self {
            bindings: self.bindings,
            parent: Some(parent),
        }
    }

    /// Get value for variable
    pub fn get(&self, var: &Var) -> Option<&Value> {
        self.bindings
            .get(var)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(var)))
    }

    /// Check if variable is bound
    pub fn contains(&self, var: &Var) -> bool {
        self.bindings.contains_key(var)
            || self
                .parent
                .as_ref()
                .map(|p| p.contains(var))
                .unwrap_or(false)
    }

    /// Get all variables in this binding (not parent)
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.bindings.keys()
    }

    /// Get all variables including parent
    pub fn all_vars(&self) -> Vec<&Var> {
        let mut vars: Vec<&Var> = self.bindings.keys().collect();
        if let Some(ref parent) = self.parent {
            for v in parent.all_vars() {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        vars
    }

    /// Number of bindings (not including parent)
    pub fn size(&self) -> usize {
        self.bindings.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.parent.as_ref().map(|p| p.is_empty()).unwrap_or(true)
    }

    /// Merge two bindings (returns None if conflict)
    pub fn merge(&self, other: &Binding) -> Option<Binding> {
        let mut merged = self.bindings.clone();

        for (var, value) in &other.bindings {
            if let Some(existing) = self.get(var) {
                if existing != value {
                    return None; // Conflict
                }
            } else {
                merged.insert(var.clone(), value.clone());
            }
        }

        Some(Binding {
            bindings: merged,
            parent: self.parent.clone(),
        })
    }

    /// Project to subset of variables
    pub fn project(&self, vars: &[Var]) -> Binding {
        let mut projected = HashMap::new();
        for var in vars {
            if let Some(value) = self.get(var) {
                projected.insert(var.clone(), value.clone());
            }
        }
        Binding {
            bindings: projected,
            parent: None,
        }
    }

    /// Extend with additional binding
    pub fn extend(&self, var: Var, value: Value) -> Binding {
        let mut bindings = self.bindings.clone();
        bindings.insert(var, value);
        Binding {
            bindings,
            parent: self.parent.clone(),
        }
    }

    /// Convert to map
    pub fn to_map(&self) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        if let Some(ref parent) = self.parent {
            for (k, v) in parent.to_map() {
                map.insert(k, v);
            }
        }
        for (var, value) in &self.bindings {
            map.insert(var.name().to_string(), value.clone());
        }
        map
    }

    /// Convert to JSON
    pub fn to_json(&self) -> JsonValue {
        let map: JsonMap<String, JsonValue> = self
            .to_map()
            .into_iter()
            .map(|(k, v)| (k, v.to_json()))
            .collect();
        JsonValue::Object(map)
    }
}

impl Default for Binding {
    fn default() -> Self {
        Self::empty()
    }
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        // Compare flattened bindings
        let self_map = self.to_map();
        let other_map = other.to_map();
        self_map == other_map
    }
}

impl Eq for Binding {}

impl Hash for Binding {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash sorted entries for consistency
        let mut entries: Vec<_> = self.to_map().into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in entries {
            k.hash(state);
            v.hash(state);
        }
    }
}

/// Builder for creating bindings
pub struct BindingBuilder {
    bindings: HashMap<Var, Value>,
    parent: Option<Arc<Binding>>,
}

impl BindingBuilder {
    /// Create new builder
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    /// Create builder from existing binding
    pub fn from(binding: &Binding) -> Self {
        Self {
            bindings: binding.bindings.clone(),
            parent: binding.parent.clone(),
        }
    }

    /// Set parent binding
    pub fn parent(mut self, parent: Arc<Binding>) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Add variable binding
    pub fn add(mut self, var: Var, value: Value) -> Self {
        self.bindings.insert(var, value);
        self
    }

    /// Add variable binding from string name
    pub fn add_named(self, name: &str, value: Value) -> Self {
        self.add(Var::from(name), value)
    }

    /// Remove variable
    pub fn remove(mut self, var: &Var) -> Self {
        self.bindings.remove(var);
        self
    }

    /// Build the binding
    pub fn build(self) -> Binding {
        Binding {
            bindings: self.bindings,
            parent: self.parent,
        }
    }
}

impl Default for BindingBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var() {
        let v = Var::new("x");
        assert_eq!(v.name(), "x");
        assert!(!v.is_anonymous());

        let anon = Var::new("_g1");
        assert!(anon.is_anonymous());
    }

    #[test]
    fn test_var_from_string() {
        let v1 = Var::from("x");
        let v2 = Var::from("?x");
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_binding_empty() {
        let b = Binding::empty();
        assert!(b.is_empty());
        assert_eq!(b.size(), 0);
    }

    #[test]
    fn test_binding_one() {
        let b = Binding::one(Var::new("x"), Value::Integer(42));
        assert!(!b.is_empty());
        assert_eq!(b.size(), 1);
        assert!(b.contains(&Var::new("x")));
        assert_eq!(b.get(&Var::new("x")), Some(&Value::Integer(42)));
    }

    #[test]
    fn test_binding_parent() {
        let parent = Arc::new(Binding::one(Var::new("x"), Value::Integer(1)));
        let child = Binding::one(Var::new("y"), Value::Integer(2)).with_parent(parent);

        // Child can see parent's bindings
        assert!(child.contains(&Var::new("x")));
        assert!(child.contains(&Var::new("y")));

        // Direct size doesn't include parent
        assert_eq!(child.size(), 1);
    }

    #[test]
    fn test_binding_merge() {
        let b1 = Binding::one(Var::new("x"), Value::Integer(1));
        let b2 = Binding::one(Var::new("y"), Value::Integer(2));

        let merged = b1.merge(&b2).unwrap();
        assert!(merged.contains(&Var::new("x")));
        assert!(merged.contains(&Var::new("y")));
    }

    #[test]
    fn test_binding_merge_conflict() {
        let b1 = Binding::one(Var::new("x"), Value::Integer(1));
        let b2 = Binding::one(Var::new("x"), Value::Integer(2));

        let merged = b1.merge(&b2);
        assert!(merged.is_none()); // Conflict
    }

    #[test]
    fn test_binding_merge_same_value() {
        let b1 = Binding::one(Var::new("x"), Value::Integer(1));
        let b2 = Binding::one(Var::new("x"), Value::Integer(1));

        let merged = b1.merge(&b2).unwrap();
        assert_eq!(merged.get(&Var::new("x")), Some(&Value::Integer(1)));
    }

    #[test]
    fn test_binding_project() {
        let b = Binding::two(
            Var::new("x"),
            Value::Integer(1),
            Var::new("y"),
            Value::Integer(2),
        );

        let projected = b.project(&[Var::new("x")]);
        assert!(projected.contains(&Var::new("x")));
        assert!(!projected.contains(&Var::new("y")));
    }

    #[test]
    fn test_binding_extend() {
        let b = Binding::one(Var::new("x"), Value::Integer(1));
        let extended = b.extend(Var::new("y"), Value::Integer(2));

        assert!(extended.contains(&Var::new("x")));
        assert!(extended.contains(&Var::new("y")));
    }

    #[test]
    fn test_binding_builder() {
        let b = BindingBuilder::new()
            .add_named("x", Value::Integer(1))
            .add_named("y", Value::String("hello".to_string()))
            .build();

        assert_eq!(b.size(), 2);
        assert_eq!(b.get(&Var::new("x")), Some(&Value::Integer(1)));
    }

    #[test]
    fn test_value_display() {
        assert_eq!(format!("{}", Value::Integer(42)), "42");
        assert_eq!(
            format!("{}", Value::String("hello".to_string())),
            "\"hello\""
        );
        assert_eq!(format!("{}", Value::Null), "NULL");
    }
}
