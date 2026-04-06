//! Barrier Steps
//!
//! Steps that synchronize traversers before emitting results.
//! Barriers require all upstream traversers to complete before processing.
//!
//! # Steps
//!
//! - `fold()`: Collect all into list
//! - `group()`: Group by key
//! - `groupCount()`: Count by key
//! - `order()`: Sort traversers
//! - `sum()`, `max()`, `min()`, `mean()`: Aggregations

use super::{Step, StepResult, Traverser, TraverserRequirement, TraverserValue};
use crate::json;
use crate::serde_json::Value;
use std::any::Any;
use std::collections::HashMap;

/// Trait for barrier steps (synchronization points)
pub trait BarrierStep: Step {
    /// Add traverser to barrier
    fn add_to_barrier(&mut self, traverser: Traverser);

    /// Flush barrier and produce results
    fn flush_barrier(&mut self) -> Vec<Traverser>;

    /// Check if barrier has accumulated data
    fn has_data(&self) -> bool;

    /// Check if barrier is ready to flush
    fn is_ready(&self) -> bool;
}

/// Generic reducing barrier step
#[derive(Debug, Clone)]
pub struct ReducingBarrierStep<T: Clone + Send + Sync + std::fmt::Debug> {
    id: String,
    labels: Vec<String>,
    /// Accumulated value
    seed: T,
    /// Current accumulated value
    accumulator: Option<T>,
    /// Reduction function name
    reducer_name: String,
}

/// Fold step - collects all traversers into a list
#[derive(Debug, Clone)]
pub struct FoldStep {
    id: String,
    labels: Vec<String>,
    /// Accumulated values
    values: Vec<Value>,
}

impl FoldStep {
    /// Create fold() step
    pub fn new() -> Self {
        Self {
            id: "fold_0".to_string(),
            labels: Vec::new(),
            values: Vec::new(),
        }
    }
}

impl Default for FoldStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for FoldStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "FoldStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Barrier];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // Barrier steps hold traversers
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.values.clear();
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

impl BarrierStep for FoldStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        // Add value based on bulk
        for _ in 0..traverser.bulk() {
            self.values.push(traverser.value().to_json());
        }
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        if self.values.is_empty() {
            return vec![Traverser::with_value(TraverserValue::List(vec![]))];
        }

        let result = std::mem::take(&mut self.values);
        vec![Traverser::with_value(TraverserValue::List(result))]
    }

    fn has_data(&self) -> bool {
        !self.values.is_empty()
    }

    fn is_ready(&self) -> bool {
        true // Always ready - needs external signal
    }
}

/// Collecting barrier step - collects into various containers
#[derive(Debug, Clone)]
pub struct CollectingBarrierStep {
    id: String,
    labels: Vec<String>,
    /// Collected values
    values: Vec<Value>,
    /// Container type
    container: ContainerType,
}

/// Container types for collecting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerType {
    /// Standard list
    List,
    /// Set (unique values)
    Set,
    /// Bulk map (value -> count)
    BulkMap,
}

impl CollectingBarrierStep {
    /// Create list collector
    pub fn to_list() -> Self {
        Self {
            id: "toList_0".to_string(),
            labels: Vec::new(),
            values: Vec::new(),
            container: ContainerType::List,
        }
    }

    /// Create set collector
    pub fn to_set() -> Self {
        Self {
            id: "toSet_0".to_string(),
            labels: Vec::new(),
            values: Vec::new(),
            container: ContainerType::Set,
        }
    }

    /// Create bulk map collector
    pub fn to_bulk_map() -> Self {
        Self {
            id: "toBulkSet_0".to_string(),
            labels: Vec::new(),
            values: Vec::new(),
            container: ContainerType::BulkMap,
        }
    }
}

impl Step for CollectingBarrierStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        match self.container {
            ContainerType::List => "ToListStep",
            ContainerType::Set => "ToSetStep",
            ContainerType::BulkMap => "ToBulkSetStep",
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Barrier];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.values.clear();
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

