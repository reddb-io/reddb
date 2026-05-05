//! Traverser System
//!
//! Core traverser abstraction for graph traversal execution.
//!
//! # Concepts
//!
//! - **Traverser**: Carrier of data through a traversal, with bulk and path tracking
//! - **Path**: History of traversed elements
//! - **LoopState**: Loop counter management for repeat() steps
//! - **TraverserRequirement**: Capabilities steps declare they need

use crate::json;
use crate::serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Value carried by a traverser
#[derive(Debug, Clone)]
pub enum TraverserValue {
    /// Vertex/node ID
    Vertex(String),
    /// Edge ID with source and target
    Edge {
        id: String,
        source: String,
        target: String,
        label: String,
    },
    /// Property value
    Property(String, Value),
    /// Path result
    Path(Path),
    /// Map/object
    Map(HashMap<String, Value>),
    /// List
    List(Vec<Value>),
    /// Scalar values
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    /// Null/none
    Null,
}

impl TraverserValue {
    /// Get as vertex ID if applicable
    pub fn as_vertex_id(&self) -> Option<&str> {
        match self {
            TraverserValue::Vertex(id) => Some(id),
            TraverserValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get as edge info if applicable
    pub fn as_edge(&self) -> Option<(&str, &str, &str, &str)> {
        match self {
            TraverserValue::Edge {
                id,
                source,
                target,
                label,
            } => Some((id, source, target, label)),
            _ => None,
        }
    }

    /// Check if null
    pub fn is_null(&self) -> bool {
        matches!(self, TraverserValue::Null)
    }

    /// Convert to JSON value
    pub fn to_json(&self) -> Value {
        match self {
            TraverserValue::Vertex(id) => json!({ "type": "vertex", "id": id }),
            TraverserValue::Edge {
                id,
                source,
                target,
                label,
            } => {
                json!({
                    "type": "edge",
                    "id": id,
                    "source": source,
                    "target": target,
                    "label": label
                })
            }
            TraverserValue::Property(key, value) => {
                json!({ "key": key, "value": value })
            }
            TraverserValue::Path(path) => path.to_json(),
            TraverserValue::Map(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            }
            TraverserValue::List(list) => Value::Array(list.clone()),
            TraverserValue::String(s) => Value::String(s.clone()),
            TraverserValue::Integer(i) => json!(i),
            TraverserValue::Float(f) => json!(f),
            TraverserValue::Boolean(b) => Value::Bool(*b),
            TraverserValue::Null => Value::Null,
        }
    }
}

impl PartialEq for TraverserValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TraverserValue::Vertex(a), TraverserValue::Vertex(b)) => a == b,
            (TraverserValue::String(a), TraverserValue::String(b)) => a == b,
            (TraverserValue::Integer(a), TraverserValue::Integer(b)) => a == b,
            (TraverserValue::Boolean(a), TraverserValue::Boolean(b)) => a == b,
            (TraverserValue::Null, TraverserValue::Null) => true,
            _ => false,
        }
    }
}

impl Eq for TraverserValue {}

impl Hash for TraverserValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            TraverserValue::Vertex(id) => {
                0u8.hash(state);
                id.hash(state);
            }
            TraverserValue::String(s) => {
                1u8.hash(state);
                s.hash(state);
            }
            TraverserValue::Integer(i) => {
                2u8.hash(state);
                i.hash(state);
            }
            TraverserValue::Boolean(b) => {
                3u8.hash(state);
                b.hash(state);
            }
            TraverserValue::Null => {
                4u8.hash(state);
            }
            _ => {
                // For complex types, use debug representation
                255u8.hash(state);
                format!("{:?}", self).hash(state);
            }
        }
    }
}

/// Path through the graph (history of traversed elements)
#[derive(Debug, Clone, Default)]
pub struct Path {
    /// Objects in order of traversal
    objects: Vec<TraverserValue>,
    /// Labels for each position (multiple labels possible per position)
    labels: Vec<Vec<String>>,
}

impl Path {
    /// Create empty path
    pub fn new() -> Self {
        Self::default()
    }

    /// Create path starting with an element
    pub fn start(value: TraverserValue) -> Self {
        Self {
            objects: vec![value],
            labels: vec![vec![]],
        }
    }

