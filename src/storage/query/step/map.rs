//! Map Steps
//!
//! Steps that transform traversers 1:1 (one input produces one output).
//!
//! # Steps
//!
//! - `map()`: Apply transformation function
//! - `select()`: Select labeled values
//! - `project()`: Project specific properties
//! - `path()`: Get traversal path
//! - `valueMap()`: Get property map
//! - `id()`: Get element ID

use super::{Path, Step, StepResult, Traverser, TraverserRequirement, TraverserValue};
use crate::json;
use crate::serde_json::Value;
use std::any::Any;
use std::collections::HashMap;

/// Trait for map steps (1:1 transformation)
pub trait MapStep: Step {
    /// Map a traverser to a new value
    fn map(&self, traverser: &Traverser) -> TraverserValue;
}

/// Select step - selects labeled values from path
#[derive(Debug, Clone)]
pub struct SelectStep {
    id: String,
    labels: Vec<String>,
    /// Labels to select
    select_labels: Vec<String>,
    /// Pop behavior (first, last, all, mixed)
    pop: Pop,
}

/// Pop behavior for select
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pop {
    /// First occurrence
    First,
    /// Last occurrence
    Last,
    /// All occurrences
    All,
    /// Mixed (per-label)
    Mixed,
}

impl SelectStep {
    /// Create select() step with labels
    pub fn new(select_labels: Vec<String>) -> Self {
        Self {
            id: format!("select_{}", select_labels.join("_")),
            labels: Vec::new(),
            select_labels,
            pop: Pop::Last,
        }
    }

    /// Create select with pop behavior
    pub fn with_pop(select_labels: Vec<String>, pop: Pop) -> Self {
        Self {
            id: format!("select_{:?}_{}", pop, select_labels.join("_")),
            labels: Vec::new(),
            select_labels,
            pop,
        }
    }

    /// Get selected labels
    pub fn select_labels(&self) -> &[String] {
        &self.select_labels
    }
}

impl Step for SelectStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "SelectStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        static REQS: &[TraverserRequirement] =
            &[TraverserRequirement::Path, TraverserRequirement::Labels];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        if new_value.is_null() {
            StepResult::Filter
        } else {
            StepResult::emit_one(traverser.clone_with_value(new_value))
        }
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for SelectStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        if let Some(path) = traverser.path() {
            if self.select_labels.len() == 1 {
                // Single label - return value directly
                let label = &self.select_labels[0];
                match self.pop {
                    Pop::Last => path.get(label).cloned().unwrap_or(TraverserValue::Null),
                    Pop::First => {
                        let all = path.get_all(label);
                        all.first()
                            .cloned()
                            .cloned()
                            .unwrap_or(TraverserValue::Null)
                    }
                    Pop::All => {
                        let all: Vec<Value> = path
                            .get_all(label)
                            .into_iter()
                            .map(|v| v.to_json())
                            .collect();
                        TraverserValue::List(all)
                    }
                    Pop::Mixed => path.get(label).cloned().unwrap_or(TraverserValue::Null),
                }
            } else {
                // Multiple labels - return map
                let mut map = HashMap::new();
                for label in &self.select_labels {
                    if let Some(value) = path.get(label) {
                        map.insert(label.clone(), value.to_json());
                    }
                }
                TraverserValue::Map(map)
            }
        } else {
            TraverserValue::Null
        }
    }
}

/// Project step - projects specific properties
#[derive(Debug, Clone)]
pub struct ProjectStep {
    id: String,
    labels: Vec<String>,
    /// Keys to project
    keys: Vec<String>,
    // In real impl, would have by-modulator traversals
}

impl ProjectStep {
    /// Create project() step
    pub fn new(keys: Vec<String>) -> Self {
        Self {
            id: format!("project_{}", keys.join("_")),
            labels: Vec::new(),
            keys,
        }
    }

    /// Get projection keys
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
}

impl Step for ProjectStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "ProjectStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        StepResult::emit_one(traverser.clone_with_value(new_value))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for ProjectStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        let mut result = HashMap::new();

        // For each key, apply the corresponding by-modulator
        // In real impl, each key would have a child traversal
        if let TraverserValue::Map(current) = traverser.value() {
            for key in &self.keys {
                if let Some(value) = current.get(key) {
                    result.insert(key.clone(), value.clone());
                } else {
                    result.insert(key.clone(), Value::Null);
                }
            }
        }

        TraverserValue::Map(result)
    }
}

/// Path step - returns the traversal path
#[derive(Debug, Clone)]
pub struct PathStep {
    id: String,
    labels: Vec<String>,
}

impl PathStep {
    /// Create path() step
    pub fn new() -> Self {
        Self {
            id: "path_0".to_string(),
            labels: Vec::new(),
        }
    }
}

impl Default for PathStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for PathStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "PathStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Path];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        StepResult::emit_one(traverser.clone_with_value(new_value))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for PathStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        if let Some(path) = traverser.path() {
            TraverserValue::Path(path.clone())
        } else {
            TraverserValue::Path(Path::new())
        }
    }
}

/// ValueMap step - returns property map
#[derive(Debug, Clone)]
pub struct ValueMapStep {
    id: String,
    labels: Vec<String>,
    /// Property keys to include (empty = all)
    keys: Vec<String>,
    /// Include element type and id
    with_tokens: bool,
}

impl ValueMapStep {
    /// Create valueMap() step
    pub fn new() -> Self {
        Self {
            id: "valueMap_0".to_string(),
            labels: Vec::new(),
            keys: Vec::new(),
            with_tokens: false,
        }
    }

    /// Create valueMap(keys) step
    pub fn with_keys(keys: Vec<String>) -> Self {
        Self {
            id: format!("valueMap_{}", keys.join("_")),
            labels: Vec::new(),
            keys,
            with_tokens: false,
        }
    }

