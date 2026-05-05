//! Source Steps
//!
//! Steps that initiate traversals by reading from graph data sources.
//!
//! # Steps
//!
//! - `V()`: Start with vertices
//! - `E()`: Start with edges
//! - `addV()`: Create vertex
//! - `addE()`: Create edge

use super::{Step, StepResult, Traverser, TraverserRequirement, TraverserValue};
use std::any::Any;

/// Trait for source steps (traversal starters)
pub trait SourceStep: Step {
    /// Generate initial traversers
    fn generate_traversers(&self) -> Vec<Traverser>;
}

/// Vertex source step - V()
#[derive(Debug, Clone)]
pub struct VertexSourceStep {
    id: String,
    labels: Vec<String>,
    /// Specific vertex IDs to start from (if empty, all vertices)
    vertex_ids: Vec<String>,
    /// Vertex type filter
    vertex_type: Option<String>,
}

impl VertexSourceStep {
    /// Create V() step for all vertices
    pub fn new() -> Self {
        Self {
            id: "V_0".to_string(),
            labels: Vec::new(),
            vertex_ids: Vec::new(),
            vertex_type: None,
        }
    }

    /// Create V(id) step for specific vertex
    pub fn with_ids(ids: Vec<String>) -> Self {
        Self {
            id: format!("V_{}", ids.first().unwrap_or(&"0".to_string())),
            labels: Vec::new(),
            vertex_ids: ids,
            vertex_type: None,
        }
    }

    /// Create V().hasLabel(type) step
    pub fn with_type(vertex_type: String) -> Self {
        Self {
            id: format!("V_{}", vertex_type),
            labels: Vec::new(),
            vertex_ids: Vec::new(),
            vertex_type: Some(vertex_type),
        }
    }

    /// Set step ID
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Get vertex IDs filter
    pub fn vertex_ids(&self) -> &[String] {
        &self.vertex_ids
    }

    /// Get vertex type filter
    pub fn vertex_type(&self) -> Option<&str> {
        self.vertex_type.as_deref()
    }
}

impl Default for VertexSourceStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for VertexSourceStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "VertexSourceStep"
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
        &[] // Source steps have no special requirements
    }

    fn process_traverser(&self, _traverser: Traverser) -> StepResult {
        // Source steps don't process traversers - they generate them
        // This is called via generate_traversers()
        StepResult::Filter
    }

    fn reset(&mut self) {
        // No state to reset
    }

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

impl SourceStep for VertexSourceStep {
    fn generate_traversers(&self) -> Vec<Traverser> {
        // In real implementation, this would query the graph store
        // For now, generate traversers for specified IDs
        self.vertex_ids
            .iter()
            .map(|id| Traverser::new(id))
            .collect()
    }
}

/// Edge source step - E()
#[derive(Debug, Clone)]
pub struct EdgeSourceStep {
    id: String,
    labels: Vec<String>,
    /// Specific edge IDs to start from (if empty, all edges)
    edge_ids: Vec<String>,
    /// Edge type filter
    edge_type: Option<String>,
}

impl EdgeSourceStep {
    /// Create E() step for all edges
    pub fn new() -> Self {
        Self {
            id: "E_0".to_string(),
            labels: Vec::new(),
            edge_ids: Vec::new(),
            edge_type: None,
        }
    }

    /// Create E(id) step for specific edge
    pub fn with_ids(ids: Vec<String>) -> Self {
        Self {
            id: format!("E_{}", ids.first().unwrap_or(&"0".to_string())),
            labels: Vec::new(),
            edge_ids: ids,
            edge_type: None,
        }
    }

    /// Create E().hasLabel(type) step
    pub fn with_type(edge_type: String) -> Self {
        Self {
            id: format!("E_{}", edge_type),
            labels: Vec::new(),
            edge_ids: Vec::new(),
            edge_type: Some(edge_type),
        }
    }

    /// Set step ID
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Get edge IDs filter
    pub fn edge_ids(&self) -> &[String] {
        &self.edge_ids
    }

    /// Get edge type filter
    pub fn edge_type(&self) -> Option<&str> {
        self.edge_type.as_deref()
    }
}

impl Default for EdgeSourceStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for EdgeSourceStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "EdgeSourceStep"
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

    fn process_traverser(&self, _traverser: Traverser) -> StepResult {
        StepResult::Filter
    }

    fn reset(&mut self) {
        // No state to reset
    }

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

impl SourceStep for EdgeSourceStep {
    fn generate_traversers(&self) -> Vec<Traverser> {
        // Generate edge traversers
        self.edge_ids
            .iter()
            .map(|id| {
                Traverser::with_value(TraverserValue::Edge {
                    id: id.clone(),
                    source: String::new(), // Would be filled by graph store
                    target: String::new(),
                    label: self.edge_type.clone().unwrap_or_default(),
                })
            })
            .collect()
    }
}

/// Inject step - injects arbitrary values into traversal
#[derive(Debug, Clone)]
pub struct InjectStep {
    id: String,
    labels: Vec<String>,
    values: Vec<TraverserValue>,
}

impl InjectStep {
    /// Create inject step with values
    pub fn new(values: Vec<TraverserValue>) -> Self {
        Self {
            id: "inject_0".to_string(),
            labels: Vec::new(),
            values,
        }
    }

    /// Set step ID
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }
}

impl Step for InjectStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "InjectStep"
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
        // Pass through existing traverser plus inject new values
        let mut result = vec![traverser];
        for value in &self.values {
            result.push(Traverser::with_value(value.clone()));
        }
        StepResult::Emit(result)
    }

    fn reset(&mut self) {
        // No state to reset
    }

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

impl SourceStep for InjectStep {
    fn generate_traversers(&self) -> Vec<Traverser> {
        self.values
            .iter()
            .map(|v| Traverser::with_value(v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex_source_all() {
        let step = VertexSourceStep::new();
        assert_eq!(step.name(), "VertexSourceStep");
        assert!(step.vertex_ids().is_empty());
        assert!(step.vertex_type().is_none());
    }

    #[test]
    fn test_vertex_source_with_ids() {
        let step = VertexSourceStep::with_ids(vec!["v1".to_string(), "v2".to_string()]);
        assert_eq!(step.vertex_ids().len(), 2);

        let traversers = step.generate_traversers();
        assert_eq!(traversers.len(), 2);
    }

    #[test]
    fn test_vertex_source_with_type() {
        let step = VertexSourceStep::with_type("Host".to_string());
        assert_eq!(step.vertex_type(), Some("Host"));
    }

    #[test]
    fn test_edge_source() {
        let step = EdgeSourceStep::new();
        assert_eq!(step.name(), "EdgeSourceStep");
    }

    #[test]
    fn test_inject_step() {
        let step = InjectStep::new(vec![
            TraverserValue::String("hello".to_string()),
            TraverserValue::Integer(42),
        ]);

        let traversers = step.generate_traversers();
        assert_eq!(traversers.len(), 2);
    }

    #[test]
    fn test_step_labels() {
        let mut step = VertexSourceStep::new();
        step.add_label("a".to_string());
        step.add_label("b".to_string());
        assert_eq!(step.labels().len(), 2);

        // Adding duplicate should not increase count
        step.add_label("a".to_string());
        assert_eq!(step.labels().len(), 2);
    }
}