    /// Extend path with a new element
    pub fn extend(&mut self, value: TraverserValue) {
        self.objects.push(value);
        self.labels.push(vec![]);
    }

    /// Extend with label
    pub fn extend_with_label(&mut self, value: TraverserValue, label: String) {
        self.objects.push(value);
        self.labels.push(vec![label]);
    }

    /// Add label to last element
    pub fn add_label(&mut self, label: String) {
        if let Some(last_labels) = self.labels.last_mut() {
            if !last_labels.contains(&label) {
                last_labels.push(label);
            }
        }
    }

    /// Get objects in path
    pub fn objects(&self) -> &[TraverserValue] {
        &self.objects
    }

    /// Get labels at position
    pub fn labels_at(&self, index: usize) -> &[String] {
        self.labels.get(index).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get object by label
    pub fn get(&self, label: &str) -> Option<&TraverserValue> {
        for (i, labels) in self.labels.iter().enumerate() {
            if labels.contains(&label.to_string()) {
                return self.objects.get(i);
            }
        }
        None
    }

    /// Get all objects by label (if multiple positions have same label)
    pub fn get_all(&self, label: &str) -> Vec<&TraverserValue> {
        let mut result = Vec::new();
        for (i, labels) in self.labels.iter().enumerate() {
            if labels.contains(&label.to_string()) {
                if let Some(obj) = self.objects.get(i) {
                    result.push(obj);
                }
            }
        }
        result
    }

    /// Check if path has label
    pub fn has_label(&self, label: &str) -> bool {
        self.labels
            .iter()
            .any(|labels| labels.contains(&label.to_string()))
    }

    /// Path length
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Get head (last element)
    pub fn head(&self) -> Option<&TraverserValue> {
        self.objects.last()
    }

    /// Clone path and extend
    pub fn clone_and_extend(&self, value: TraverserValue) -> Self {
        let mut new_path = self.clone();
        new_path.extend(value);
        new_path
    }

    /// Convert to JSON
    pub fn to_json(&self) -> Value {
        let objects: Vec<_> = self.objects.iter().map(|o| o.to_json()).collect();
        let labels: Vec<_> = self
            .labels
            .iter()
            .map(|l| Value::Array(l.iter().map(|s| Value::String(s.clone())).collect()))
            .collect();

        json!({
            "objects": objects,
            "labels": labels
        })
    }

    /// Retract path to only keep elements with specified labels
    pub fn retract(&mut self, keep_labels: &[String]) {
        let mut new_objects = Vec::new();
        let mut new_labels = Vec::new();

        for (i, labels) in self.labels.iter().enumerate() {
            if labels.iter().any(|l| keep_labels.contains(l)) {
                if let Some(obj) = self.objects.get(i) {
                    new_objects.push(obj.clone());
                    new_labels.push(labels.clone());
                }
            }
        }

        self.objects = new_objects;
        self.labels = new_labels;
    }
}

/// Loop state for repeat() steps
#[derive(Debug, Clone, Default)]
pub struct LoopState {
    /// Named loop counters
    loops: HashMap<String, u32>,
    /// Current loop name (if any)
    current_loop: Option<String>,
}

impl LoopState {
    /// Create new loop state
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize loop counter
    pub fn init_loop(&mut self, name: &str) {
        self.loops.insert(name.to_string(), 0);
        self.current_loop = Some(name.to_string());
    }

    /// Increment loop counter
    pub fn incr_loop(&mut self, name: &str) {
        if let Some(count) = self.loops.get_mut(name) {
            *count += 1;
        }
    }

    /// Get loop counter
    pub fn loop_count(&self, name: &str) -> u32 {
        self.loops.get(name).copied().unwrap_or(0)
    }

    /// Get current loop count
    pub fn current_count(&self) -> u32 {
        self.current_loop
            .as_ref()
            .and_then(|name| self.loops.get(name))
            .copied()
            .unwrap_or(0)
    }