    /// Include T.id and T.label
    pub fn with_tokens(mut self) -> Self {
        self.with_tokens = true;
        self
    }
}

impl Default for ValueMapStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for ValueMapStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "ValueMapStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        StepResult::emit_one(traverser.clone_with_value(new_value))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for ValueMapStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        let mut result = HashMap::new();

        match traverser.value() {
            TraverserValue::Vertex(id) => {
                if self.with_tokens {
                    result.insert("@id".to_string(), json!(id));
                    result.insert("@type".to_string(), json!("vertex"));
                }
            }
            TraverserValue::Edge {
                id,
                source,
                target,
                label,
            } => {
                if self.with_tokens {
                    result.insert("@id".to_string(), json!(id));
                    result.insert("@type".to_string(), json!("edge"));
                    result.insert("@label".to_string(), json!(label));
                    result.insert("@source".to_string(), json!(source));
                    result.insert("@target".to_string(), json!(target));
                }
            }
            TraverserValue::Map(map) => {
                for (key, value) in map {
                    if self.keys.is_empty() || self.keys.contains(key) {
                        result.insert(key.clone(), value.clone());
                    }
                }
            }
            _ => {}
        }

        TraverserValue::Map(result)
    }
}

/// Id step - returns element ID
#[derive(Debug, Clone)]
pub struct IdStep {
    id: String,
    labels: Vec<String>,
}

impl IdStep {
    /// Create id() step
    pub fn new() -> Self {
        Self {
            id: "id_0".to_string(),
            labels: Vec::new(),
        }
    }
}

impl Default for IdStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for IdStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "IdStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        StepResult::emit_one(traverser.clone_with_value(new_value))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for IdStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        match traverser.value() {
            TraverserValue::Vertex(id) => TraverserValue::String(id.clone()),
            TraverserValue::Edge { id, .. } => TraverserValue::String(id.clone()),
            _ => TraverserValue::Null,
        }
    }
}

/// Label step - returns element label
#[derive(Debug, Clone)]
pub struct LabelStep {
    id: String,
    labels: Vec<String>,
}

impl LabelStep {
    /// Create label() step
    pub fn new() -> Self {
        Self {
            id: "label_0".to_string(),
            labels: Vec::new(),
        }
    }
}

impl Default for LabelStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for LabelStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "LabelStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        let new_value = self.map(&traverser);
        StepResult::emit_one(traverser.clone_with_value(new_value))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for LabelStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        match traverser.value() {
            TraverserValue::Edge { label, .. } => TraverserValue::String(label.clone()),
            // Vertices would need type lookup
            _ => TraverserValue::Null,
        }
    }
}

/// Count step - counts traversers (reduce)
#[derive(Debug, Clone)]
pub struct CountStep {
    id: String,
    labels: Vec<String>,
}

impl CountStep {
    /// Create count() step
    pub fn new() -> Self {
        Self {
            id: "count_0".to_string(),
            labels: Vec::new(),
        }
    }
}

impl Default for CountStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for CountStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "CountStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        static REQS: &[TraverserRequirement] =
            &[TraverserRequirement::Barrier, TraverserRequirement::Bulk];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // Count is a barrier - would accumulate and then emit single count
        // For single traverser, count its bulk
        let count = traverser.bulk();
        StepResult::emit_one(traverser.clone_with_value(TraverserValue::Integer(count as i64)))
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl MapStep for CountStep {
    fn map(&self, traverser: &Traverser) -> TraverserValue {
        TraverserValue::Integer(traverser.bulk() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_single() {
        let step = SelectStep::new(vec!["a".to_string()]);
        assert_eq!(step.select_labels(), &["a"]);
        assert_eq!(step.name(), "SelectStep");
    }

    #[test]
    fn test_select_multiple() {
        let step = SelectStep::new(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(step.select_labels().len(), 2);
    }

    #[test]
    fn test_project_step() {
        let step = ProjectStep::new(vec!["name".to_string(), "age".to_string()]);
        assert_eq!(step.keys().len(), 2);
    }

    #[test]
    fn test_path_step() {
        let step = PathStep::new();
        assert_eq!(step.name(), "PathStep");

        let mut traverser = Traverser::new("v1");
        traverser.enable_path();

        let result = step.map(&traverser);
        assert!(matches!(result, TraverserValue::Path(_)));
    }

    #[test]
    fn test_value_map_step() {
        let step = ValueMapStep::new().with_tokens();
        assert_eq!(step.name(), "ValueMapStep");

        let traverser = Traverser::new("v1");
        let result = step.map(&traverser);

        if let TraverserValue::Map(map) = result {
            assert!(map.contains_key("@id"));
            assert!(map.contains_key("@type"));
        }
    }

    #[test]
    fn test_id_step() {
        let step = IdStep::new();

        let traverser = Traverser::new("vertex123");
        let result = step.map(&traverser);
        assert!(matches!(result, TraverserValue::String(id) if id == "vertex123"));
    }

    #[test]
    fn test_label_step() {
        let step = LabelStep::new();

        let edge = TraverserValue::Edge {
            id: "e1".to_string(),
            source: "v1".to_string(),
            target: "v2".to_string(),
            label: "knows".to_string(),
        };
        let traverser = Traverser::with_value(edge);
        let result = step.map(&traverser);
        assert!(matches!(result, TraverserValue::String(l) if l == "knows"));
    }

    #[test]
    fn test_count_step() {
        let step = CountStep::new();

        let mut traverser = Traverser::new("v1");
        traverser.set_bulk(5);

        let result = step.map(&traverser);
        assert!(matches!(result, TraverserValue::Integer(5)));
    }
}
