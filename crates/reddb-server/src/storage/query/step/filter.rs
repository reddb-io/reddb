//! Filter Steps
//!
//! Steps that filter traversers based on predicates.
//!
//! # Steps
//!
//! - `has()`: Property filter
//! - `where()`: Traversal-based filter
//! - `dedup()`: Deduplicate
//! - `range()`: Offset + limit
//! - `limit()`: Limit results
//! - `is()`: Value comparison
//! - `and()`, `or()`, `not()`: Logical connectives

use super::{Step, StepResult, Traverser, TraverserRequirement, TraverserValue};
use crate::serde_json::Value;
use std::any::Any;
use std::collections::HashSet;

/// Trait for filter steps
pub trait FilterStep: Step {
    /// Test if traverser passes filter
    fn test(&self, traverser: &Traverser) -> bool;
}

/// Predicate for comparisons
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// Equal
    Eq(Value),
    /// Not equal
    Neq(Value),
    /// Less than
    Lt(Value),
    /// Less than or equal
    Lte(Value),
    /// Greater than
    Gt(Value),
    /// Greater than or equal
    Gte(Value),
    /// Within set of values
    Within(Vec<Value>),
    /// Without (not in set)
    Without(Vec<Value>),
    /// Between (inclusive)
    Between(Value, Value),
    /// Inside (exclusive)
    Inside(Value, Value),
    /// Outside
    Outside(Value, Value),
    /// Starts with
    StartingWith(String),
    /// Ends with
    EndingWith(String),
    /// Contains
    Containing(String),
    /// Regex match
    Regex(String),
    /// Not predicate
    Not(Box<Predicate>),
}

impl Predicate {
    /// Test value against predicate
    pub fn test(&self, value: &Value) -> bool {
        match self {
            Predicate::Eq(expected) => value == expected,
            Predicate::Neq(expected) => value != expected,
            Predicate::Lt(expected) => {
                compare_json(value, expected) == Some(std::cmp::Ordering::Less)
            }
            Predicate::Lte(expected) => {
                matches!(
                    compare_json(value, expected),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
            Predicate::Gt(expected) => {
                compare_json(value, expected) == Some(std::cmp::Ordering::Greater)
            }
            Predicate::Gte(expected) => {
                matches!(
                    compare_json(value, expected),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )
            }
            Predicate::Within(set) => set.contains(value),
            Predicate::Without(set) => !set.contains(value),
            Predicate::Between(low, high) => {
                matches!(
                    compare_json(value, low),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                ) && matches!(
                    compare_json(value, high),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
            Predicate::Inside(low, high) => {
                compare_json(value, low) == Some(std::cmp::Ordering::Greater)
                    && compare_json(value, high) == Some(std::cmp::Ordering::Less)
            }
            Predicate::Outside(low, high) => {
                compare_json(value, low) == Some(std::cmp::Ordering::Less)
                    || compare_json(value, high) == Some(std::cmp::Ordering::Greater)
            }
            Predicate::StartingWith(prefix) => {
                if let Value::String(s) = value {
                    s.starts_with(prefix)
                } else {
                    false
                }
            }
            Predicate::EndingWith(suffix) => {
                if let Value::String(s) = value {
                    s.ends_with(suffix)
                } else {
                    false
                }
            }
            Predicate::Containing(substr) => {
                if let Value::String(s) = value {
                    s.contains(substr)
                } else {
                    false
                }
            }
            Predicate::Regex(pattern) => {
                // Simple regex support without external crate
                if let Value::String(s) = value {
                    simple_regex_match(s, pattern)
                } else {
                    false
                }
            }
            Predicate::Not(inner) => !inner.test(value),
        }
    }

    /// Get ranking for filter optimization (lower = more selective)
    pub fn ranking(&self) -> u32 {
        match self {
            Predicate::Eq(_) => 1, // Equality is most selective
            Predicate::Within(v) if v.len() == 1 => 1,
            Predicate::Neq(_) => 8, // Not equal is least selective
            Predicate::Within(v) => 2 + (v.len() as u32).min(5),
            Predicate::Without(_) => 7,
            Predicate::Between(_, _) | Predicate::Inside(_, _) => 3,
            Predicate::Lt(_) | Predicate::Lte(_) | Predicate::Gt(_) | Predicate::Gte(_) => 4,
            Predicate::StartingWith(_) => 2, // Prefix is often indexed
            Predicate::EndingWith(_) | Predicate::Containing(_) => 5,
            Predicate::Regex(_) => 6,
            Predicate::Outside(_, _) => 7,
            Predicate::Not(inner) => 10 - inner.ranking().min(9), // Inverse selectivity
        }
    }
}

/// Compare two JSON values
fn compare_json(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => na.partial_cmp(nb),
        (Value::String(sa), Value::String(sb)) => Some(sa.cmp(sb)),
        _ => None,
    }
}

/// Simple regex matching without external dependencies
fn simple_regex_match(s: &str, pattern: &str) -> bool {
    // Very basic pattern matching: * for wildcard, ^ for start, $ for end
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with('^') && pattern.ends_with('$') {
        return s == &pattern[1..pattern.len() - 1];
    }
    if let Some(stripped) = pattern.strip_prefix('^') {
        return s.starts_with(stripped);
    }
    if let Some(stripped) = pattern.strip_suffix('$') {
        return s.ends_with(stripped);
    }
    s.contains(pattern)
}

/// Has step - property-based filtering
#[derive(Debug, Clone)]
pub struct HasStep {
    id: String,
    labels: Vec<String>,
    /// Property key
    key: String,
    /// Predicate to test
    predicate: Option<Predicate>,
}

impl HasStep {
    /// Create has(key) step - tests property exists
    pub fn new(key: String) -> Self {
        Self {
            id: format!("has_{}", key),
            labels: Vec::new(),
            key,
            predicate: None,
        }
    }

    /// Create has(key, value) step - tests property equals value
    pub fn eq(key: String, value: Value) -> Self {
        Self {
            id: format!("has_{}_{}", key, value),
            labels: Vec::new(),
            key,
            predicate: Some(Predicate::Eq(value)),
        }
    }

    /// Create has(key, predicate) step
    pub fn with_predicate(key: String, predicate: Predicate) -> Self {
        Self {
            id: format!("has_{}_{:?}", key, predicate),
            labels: Vec::new(),
            key,
            predicate: Some(predicate),
        }
    }

    /// Get property key
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Get predicate
    pub fn predicate(&self) -> Option<&Predicate> {
        self.predicate.as_ref()
    }

    /// Set step ID
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }
}

impl Step for HasStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "HasStep"
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
        &[] // No special requirements
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        if self.test(&traverser) {
            StepResult::emit_one(traverser)
        } else {
            StepResult::Filter
        }
    }

