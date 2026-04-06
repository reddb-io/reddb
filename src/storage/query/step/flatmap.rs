//! FlatMap Steps
//!
//! Steps that expand traversers 1:N (one input produces N outputs).
//!
//! # Steps
//!
//! - `out()`: Follow outgoing edges
//! - `in()`: Follow incoming edges
//! - `both()`: Follow both directions
//! - `outE()`: Get outgoing edges
//! - `inE()`: Get incoming edges
//! - `bothE()`: Get all adjacent edges
//! - `outV()`, `inV()`, `bothV()`, `otherV()`: Edge vertex accessors

use super::{Step, StepResult, Traverser, TraverserRequirement, TraverserValue};
use std::any::Any;

/// Trait for flatmap steps (1:N expansion)
pub trait FlatMapStep: Step {
    /// Expand a single traverser to multiple
    fn flat_map(&self, traverser: &Traverser) -> Vec<Traverser>;
}

/// Direction for edge traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Outgoing edges
    Out,
    /// Incoming edges
    In,
    /// Both directions
    Both,
}

/// Generic vertex step - traverses edges to adjacent vertices
#[derive(Debug, Clone)]
pub struct VertexStep {
    id: String,
    labels: Vec<String>,
    /// Direction to traverse
    direction: Direction,
    /// Edge labels to follow (empty = all)
    edge_labels: Vec<String>,
    /// Return edges instead of vertices
    return_edges: bool,
}

impl VertexStep {
    /// Create out() step
    pub fn out(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("out_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Out,
            edge_labels,
            return_edges: false,
        }
    }

    /// Create in() step
    pub fn in_(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("in_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::In,
            edge_labels,
            return_edges: false,
        }
    }

    /// Create both() step
    pub fn both(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("both_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Both,
            edge_labels,
            return_edges: false,
        }
    }

    /// Create outE() step
    pub fn out_e(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("outE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Out,
            edge_labels,
            return_edges: true,
        }
    }

    /// Create inE() step
    pub fn in_e(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("inE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::In,
            edge_labels,
            return_edges: true,
        }
    }

    /// Create bothE() step
    pub fn both_e(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("bothE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Both,
            edge_labels,
            return_edges: true,
        }
    }

    /// Get direction
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Get edge labels
    pub fn edge_labels(&self) -> &[String] {
        &self.edge_labels
    }

    /// Check if returning edges
    pub fn returns_edges(&self) -> bool {
        self.return_edges
    }

    /// Set step ID
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }
}

impl Step for VertexStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        match (self.direction, self.return_edges) {
            (Direction::Out, false) => "OutStep",
            (Direction::In, false) => "InStep",
            (Direction::Both, false) => "BothStep",
            (Direction::Out, true) => "OutEStep",
            (Direction::In, true) => "InEStep",
            (Direction::Both, true) => "BothEStep",
        }
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
        let new_traversers = self.flat_map(&traverser);
        StepResult::emit_many(new_traversers)
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

impl FlatMapStep for VertexStep {
    fn flat_map(&self, traverser: &Traverser) -> Vec<Traverser> {
        // In real implementation, this would query the graph store
        // For now, return empty - execution engine will populate
        Vec::new()
    }
}

/// Out step - convenience alias
pub type OutStep = VertexStep;

/// In step - convenience alias
pub type InStep = VertexStep;

/// Both step - convenience alias
pub type BothStep = VertexStep;

/// Edge step - get edges from current element
#[derive(Debug, Clone)]
pub struct EdgeStep {
    id: String,
    labels: Vec<String>,
    /// Direction
    direction: Direction,
    /// Edge labels
    edge_labels: Vec<String>,
}

impl EdgeStep {
    /// Create outE() step
    pub fn out(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("outE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Out,
            edge_labels,
        }
    }

    /// Create inE() step
    pub fn in_(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("inE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::In,
            edge_labels,
        }
    }

    /// Create bothE() step
    pub fn both(edge_labels: Vec<String>) -> Self {
        Self {
            id: format!("bothE_{}", edge_labels.join("_")),
            labels: Vec::new(),
            direction: Direction::Both,
            edge_labels,
        }
    }
}

impl Step for EdgeStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        match self.direction {
            Direction::Out => "OutEStep",
            Direction::In => "InEStep",
            Direction::Both => "BothEStep",
        }
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
        let new_traversers = self.flat_map(&traverser);
        StepResult::emit_many(new_traversers)
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

impl FlatMapStep for EdgeStep {
    fn flat_map(&self, _traverser: &Traverser) -> Vec<Traverser> {
        // Would query graph store for edges
        Vec::new()
    }
}

/// Edge vertex accessor - gets vertex from edge
#[derive(Debug, Clone)]
pub struct EdgeVertexStep {
    id: String,
    labels: Vec<String>,
    /// Which vertex to get
    direction: Direction,
}

impl EdgeVertexStep {
    /// Create outV() step - get target vertex
    pub fn out_v() -> Self {
        Self {
            id: "outV_0".to_string(),
            labels: Vec::new(),
            direction: Direction::Out,
        }
    }

    /// Create inV() step - get source vertex
    pub fn in_v() -> Self {
        Self {
            id: "inV_0".to_string(),
            labels: Vec::new(),
            direction: Direction::In,
        }
    }