    /// Reset loop counter
    pub fn reset_loop(&mut self, name: &str) {
        self.loops.remove(name);
        if self.current_loop.as_deref() == Some(name) {
            self.current_loop = None;
        }
    }
}

/// Traverser - carrier of data through traversal
#[derive(Debug, Clone)]
pub struct Traverser {
    /// Current value
    value: TraverserValue,
    /// Bulk count (for optimization)
    bulk: u64,
    /// Path history (if PATH requirement enabled)
    path: Option<Path>,
    /// Loop state (if LOOP requirement enabled)
    loops: Option<LoopState>,
    /// Side-effect data
    sack: Option<Value>,
    /// Step-specific tags
    tags: HashMap<String, String>,
}

impl Traverser {
    /// Create new traverser with vertex ID
    pub fn new(vertex_id: &str) -> Self {
        Self {
            value: TraverserValue::Vertex(vertex_id.to_string()),
            bulk: 1,
            path: None,
            loops: None,
            sack: None,
            tags: HashMap::new(),
        }
    }

    /// Create traverser with value
    pub fn with_value(value: TraverserValue) -> Self {
        Self {
            value,
            bulk: 1,
            path: None,
            loops: None,
            sack: None,
            tags: HashMap::new(),
        }
    }

    /// Get current value
    pub fn value(&self) -> &TraverserValue {
        &self.value
    }

    /// Set value
    pub fn set_value(&mut self, value: TraverserValue) {
        self.value = value;
    }

    /// Get bulk count
    pub fn bulk(&self) -> u64 {
        self.bulk
    }

    /// Set bulk count
    pub fn set_bulk(&mut self, bulk: u64) {
        self.bulk = bulk;
    }

    /// Multiply bulk
    pub fn multiply_bulk(&mut self, factor: u64) {
        self.bulk *= factor;
    }

    /// Get path (if tracking)
    pub fn path(&self) -> Option<&Path> {
        self.path.as_ref()
    }

    /// Get mutable path
    pub fn path_mut(&mut self) -> Option<&mut Path> {
        self.path.as_mut()
    }

    /// Enable path tracking
    pub fn enable_path(&mut self) {
        if self.path.is_none() {
            self.path = Some(Path::start(self.value.clone()));
        }
    }

    /// Add to path
    pub fn extend_path(&mut self, value: TraverserValue) {
        if let Some(ref mut path) = self.path {
            path.extend(value);
        }
    }

    /// Add label to current path position
    pub fn add_path_label(&mut self, label: String) {
        if let Some(ref mut path) = self.path {
            path.add_label(label);
        }
    }

    /// Get loop state
    pub fn loops(&self) -> Option<&LoopState> {
        self.loops.as_ref()
    }

    /// Get mutable loop state
    pub fn loops_mut(&mut self) -> Option<&mut LoopState> {
        self.loops.as_mut()
    }

    /// Enable loop tracking
    pub fn enable_loops(&mut self) {
        if self.loops.is_none() {
            self.loops = Some(LoopState::new());
        }
    }

    /// Initialize a named loop
    pub fn init_loop(&mut self, name: &str) {
        self.enable_loops();
        if let Some(ref mut loops) = self.loops {
            loops.init_loop(name);
        }
    }

    /// Increment loop counter
    pub fn incr_loop(&mut self, name: &str) {
        if let Some(ref mut loops) = self.loops {
            loops.incr_loop(name);
        }
    }

    /// Get loop count
    pub fn loop_count(&self, name: &str) -> u32 {
        self.loops.as_ref().map(|l| l.loop_count(name)).unwrap_or(0)
    }

    /// Get sack
    pub fn sack(&self) -> Option<&Value> {
        self.sack.as_ref()
    }

    /// Set sack
    pub fn set_sack(&mut self, value: Value) {
        self.sack = Some(value);
    }

    /// Get tag
    pub fn tag(&self, key: &str) -> Option<&String> {
        self.tags.get(key)
    }

    /// Set tag
    pub fn set_tag(&mut self, key: String, value: String) {
        self.tags.insert(key, value);
    }

    /// Split traverser (for branching)
    pub fn split(&self) -> Self {
        Self {
            value: self.value.clone(),
            bulk: 1, // Split starts with bulk 1
            path: self.path.clone(),
            loops: self.loops.clone(),
            sack: self.sack.clone(),
            tags: self.tags.clone(),
        }
    }

    /// Clone with new value
    pub fn clone_with_value(&self, value: TraverserValue) -> Self {
        let mut new = self.clone();
        new.value = value;
        new
    }

