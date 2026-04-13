//! Side Effect Steps
//!
//! Steps that perform side effects while passing through traversers.
//!
//! # Steps
//!
//! - `store()`: Store values in side-effect
//! - `aggregate()`: Aggregate values globally
//! - `property()`: Set property on element
//! - `sack()`: Manipulate traverser sack
//! - `profile()`: Gather profiling metrics

use super::{
    BasicTraversal, Step, StepResult, Traversal, Traverser, TraverserRequirement, TraverserValue,
};
use crate::json;
use crate::serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

fn sideeffect_read<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sideeffect_write<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Trait for side effect steps
pub trait SideEffectStep: Step {
    /// Execute side effect
    fn side_effect(&self, traverser: &Traverser);
}

/// Store step - stores values in side-effect key
#[derive(Debug)]
pub struct StoreStep {
    id: String,
    labels: Vec<String>,
    /// Side-effect key
    key: String,
    /// Stored values
    values: Arc<RwLock<Vec<Value>>>,
}

impl Clone for StoreStep {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            labels: self.labels.clone(),
            key: self.key.clone(),
            values: Arc::clone(&self.values),
        }
    }
}

impl StoreStep {
    /// Create store() step
    pub fn new(key: String) -> Self {
        Self {
            id: format!("store_{}", key),
            labels: Vec::new(),
            key,
            values: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get stored values
    pub fn values(&self) -> Vec<Value> {
        sideeffect_read(&self.values).clone()
    }
}

impl Step for StoreStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "StoreStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Sack];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        self.side_effect(&traverser);
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        sideeffect_write(&self.values).clear();
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

impl SideEffectStep for StoreStep {
    fn side_effect(&self, traverser: &Traverser) {
        let value = traverser.value().to_json();
        sideeffect_write(&self.values).push(value);
    }
}

/// Aggregate step - aggregates values globally (barrier-like)
#[derive(Debug)]
pub struct AggregateStep {
    id: String,
    labels: Vec<String>,
    /// Side-effect key
    key: String,
    /// Scope: local (per-traverser) or global
    global: bool,
    /// Aggregated values
    values: Arc<RwLock<Vec<Value>>>,
}

impl Clone for AggregateStep {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            labels: self.labels.clone(),
            key: self.key.clone(),
            global: self.global,
            values: Arc::clone(&self.values),
        }
    }
}