impl BarrierStep for CollectingBarrierStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        let value = traverser.value().to_json();

        match self.container {
            ContainerType::List => {
                for _ in 0..traverser.bulk() {
                    self.values.push(value.clone());
                }
            }
            ContainerType::Set => {
                if !self.values.contains(&value) {
                    self.values.push(value);
                }
            }
            ContainerType::BulkMap => {
                // Would use map instead of vec
                self.values.push(value);
            }
        }
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        let result = std::mem::take(&mut self.values);
        vec![Traverser::with_value(TraverserValue::List(result))]
    }

    fn has_data(&self) -> bool {
        !self.values.is_empty()
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// Group step - groups by key
#[derive(Debug, Clone)]
pub struct GroupStep {
    id: String,
    labels: Vec<String>,
    /// Group storage: key -> values
    groups: HashMap<String, Vec<Value>>,
    /// Current state: 'k' for key, 'v' for value
    state: char,
}

impl GroupStep {
    /// Create group() step
    pub fn new() -> Self {
        Self {
            id: "group_0".to_string(),
            labels: Vec::new(),
            groups: HashMap::new(),
            state: 'k',
        }
    }
}

impl Default for GroupStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for GroupStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "GroupStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Barrier];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.groups.clear();
        self.state = 'k';
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

impl BarrierStep for GroupStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        // In real impl, would use key/value child traversals
        // For now, use value.to_string() as key
        let key = match traverser.value() {
            TraverserValue::String(s) => s.clone(),
            TraverserValue::Vertex(id) => id.clone(),
            other => format!("{:?}", other),
        };

        self.groups
            .entry(key)
            .or_insert_with(Vec::new)
            .push(traverser.value().to_json());
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        let result: HashMap<String, Value> = self
            .groups
            .drain()
            .map(|(k, v)| (k, Value::Array(v)))
            .collect();

        vec![Traverser::with_value(TraverserValue::Map(result))]
    }

    fn has_data(&self) -> bool {
        !self.groups.is_empty()
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// GroupCount step - counts by key
#[derive(Debug, Clone)]
pub struct GroupCountStep {
    id: String,
    labels: Vec<String>,
    /// Count storage: key -> count
    counts: HashMap<String, u64>,
}

impl GroupCountStep {
    /// Create groupCount() step
    pub fn new() -> Self {
        Self {
            id: "groupCount_0".to_string(),
            labels: Vec::new(),
            counts: HashMap::new(),
        }
    }
}

impl Default for GroupCountStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for GroupCountStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "GroupCountStep"
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
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.counts.clear();
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

impl BarrierStep for GroupCountStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        let key = match traverser.value() {
            TraverserValue::String(s) => s.clone(),
            TraverserValue::Vertex(id) => id.clone(),
            other => format!("{:?}", other),
        };

        *self.counts.entry(key).or_insert(0) += traverser.bulk();
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        let result: HashMap<String, Value> =
            self.counts.drain().map(|(k, v)| (k, json!(v))).collect();

        vec![Traverser::with_value(TraverserValue::Map(result))]
    }

    fn has_data(&self) -> bool {
        !self.counts.is_empty()
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// Order step - sorts traversers
#[derive(Debug, Clone)]
pub struct OrderStep {
    id: String,
    labels: Vec<String>,
    /// Collected traversers for sorting
    traversers: Vec<Traverser>,
    /// Sort direction
    ascending: bool,
}

impl OrderStep {
    /// Create order() step
    pub fn new() -> Self {
        Self {
            id: "order_0".to_string(),
            labels: Vec::new(),
            traversers: Vec::new(),
            ascending: true,
        }
    }

    /// Create descending order
    pub fn descending() -> Self {
        Self {
            id: "order_desc_0".to_string(),
            labels: Vec::new(),
            traversers: Vec::new(),
            ascending: false,
        }
    }
}

impl Default for OrderStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for OrderStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "OrderStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Barrier];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.traversers.clear();
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

impl BarrierStep for OrderStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        self.traversers.push(traverser);
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        // Sort by value string representation
        let ascending = self.ascending;
        self.traversers.sort_by(|a, b| {
            let a_str = format!("{:?}", a.value());
            let b_str = format!("{:?}", b.value());
            if ascending {
                a_str.cmp(&b_str)
            } else {
                b_str.cmp(&a_str)
            }
        });

        std::mem::take(&mut self.traversers)
    }

    fn has_data(&self) -> bool {
        !self.traversers.is_empty()
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// Sum step - sums numeric values
#[derive(Debug, Clone)]
pub struct SumStep {
    id: String,
    labels: Vec<String>,
    sum: f64,
}

impl SumStep {
    /// Create sum() step
    pub fn new() -> Self {
        Self {
            id: "sum_0".to_string(),
            labels: Vec::new(),
            sum: 0.0,
        }
    }
}

impl Default for SumStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for SumStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "SumStep"
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
        StepResult::Hold(vec![traverser])
    }

    fn reset(&mut self) {
        self.sum = 0.0;
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

impl BarrierStep for SumStep {
    fn add_to_barrier(&mut self, traverser: Traverser) {
        let value = match traverser.value() {
            TraverserValue::Integer(i) => *i as f64,
            TraverserValue::Float(f) => *f,
            _ => 0.0,
        };
        self.sum += value * traverser.bulk() as f64;
    }

    fn flush_barrier(&mut self) -> Vec<Traverser> {
        let result = self.sum;
        self.sum = 0.0;
        vec![Traverser::with_value(TraverserValue::Float(result))]
    }

    fn has_data(&self) -> bool {
        self.sum != 0.0
    }

    fn is_ready(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fold_step() {
        let mut step = FoldStep::new();

        step.add_to_barrier(Traverser::new("v1"));
        step.add_to_barrier(Traverser::new("v2"));
        step.add_to_barrier(Traverser::new("v3"));

        let result = step.flush_barrier();
        assert_eq!(result.len(), 1);

        if let TraverserValue::List(list) = result[0].value() {
            assert_eq!(list.len(), 3);
        } else {
            panic!("Expected list");
        }
    }

    #[test]
    fn test_fold_with_bulk() {
        let mut step = FoldStep::new();

        let mut t = Traverser::new("v1");
        t.set_bulk(3);
        step.add_to_barrier(t);

        let result = step.flush_barrier();
        if let TraverserValue::List(list) = result[0].value() {
            assert_eq!(list.len(), 3); // Bulk expanded
        }
    }

    #[test]
    fn test_group_step() {
        let mut step = GroupStep::new();

        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "b".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));

        let result = step.flush_barrier();
        assert_eq!(result.len(), 1);

        if let TraverserValue::Map(map) = result[0].value() {
            assert_eq!(map.len(), 2); // Two groups: a and b
        }
    }

    #[test]
    fn test_group_count_step() {
        let mut step = GroupCountStep::new();

        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "b".to_string(),
        )));

        let mut t = Traverser::with_value(TraverserValue::String("a".to_string()));
        t.set_bulk(2);
        step.add_to_barrier(t);

        let result = step.flush_barrier();
        if let TraverserValue::Map(map) = result[0].value() {
            assert_eq!(map.get("a"), Some(&json!(3)));
            assert_eq!(map.get("b"), Some(&json!(1)));
        }
    }

    #[test]
    fn test_order_step() {
        let mut step = OrderStep::new();

        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "c".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "b".to_string(),
        )));

        let result = step.flush_barrier();
        assert_eq!(result.len(), 3);
        // Should be sorted ascending
    }

    #[test]
    fn test_sum_step() {
        let mut step = SumStep::new();

        step.add_to_barrier(Traverser::with_value(TraverserValue::Integer(10)));
        step.add_to_barrier(Traverser::with_value(TraverserValue::Integer(20)));
        step.add_to_barrier(Traverser::with_value(TraverserValue::Integer(30)));

        let result = step.flush_barrier();
        if let TraverserValue::Float(sum) = result[0].value() {
            assert_eq!(*sum, 60.0);
        }
    }

    #[test]
    fn test_collecting_to_set() {
        let mut step = CollectingBarrierStep::to_set();

        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "a".to_string(),
        )));
        step.add_to_barrier(Traverser::with_value(TraverserValue::String(
            "b".to_string(),
        )));

        let result = step.flush_barrier();
        if let TraverserValue::List(list) = result[0].value() {
            assert_eq!(list.len(), 2); // Duplicates removed
        }
    }
}