    fn reset(&mut self) {
        // No state
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

impl FilterStep for HasStep {
    fn test(&self, traverser: &Traverser) -> bool {
        // Get property value from traverser
        let value = match traverser.value() {
            TraverserValue::Map(map) => map.get(&self.key).cloned(),
            TraverserValue::Vertex(id) => {
                // For vertices, we'd need to look up properties from graph store
                // For now, treat ID as special property
                if self.key == "id" || self.key == "@id" {
                    Some(Value::String(id.clone()))
                } else {
                    None
                }
            }
            _ => None,
        };

        match (&value, &self.predicate) {
            (Some(v), Some(pred)) => pred.test(v),
            (Some(_), None) => true, // Property exists
            (None, _) => false,      // Property doesn't exist
        }
    }
}

/// Where step - traversal-based filtering
#[derive(Debug, Clone)]
pub struct WhereStep {
    id: String,
    labels: Vec<String>,
    /// Label to compare with
    compare_label: Option<String>,
    /// Predicate for comparison
    predicate: Option<Predicate>,
}

impl WhereStep {
    /// Create where() step with label comparison
    pub fn compare(label: String) -> Self {
        Self {
            id: format!("where_{}", label),
            labels: Vec::new(),
            compare_label: Some(label),
            predicate: None,
        }
    }

    /// Create where() step with predicate
    pub fn with_predicate(label: String, predicate: Predicate) -> Self {
        Self {
            id: format!("where_{}_{:?}", label, predicate),
            labels: Vec::new(),
            compare_label: Some(label),
            predicate: Some(predicate),
        }
    }
}

impl Step for WhereStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "WhereStep"
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
        // Where step often needs path for label lookup
        static REQS: &[TraverserRequirement] =
            &[TraverserRequirement::Path, TraverserRequirement::Labels];
        REQS
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        if self.test(&traverser) {
            StepResult::emit_one(traverser)
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

impl FilterStep for WhereStep {
    fn test(&self, traverser: &Traverser) -> bool {
        // Look up labeled value in path
        if let Some(label) = &self.compare_label {
            if let Some(path) = traverser.path() {
                if let Some(labeled_value) = path.get(label) {
                    let json_value = labeled_value.to_json();
                    if let Some(pred) = &self.predicate {
                        return pred.test(&json_value);
                    }
                    // Just check labeled value exists
                    return true;
                }
            }
        }
        false
    }
}

/// Dedup step - removes duplicates
#[derive(Debug, Clone)]
pub struct DedupStep {
    id: String,
    labels: Vec<String>,
    /// Dedup scope (labels to dedup on)
    by_labels: Vec<String>,
    /// Seen values (state)
    seen: HashSet<u64>,
}

impl DedupStep {
    /// Create dedup() step
    pub fn new() -> Self {
        Self {
            id: "dedup_0".to_string(),
            labels: Vec::new(),
            by_labels: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Create dedup(label) step
    pub fn by_labels(by_labels: Vec<String>) -> Self {
        Self {
            id: format!("dedup_{}", by_labels.join("_")),
            labels: Vec::new(),
            by_labels,
            seen: HashSet::new(),
        }
    }

    /// Hash a traverser value for dedup
    fn hash_value(&self, traverser: &Traverser) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        if self.by_labels.is_empty() {
            // Dedup on current value
            traverser.value().hash(&mut hasher);
        } else {
            // Dedup on labeled values
            if let Some(path) = traverser.path() {
                for label in &self.by_labels {
                    if let Some(value) = path.get(label) {
                        value.hash(&mut hasher);
                    }
                }
            }
        }

        hasher.finish()
    }
}

impl Default for DedupStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for DedupStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "DedupStep"
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
        if self.by_labels.is_empty() {
            &[]
        } else {
            static REQS: &[TraverserRequirement] =
                &[TraverserRequirement::Path, TraverserRequirement::Labels];
            REQS
        }
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // Note: This needs mutable access to seen set
        // In real impl, this would use interior mutability
        let hash = self.hash_value(&traverser);
        // For now, just emit (actual dedup would be in barrier)
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.seen.clear();
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

impl FilterStep for DedupStep {
    fn test(&self, _traverser: &Traverser) -> bool {
        // Dedup is stateful - can't test without modifying state
        true
    }
}

/// Range step - offset and limit
#[derive(Debug, Clone)]
pub struct RangeStep {
    id: String,
    labels: Vec<String>,
    /// Start offset (inclusive)
    low: u64,
    /// End offset (exclusive)
    high: u64,
    /// Counter (state)
    count: u64,
}

impl RangeStep {
    /// Create range(low, high) step
    pub fn new(low: u64, high: u64) -> Self {
        Self {
            id: format!("range_{}_{}", low, high),
            labels: Vec::new(),
            low,
            high,
            count: 0,
        }
    }

    /// Get low bound
    pub fn low(&self) -> u64 {
        self.low
    }

    /// Get high bound
    pub fn high(&self) -> u64 {
        self.high
    }
}

impl Step for RangeStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "RangeStep"
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
        // Note: This needs mutable access to count
        // Would use interior mutability in real impl
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.count = 0;
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

impl FilterStep for RangeStep {
    fn test(&self, _traverser: &Traverser) -> bool {
        // Range is stateful
        true
    }
}

/// Limit step - shorthand for range(0, limit)
#[derive(Debug, Clone)]
pub struct LimitStep {
    id: String,
    labels: Vec<String>,
    /// Maximum count
    limit: u64,
    /// Current count (state)
    count: u64,
}

impl LimitStep {
    /// Create limit(n) step
    pub fn new(limit: u64) -> Self {
        Self {
            id: format!("limit_{}", limit),
            labels: Vec::new(),
            limit,
            count: 0,
        }
    }

    /// Get limit
    pub fn limit(&self) -> u64 {
        self.limit
    }
}

impl Step for LimitStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "LimitStep"
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
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.count = 0;
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

impl FilterStep for LimitStep {
    fn test(&self, _traverser: &Traverser) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn test_predicate_eq() {
        let pred = Predicate::Eq(json!("foo"));
        assert!(pred.test(&json!("foo")));
        assert!(!pred.test(&json!("bar")));
    }

    #[test]
    fn test_predicate_within() {
        let pred = Predicate::Within(vec![json!(1), json!(2), json!(3)]);
        assert!(pred.test(&json!(2)));
        assert!(!pred.test(&json!(4)));
    }

    #[test]
    fn test_predicate_between() {
        let pred = Predicate::Between(json!(10), json!(20));
        assert!(pred.test(&json!(15)));
        assert!(pred.test(&json!(10))); // Inclusive
        assert!(pred.test(&json!(20))); // Inclusive
        assert!(!pred.test(&json!(9)));
        assert!(!pred.test(&json!(21)));
    }

    #[test]
    fn test_predicate_starting_with() {
        let pred = Predicate::StartingWith("192.168".to_string());
        assert!(pred.test(&json!("192.168.1.1")));
        assert!(!pred.test(&json!("10.0.0.1")));
    }

    #[test]
    fn test_predicate_ranking() {
        let eq = Predicate::Eq(json!(1));
        let neq = Predicate::Neq(json!(1));
        assert!(eq.ranking() < neq.ranking());
    }

    #[test]
    fn test_has_step() {
        let step = HasStep::eq("status".to_string(), json!("active"));
        assert_eq!(step.key(), "status");
        assert!(step.predicate().is_some());
    }

    #[test]
    fn test_dedup_step() {
        let step = DedupStep::new();
        assert_eq!(step.name(), "DedupStep");
    }

    #[test]
    fn test_range_step() {
        let step = RangeStep::new(10, 20);
        assert_eq!(step.low(), 10);
        assert_eq!(step.high(), 20);
    }

    #[test]
    fn test_limit_step() {
        let step = LimitStep::new(100);
        assert_eq!(step.limit(), 100);
    }
}