impl AggregateStep {
    /// Create aggregate() step (global)
    pub fn global(key: String) -> Self {
        Self {
            id: format!("aggregate_global_{}", key),
            labels: Vec::new(),
            key,
            global: true,
            values: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create aggregate() step (local)
    pub fn local(key: String) -> Self {
        Self {
            id: format!("aggregate_local_{}", key),
            labels: Vec::new(),
            key,
            global: false,
            values: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get aggregated values
    pub fn values(&self) -> Vec<Value> {
        sideeffect_read(&self.values).clone()
    }
}

impl Step for AggregateStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        if self.global {
            "AggregateGlobalStep"
        } else {
            "AggregateLocalStep"
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
        if self.global {
            static REQS: &[TraverserRequirement] =
                &[TraverserRequirement::Barrier, TraverserRequirement::Sack];
            REQS
        } else {
            static REQS: &[TraverserRequirement] = &[TraverserRequirement::Sack];
            REQS
        }
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        self.side_effect(&traverser);
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        sideeffect_write(&self.values).clear();
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

impl SideEffectStep for AggregateStep {
    fn side_effect(&self, traverser: &Traverser) {
        let value = traverser.value().to_json();
        sideeffect_write(&self.values).push(value);
    }
}

/// Property step - sets property on element
#[derive(Debug, Clone)]
pub struct PropertyStep {
    id: String,
    labels: Vec<String>,
    /// Property key
    key: String,
    /// Property value (or child traversal for value)
    value: Option<Value>,
    /// Value traversal
    value_traversal: Option<BasicTraversal>,
    /// Cardinality (single, list, set)
    cardinality: PropertyCardinality,
}

/// Property cardinality
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyCardinality {
    /// Single value (replaces)
    Single,
    /// List (appends)
    List,
    /// Set (unique)
    Set,
}

impl PropertyStep {
    /// Create property() step with value
    pub fn with_value(key: String, value: Value) -> Self {
        Self {
            id: format!("property_{}_{}", key, value),
            labels: Vec::new(),
            key,
            value: Some(value),
            value_traversal: None,
            cardinality: PropertyCardinality::Single,
        }
    }

    /// Create property() step with traversal
    pub fn with_traversal(key: String, traversal: BasicTraversal) -> Self {
        Self {
            id: format!("property_{}_traversal", key),
            labels: Vec::new(),
            key,
            value: None,
            value_traversal: Some(traversal),
            cardinality: PropertyCardinality::Single,
        }
    }

    /// Set cardinality
    pub fn cardinality(mut self, cardinality: PropertyCardinality) -> Self {
        self.cardinality = cardinality;
        self
    }

    /// Get property key
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Step for PropertyStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "PropertyStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Mutates];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would modify the element in the graph
        // For now, just pass through
        self.side_effect(&traverser);
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        if let Some(ref mut t) = self.value_traversal {
            t.reset();
        }
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

impl SideEffectStep for PropertyStep {
    fn side_effect(&self, _traverser: &Traverser) {
        // Would mutate the graph element
    }
}

/// Sack step - manipulates traverser sack
#[derive(Debug, Clone)]
pub struct SackStep {
    id: String,
    labels: Vec<String>,
    /// Sack operation
    operation: SackOperation,
}

/// Sack operation types
#[derive(Debug, Clone)]
pub enum SackOperation {
    /// Set sack value
    Set(Value),
    /// Sum into sack
    Sum,
    /// Multiply into sack
    Mult,
    /// Merge map/list
    Merge,
}

impl SackStep {
    /// Create sack() step to set value
    pub fn set(value: Value) -> Self {
        Self {
            id: "sack_set_0".to_string(),
            labels: Vec::new(),
            operation: SackOperation::Set(value),
        }
    }

    /// Create sack() step for sum
    pub fn sum() -> Self {
        Self {
            id: "sack_sum_0".to_string(),
            labels: Vec::new(),
            operation: SackOperation::Sum,
        }
    }

    /// Create sack() step for multiply
    pub fn mult() -> Self {
        Self {
            id: "sack_mult_0".to_string(),
            labels: Vec::new(),
            operation: SackOperation::Mult,
        }
    }

    /// Create sack() step for merge
    pub fn merge() -> Self {
        Self {
            id: "sack_merge_0".to_string(),
            labels: Vec::new(),
            operation: SackOperation::Merge,
        }
    }
}

impl Step for SackStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "SackStep"
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
        static REQS: &[TraverserRequirement] = &[TraverserRequirement::Sack];
        REQS
    }

    fn process_traverser(&self, mut traverser: Traverser) -> StepResult {
        match &self.operation {
            SackOperation::Set(value) => {
                traverser.set_sack(value.clone());
            }
            SackOperation::Sum => {
                if let (Some(sack), TraverserValue::Integer(i)) =
                    (traverser.sack(), traverser.value())
                {
                    if let Some(s) = sack.as_i64() {
                        traverser.set_sack(json!(s + i));
                    }
                }
            }
            SackOperation::Mult => {
                if let (Some(sack), TraverserValue::Integer(i)) =
                    (traverser.sack(), traverser.value())
                {
                    if let Some(s) = sack.as_i64() {
                        traverser.set_sack(json!(s * i));
                    }
                }
            }
            SackOperation::Merge => {
                // Would merge maps/lists
            }
        }
        StepResult::emit_one(traverser)
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

impl SideEffectStep for SackStep {
    fn side_effect(&self, _traverser: &Traverser) {
        // Side effect is handled in process_traverser
    }
}

/// Profile step - gathers execution metrics
#[derive(Debug)]
pub struct ProfileStep {
    id: String,
    labels: Vec<String>,
    /// Profile key
    key: Option<String>,
    /// Metrics storage
    metrics: Arc<RwLock<ProfileMetrics>>,
}

/// Profile metrics
#[derive(Debug, Clone, Default)]
pub struct ProfileMetrics {
    /// Step timings (step_id -> duration_ns)
    pub step_times: HashMap<String, u64>,
    /// Traverser counts
    pub traverser_count: u64,
    /// Total execution time
    pub total_time: u64,
}

impl Clone for ProfileStep {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            labels: self.labels.clone(),
            key: self.key.clone(),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

impl ProfileStep {
    /// Create profile() step
    pub fn new() -> Self {
        Self {
            id: "profile_0".to_string(),
            labels: Vec::new(),
            key: None,
            metrics: Arc::new(RwLock::new(ProfileMetrics::default())),
        }
    }

    /// Create profile() step with key
    pub fn with_key(key: String) -> Self {
        Self {
            id: format!("profile_{}", key),
            labels: Vec::new(),
            key: Some(key),
            metrics: Arc::new(RwLock::new(ProfileMetrics::default())),
        }
    }

    /// Get metrics
    pub fn metrics(&self) -> ProfileMetrics {
        sideeffect_read(&self.metrics).clone()
    }
}

impl Default for ProfileStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for ProfileStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "ProfileStep"
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
        self.side_effect(&traverser);
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        *sideeffect_write(&self.metrics) = ProfileMetrics::default();
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

impl SideEffectStep for ProfileStep {
    fn side_effect(&self, _traverser: &Traverser) {
        let mut metrics = sideeffect_write(&self.metrics);
        metrics.traverser_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_step() {
        let step = StoreStep::new("x".to_string());

        step.side_effect(&Traverser::new("v1"));
        step.side_effect(&Traverser::new("v2"));

        let values = step.values();
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn test_aggregate_step() {
        let step = AggregateStep::global("x".to_string());
        assert_eq!(step.name(), "AggregateGlobalStep");

        step.side_effect(&Traverser::new("v1"));
        assert_eq!(step.values().len(), 1);
    }

    #[test]
    fn test_aggregate_local() {
        let step = AggregateStep::local("x".to_string());
        assert_eq!(step.name(), "AggregateLocalStep");
    }

    #[test]
    fn test_property_step() {
        let step = PropertyStep::with_value("status".to_string(), json!("active"));
        assert_eq!(step.key(), "status");
    }

    #[test]
    fn test_property_cardinality() {
        let step = PropertyStep::with_value("tags".to_string(), json!("new"))
            .cardinality(PropertyCardinality::List);

        assert!(matches!(step.cardinality, PropertyCardinality::List));
    }

    #[test]
    fn test_sack_step_set() {
        let step = SackStep::set(json!(0));

        let traverser = Traverser::new("v1");
        let result = step.process_traverser(traverser);

        if let StepResult::Emit(t) = result {
            assert_eq!(t[0].sack(), Some(&json!(0)));
        }
    }

    #[test]
    fn test_profile_step() {
        let step = ProfileStep::new();

        step.side_effect(&Traverser::new("v1"));
        step.side_effect(&Traverser::new("v2"));

        let metrics = step.metrics();
        assert_eq!(metrics.traverser_count, 2);
    }

    #[test]
    fn test_store_step_recovers_after_values_lock_poisoning() {
        let step = StoreStep::new("x".to_string());
        let poison_target = step.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .values
                .write()
                .expect("store values lock should be acquired");
            panic!("poison store values lock");
        })
        .join();

        step.side_effect(&Traverser::new("v1"));
        let values = step.values();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["id"], json!("v1"));
    }

    #[test]
    fn test_profile_step_recovers_after_metrics_lock_poisoning() {
        let step = ProfileStep::new();
        let poison_target = step.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .metrics
                .write()
                .expect("profile metrics lock should be acquired");
            panic!("poison profile metrics lock");
        })
        .join();

        step.side_effect(&Traverser::new("v1"));
        assert_eq!(step.metrics().traverser_count, 1);
    }
}