    /// Create bothV() step - get both vertices
    pub fn both_v() -> Self {
        Self {
            id: "bothV_0".to_string(),
            labels: Vec::new(),
            direction: Direction::Both,
        }
    }

    /// Create otherV() step - get opposite vertex from traversal
    pub fn other_v() -> Self {
        // otherV is context-dependent - needs path info
        Self {
            id: "otherV_0".to_string(),
            labels: Vec::new(),
            direction: Direction::Both, // Direction determined at runtime
        }
    }
}

impl Step for EdgeVertexStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        match self.direction {
            Direction::Out => "OutVStep",
            Direction::In => "InVStep",
            Direction::Both => "BothVStep",
        }
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
        // Get edge and extract vertex
        if let TraverserValue::Edge {
            id: _,
            source,
            target,
            label: _,
        } = traverser.value()
        {
            let new_traversers = match self.direction {
                Direction::Out => {
                    vec![traverser.clone_with_value(TraverserValue::Vertex(target.clone()))]
                }
                Direction::In => {
                    vec![traverser.clone_with_value(TraverserValue::Vertex(source.clone()))]
                }
                Direction::Both => vec![
                    traverser.clone_with_value(TraverserValue::Vertex(source.clone())),
                    traverser.clone_with_value(TraverserValue::Vertex(target.clone())),
                ],
            };
            StepResult::emit_many(new_traversers)
        } else {
            StepResult::Filter
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

impl FlatMapStep for EdgeVertexStep {
    fn flat_map(&self, traverser: &Traverser) -> Vec<Traverser> {
        // Delegate to process_traverser
        match self.process_traverser(traverser.clone()) {
            StepResult::Emit(t) => t,
            _ => Vec::new(),
        }
    }
}

/// Properties step - get all properties as map
#[derive(Debug, Clone)]
pub struct PropertiesStep {
    id: String,
    labels: Vec<String>,
    /// Property keys to get (empty = all)
    keys: Vec<String>,
}

impl PropertiesStep {
    /// Create properties() step
    pub fn new() -> Self {
        Self {
            id: "properties_0".to_string(),
            labels: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// Create properties(keys) step
    pub fn with_keys(keys: Vec<String>) -> Self {
        Self {
            id: format!("properties_{}", keys.join("_")),
            labels: Vec::new(),
            keys,
        }
    }
}

impl Default for PropertiesStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for PropertiesStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "PropertiesStep"
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
        let new_traversers = self.flat_map(&traverser);
        StepResult::emit_many(new_traversers)
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

impl FlatMapStep for PropertiesStep {
    fn flat_map(&self, traverser: &Traverser) -> Vec<Traverser> {
        // Would extract properties from vertex/edge
        // For now, return single property traverser per key
        if let TraverserValue::Map(map) = traverser.value() {
            let mut result = Vec::new();
            for (key, value) in map {
                if self.keys.is_empty() || self.keys.contains(key) {
                    result.push(
                        traverser
                            .clone_with_value(TraverserValue::Property(key.clone(), value.clone())),
                    );
                }
            }
            result
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_out_step() {
        let step = VertexStep::out(vec!["knows".to_string()]);
        assert_eq!(step.direction(), Direction::Out);
        assert_eq!(step.edge_labels(), &["knows"]);
        assert!(!step.returns_edges());
    }

    #[test]
    fn test_in_step() {
        let step = VertexStep::in_(vec![]);
        assert_eq!(step.direction(), Direction::In);
        assert!(step.edge_labels().is_empty());
    }

    #[test]
    fn test_both_step() {
        let step = VertexStep::both(vec!["connects".to_string(), "knows".to_string()]);
        assert_eq!(step.direction(), Direction::Both);
        assert_eq!(step.edge_labels().len(), 2);
    }

    #[test]
    fn test_out_e_step() {
        let step = VertexStep::out_e(vec![]);
        assert_eq!(step.direction(), Direction::Out);
        assert!(step.returns_edges());
    }

    #[test]
    fn test_edge_vertex_step() {
        let edge = TraverserValue::Edge {
            id: "e1".to_string(),
            source: "v1".to_string(),
            target: "v2".to_string(),
            label: "knows".to_string(),
        };
        let traverser = Traverser::with_value(edge);

        // outV should return target
        let out_v = EdgeVertexStep::out_v();
        let result = out_v.process_traverser(traverser.clone());
        if let StepResult::Emit(t) = result {
            assert_eq!(t.len(), 1);
            assert!(matches!(t[0].value(), TraverserValue::Vertex(id) if id == "v2"));
        }

        // inV should return source
        let in_v = EdgeVertexStep::in_v();
        let result = in_v.process_traverser(traverser.clone());
        if let StepResult::Emit(t) = result {
            assert_eq!(t.len(), 1);
            assert!(matches!(t[0].value(), TraverserValue::Vertex(id) if id == "v1"));
        }

        // bothV should return both
        let both_v = EdgeVertexStep::both_v();
        let result = both_v.process_traverser(traverser);
        if let StepResult::Emit(t) = result {
            assert_eq!(t.len(), 2);
        }
    }

    #[test]
    fn test_properties_step() {
        let step = PropertiesStep::with_keys(vec!["name".to_string(), "age".to_string()]);
        assert_eq!(step.name(), "PropertiesStep");
    }
}