    /// Merge with another traverser (combine bulks)
    pub fn merge(&mut self, other: &Traverser) {
        self.bulk += other.bulk;
    }
}

/// Requirements that steps can declare
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TraverserRequirement {
    /// Needs bulk tracking for optimization
    Bulk,
    /// Needs path history
    Path,
    /// Needs single-level loop counter
    SingleLoop,
    /// Needs nested loop counters
    NestedLoop,
    /// Needs step labels
    Labels,
    /// Needs sack/side-effect data
    Sack,
    /// Step is a barrier (synchronization point)
    Barrier,
    /// Step modifies graph
    Mutates,
}

/// Traverser generator for creating initial traversers
pub struct TraverserGenerator {
    requirements: Vec<TraverserRequirement>,
}

impl TraverserGenerator {
    /// Create new generator with requirements
    pub fn new(requirements: Vec<TraverserRequirement>) -> Self {
        Self { requirements }
    }

    /// Generate traverser for vertex ID
    pub fn generate(&self, vertex_id: &str) -> Traverser {
        let mut traverser = Traverser::new(vertex_id);
        self.apply_requirements(&mut traverser);
        traverser
    }

    /// Generate traverser with value
    pub fn generate_value(&self, value: TraverserValue) -> Traverser {
        let mut traverser = Traverser::with_value(value);
        self.apply_requirements(&mut traverser);
        traverser
    }

    /// Generate multiple traversers
    pub fn generate_many(&self, vertex_ids: &[String]) -> Vec<Traverser> {
        vertex_ids.iter().map(|id| self.generate(id)).collect()
    }

    /// Apply requirements to traverser
    fn apply_requirements(&self, traverser: &mut Traverser) {
        for req in &self.requirements {
            match req {
                TraverserRequirement::Path => traverser.enable_path(),
                TraverserRequirement::SingleLoop | TraverserRequirement::NestedLoop => {
                    traverser.enable_loops()
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traverser_new() {
        let t = Traverser::new("v1");
        assert!(matches!(t.value(), TraverserValue::Vertex(id) if id == "v1"));
        assert_eq!(t.bulk(), 1);
    }

    #[test]
    fn test_traverser_path() {
        let mut t = Traverser::new("v1");
        assert!(t.path().is_none());

        t.enable_path();
        assert!(t.path().is_some());
        assert_eq!(t.path().unwrap().len(), 1);

        t.extend_path(TraverserValue::Vertex("v2".to_string()));
        assert_eq!(t.path().unwrap().len(), 2);
    }

    #[test]
    fn test_traverser_loops() {
        let mut t = Traverser::new("v1");
        t.init_loop("repeat_0");
        assert_eq!(t.loop_count("repeat_0"), 0);

        t.incr_loop("repeat_0");
        assert_eq!(t.loop_count("repeat_0"), 1);

        t.incr_loop("repeat_0");
        assert_eq!(t.loop_count("repeat_0"), 2);
    }

    #[test]
    fn test_traverser_split() {
        let mut t = Traverser::new("v1");
        t.set_bulk(5);
        t.enable_path();

        let split = t.split();
        assert_eq!(split.bulk(), 1); // Split resets bulk
        assert!(split.path().is_some()); // But keeps path
    }

    #[test]
    fn test_path_labels() {
        let mut path = Path::new();
        path.extend_with_label(TraverserValue::Vertex("v1".to_string()), "a".to_string());
        path.extend_with_label(TraverserValue::Vertex("v2".to_string()), "b".to_string());

        assert!(path.has_label("a"));
        assert!(path.has_label("b"));
        assert!(!path.has_label("c"));

        let v1 = path.get("a").unwrap();
        assert!(matches!(v1, TraverserValue::Vertex(id) if id == "v1"));
    }

    #[test]
    fn test_path_retract() {
        let mut path = Path::new();
        path.extend_with_label(TraverserValue::Vertex("v1".to_string()), "a".to_string());
        path.extend(TraverserValue::Vertex("v2".to_string())); // No label
        path.extend_with_label(TraverserValue::Vertex("v3".to_string()), "b".to_string());

        path.retract(&["a".to_string(), "b".to_string()]);
        assert_eq!(path.len(), 2); // Only labeled elements kept
    }

    #[test]
    fn test_traverser_generator() {
        let gen = TraverserGenerator::new(vec![
            TraverserRequirement::Path,
            TraverserRequirement::SingleLoop,
        ]);

        let t = gen.generate("v1");
        assert!(t.path().is_some());
        assert!(t.loops().is_some());
    }
}
