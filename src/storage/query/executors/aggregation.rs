//! Aggregation Framework
//!
//! Provides aggregation functions for query results.
//!
//! # Supported Functions
//!
//! - **COUNT**: Count rows (including COUNT(*) and COUNT DISTINCT)
//! - **SUM**: Sum numeric values
//! - **AVG**: Average of numeric values
//! - **MIN**: Minimum value
//! - **MAX**: Maximum value
//! - **STDDEV**: Standard deviation
//! - **VARIANCE**: Statistical variance
//! - **PERCENTILE**: Nth percentile value
//!
//! # GROUP BY
//!
//! Aggregations can be grouped by one or more columns.
//! HAVING clause filters groups after aggregation.

use std::collections::HashMap;

use super::super::engine::binding::{Binding, Value, Var};
use super::value_compare::total_compare_values;

// ============================================================================
// Aggregator Trait
// ============================================================================

/// Trait for aggregation functions
pub trait Aggregator: Send + Sync {
    /// Process a single value
    fn accumulate(&mut self, value: Option<&Value>);

    /// Get the final aggregated result
    fn finalize(&self) -> Value;

    /// Reset for new group
    fn reset(&mut self);

    /// Create a fresh copy for a new group
    fn new_instance(&self) -> Box<dyn Aggregator>;

    /// Name of the aggregator
    fn name(&self) -> &'static str;
}

// ============================================================================
// COUNT Aggregator
// ============================================================================

/// COUNT aggregator
#[derive(Debug, Clone, Default)]
pub struct CountAggregator {
    count: i64,
    count_all: bool, // COUNT(*) vs COUNT(column)
}

impl CountAggregator {
    /// Create COUNT(*) aggregator
    pub fn count_all() -> Self {
        Self {
            count: 0,
            count_all: true,
        }
    }

    /// Create COUNT(column) aggregator (ignores nulls)
    pub fn count_column() -> Self {
        Self {
            count: 0,
            count_all: false,
        }
    }
}

impl Aggregator for CountAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if self.count_all || (value.is_some() && !matches!(value, Some(Value::Null))) {
            self.count += 1;
        }
    }

    fn finalize(&self) -> Value {
        Value::Integer(self.count)
    }

    fn reset(&mut self) {
        self.count = 0;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self {
            count: 0,
            count_all: self.count_all,
        })
    }

    fn name(&self) -> &'static str {
        "COUNT"
    }
}

// ============================================================================
// COUNT DISTINCT Aggregator
// ============================================================================

/// COUNT DISTINCT aggregator
#[derive(Debug, Clone, Default)]
pub struct CountDistinctAggregator {
    seen: std::collections::HashSet<String>,
}

impl CountDistinctAggregator {
    pub fn new() -> Self {
        Self {
            seen: std::collections::HashSet::new(),
        }
    }
}

impl Aggregator for CountDistinctAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if !matches!(v, Value::Null) {
                self.seen.insert(value_to_string(v));
            }
        }
    }

    fn finalize(&self) -> Value {
        Value::Integer(self.seen.len() as i64)
    }

    fn reset(&mut self) {
        self.seen.clear();
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "COUNT_DISTINCT"
    }
}

// ============================================================================
// SUM Aggregator
// ============================================================================

/// SUM aggregator
#[derive(Debug, Clone, Default)]
pub struct SumAggregator {
    sum: f64,
    has_value: bool,
    all_integers: bool,
}

impl SumAggregator {
    pub fn new() -> Self {
        Self {
            sum: 0.0,
            has_value: false,
            all_integers: true,
        }
    }
}

impl Aggregator for SumAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            match v {
                Value::Integer(i) => {
                    self.sum += *i as f64;
                    self.has_value = true;
                }
                Value::Float(f) => {
                    self.sum += *f;
                    self.has_value = true;
                    if f.fract() != 0.0 {
                        self.all_integers = false;
                    }
                }
                _ => {}
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.has_value {
            if self.all_integers && self.sum.fract() == 0.0 {
                Value::Integer(self.sum as i64)
            } else {
                Value::Float(self.sum)
            }
        } else {
            Value::Null
        }
    }

    fn reset(&mut self) {
        self.sum = 0.0;
        self.has_value = false;
        self.all_integers = true;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "SUM"
    }
}

// ============================================================================
// AVG Aggregator
// ============================================================================

/// AVG aggregator
#[derive(Debug, Clone, Default)]
pub struct AvgAggregator {
    sum: f64,
    count: i64,
}

impl AvgAggregator {
    pub fn new() -> Self {
        Self { sum: 0.0, count: 0 }
    }
}

impl Aggregator for AvgAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if let Some(n) = value_to_number(v) {
                self.sum += n;
                self.count += 1;
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.count > 0 {
            Value::Float(self.sum / self.count as f64)
        } else {
            Value::Null
        }
    }

    fn reset(&mut self) {
        self.sum = 0.0;
        self.count = 0;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "AVG"
    }
}

// ============================================================================
// MIN Aggregator
// ============================================================================

/// MIN aggregator
#[derive(Debug, Clone, Default)]
pub struct MinAggregator {
    min: Option<Value>,
}

impl MinAggregator {
    pub fn new() -> Self {
        Self { min: None }
    }
}

impl Aggregator for MinAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if matches!(v, Value::Null) {
                return;
            }
            match &self.min {
                None => self.min = Some(v.clone()),
                Some(current) => {
                    if total_compare_values(v, current) == std::cmp::Ordering::Less {
                        self.min = Some(v.clone());
                    }
                }
            }
        }
    }

    fn finalize(&self) -> Value {
        self.min.clone().unwrap_or(Value::Null)
    }

    fn reset(&mut self) {
        self.min = None;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "MIN"
    }
}

// ============================================================================
// MAX Aggregator
// ============================================================================

/// MAX aggregator
#[derive(Debug, Clone, Default)]
pub struct MaxAggregator {
    max: Option<Value>,
}

impl MaxAggregator {
    pub fn new() -> Self {
        Self { max: None }
    }
}

impl Aggregator for MaxAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if matches!(v, Value::Null) {
                return;
            }
            match &self.max {
                None => self.max = Some(v.clone()),
                Some(current) => {
                    if total_compare_values(v, current) == std::cmp::Ordering::Greater {
                        self.max = Some(v.clone());
                    }
                }
            }
        }
    }

    fn finalize(&self) -> Value {
        self.max.clone().unwrap_or(Value::Null)
    }

    fn reset(&mut self) {
        self.max = None;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "MAX"
    }
}

// ============================================================================
// SAMPLE Aggregator
// ============================================================================

/// SAMPLE aggregator (returns first non-null value)
#[derive(Debug, Clone, Default)]
pub struct SampleAggregator {
    value: Option<Value>,
}

impl SampleAggregator {
    pub fn new() -> Self {
        Self { value: None }
    }
}

impl Aggregator for SampleAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if self.value.is_some() {
            return;
        }
        if let Some(v) = value {
            if !matches!(v, Value::Null) {
                self.value = Some(v.clone());
            }
        }
    }

    fn finalize(&self) -> Value {
        self.value.clone().unwrap_or(Value::Null)
    }

    fn reset(&mut self) {
        self.value = None;
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "SAMPLE"
    }
}

// ============================================================================
// GROUP_CONCAT Aggregator
// ============================================================================

/// GROUP_CONCAT aggregator
#[derive(Debug, Clone)]
pub struct GroupConcatAggregator {
    separator: String,
    values: Vec<String>,
}

impl GroupConcatAggregator {
    pub fn new(separator: Option<String>) -> Self {
        Self {
            separator: separator.unwrap_or_else(|| " ".to_string()),
            values: Vec::new(),
        }
    }
}

impl Aggregator for GroupConcatAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if !matches!(v, Value::Null) {
                self.values.push(value_to_string(v));
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.values.is_empty() {
            Value::Null
        } else {
            Value::String(self.values.join(&self.separator))
        }
    }

    fn reset(&mut self) {
        self.values.clear();
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new(Some(self.separator.clone())))
    }

    fn name(&self) -> &'static str {
        "GROUP_CONCAT"
    }
}

// ============================================================================
// STDDEV Aggregator (Population Standard Deviation)
// ============================================================================

/// Standard deviation aggregator
#[derive(Debug, Clone, Default)]
pub struct StdDevAggregator {
    values: Vec<f64>,
}

impl StdDevAggregator {
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }
}

impl Aggregator for StdDevAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if let Some(n) = value_to_number(v) {
                self.values.push(n);
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.values.is_empty() {
            return Value::Null;
        }

        let n = self.values.len() as f64;
        let mean = self.values.iter().sum::<f64>() / n;
        let variance = self.values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;

        Value::Float(variance.sqrt())
    }

    fn reset(&mut self) {
        self.values.clear();
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "STDDEV"
    }
}

// ============================================================================
// VARIANCE Aggregator
// ============================================================================

/// Variance aggregator
#[derive(Debug, Clone, Default)]
pub struct VarianceAggregator {
    values: Vec<f64>,
}

impl VarianceAggregator {
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }
}

impl Aggregator for VarianceAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if let Some(n) = value_to_number(v) {
                self.values.push(n);
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.values.is_empty() {
            return Value::Null;
        }

        let n = self.values.len() as f64;
        let mean = self.values.iter().sum::<f64>() / n;
        let variance = self.values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;

        Value::Float(variance)
    }

    fn reset(&mut self) {
        self.values.clear();
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new())
    }

    fn name(&self) -> &'static str {
        "VARIANCE"
    }
}

// ============================================================================
// PERCENTILE Aggregator
// ============================================================================

/// Percentile aggregator
#[derive(Debug, Clone)]
pub struct PercentileAggregator {
    values: Vec<f64>,
    percentile: f64, // 0.0 to 1.0 (e.g., 0.5 for median)
}

impl PercentileAggregator {
    pub fn new(percentile: f64) -> Self {
        Self {
            values: Vec::new(),
            percentile: percentile.clamp(0.0, 1.0),
        }
    }

    /// Create median aggregator (50th percentile)
    pub fn median() -> Self {
        Self::new(0.5)
    }
}

impl Aggregator for PercentileAggregator {
    fn accumulate(&mut self, value: Option<&Value>) {
        if let Some(v) = value {
            if let Some(n) = value_to_number(v) {
                self.values.push(n);
            }
        }
    }

    fn finalize(&self) -> Value {
        if self.values.is_empty() {
            return Value::Null;
        }

        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let index = (self.percentile * (sorted.len() - 1) as f64).round() as usize;
        Value::Float(sorted[index])
    }

    fn reset(&mut self) {
        self.values.clear();
    }

    fn new_instance(&self) -> Box<dyn Aggregator> {
        Box::new(Self::new(self.percentile))
    }

    fn name(&self) -> &'static str {
        "PERCENTILE"
    }
}

// ============================================================================
// GROUP BY Executor
// ============================================================================

/// Definition of an aggregation to compute
pub struct AggregationDef {
    /// Source variable to aggregate
    pub source_var: Var,
    /// Result variable name
    pub result_var: Var,
    /// Aggregator factory
    pub aggregator: Box<dyn Aggregator>,
}

impl std::fmt::Debug for AggregationDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregationDef")
            .field("source_var", &self.source_var)
            .field("result_var", &self.result_var)
            .field("aggregator", &self.aggregator.name())
            .finish()
    }
}

/// Soft memory cap for in-process hash aggregation.
///
/// When the groups HashMap grows beyond this threshold, an OOM-guard
/// warning fires. Full spill-to-disk requires changing the calling
/// convention to a row-at-a-time streaming API (tracked separately).
const WORK_MEM_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Estimated heap cost per group entry in the streaming HashMap.
///
/// In practice each entry holds:
///   - a String group-key (~32 B avg)
///   - group key Var/Value pairs (~64 B)
///   - one Box<dyn Aggregator> per agg_def (~64 B each, assume ≤4 defs → ~256 B)
///
/// 512 B is deliberately conservative to avoid premature eviction
/// in the common case.
const AVG_GROUP_ENTRY_BYTES: usize = 512;

/// 1-pass streaming GROUP BY.
///
/// Previous implementation accumulated ALL input bindings per group
/// (`HashMap<String, Vec<Binding>>`), then ran aggregations in a
/// second pass. Memory cost: O(input_rows) in the groups map.
///
/// This version keeps only the incremental aggregation state per group
/// — one `Box<dyn Aggregator>` per `AggregationDef`. Memory cost drops
/// to O(distinct_groups × agg_defs), which is dramatically lower for
/// high-cardinality inputs with few distinct groups.
pub fn execute_group_by(
    bindings: Vec<Binding>,
    group_vars: &[Var],
    aggregations: &[AggregationDef],
) -> Vec<Binding> {
    // Each entry: (snapshot of group-key values from first binding,
    //              incremental aggregator state for each agg_def)
    let mut groups: HashMap<String, (Binding, Vec<Box<dyn Aggregator>>)> = HashMap::new();

    for binding in &bindings {
        let key = make_group_key(binding, group_vars);
        let entry = groups.entry(key).or_insert_with(|| {
            // Capture group key values once from the first binding in this group.
            let mut key_binding = Binding::empty();
            for var in group_vars {
                if let Some(value) = binding.get(var) {
                    let partial = Binding::one(var.clone(), value.clone());
                    key_binding = key_binding.merge(&partial).unwrap_or(key_binding);
                }
            }
            // Allocate one fresh aggregator instance per agg def.
            let agg_instances = aggregations
                .iter()
                .map(|a| a.aggregator.new_instance())
                .collect();
            (key_binding, agg_instances)
        });

        // Accumulate each aggregation in a single pass over the binding.
        for (i, agg_def) in aggregations.iter().enumerate() {
            entry.1[i].accumulate(binding.get(&agg_def.source_var));
        }

        // Memory guard: O(1) check, avoids estimating actual heap usage.
        // When the number of distinct groups × avg cost exceeds WORK_MEM,
        // we've likely exhausted the intended budget. For now we continue
        // (the data is already in memory via the input Vec<Binding>) but
        // emit a debug trace so operators can see when this fires.
        #[cfg(debug_assertions)]
        if groups.len() * AVG_GROUP_ENTRY_BYTES > WORK_MEM_BYTES {
            // Only log once — on entry count crossing the threshold,
            // not on every subsequent row.
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "[reddb] hash-agg: {} distinct groups × {} B ≈ {} MiB exceeds WORK_MEM {}  MiB; \
                     disk spill not yet wired — upgrade calling convention to streaming for OOM safety",
                    groups.len(),
                    AVG_GROUP_ENTRY_BYTES,
                    (groups.len() * AVG_GROUP_ENTRY_BYTES) / (1024 * 1024),
                    WORK_MEM_BYTES / (1024 * 1024),
                );
            }
        }
    }

    // Finalize: emit one output Binding per distinct group.
    let mut results = Vec::with_capacity(groups.len());
    for (_, (key_binding, agg_instances)) in groups {
        let mut result = key_binding;
        for (i, agg_def) in aggregations.iter().enumerate() {
            let agg_result = agg_instances[i].finalize();
            let partial = Binding::one(agg_def.result_var.clone(), agg_result);
            result = result.merge(&partial).unwrap_or(result);
        }
        results.push(result);
    }
    results
}

/// Execute HAVING clause (filter on aggregated results)
pub fn execute_having<F>(bindings: Vec<Binding>, predicate: F) -> Vec<Binding>
where
    F: Fn(&Binding) -> bool,
{
    bindings.into_iter().filter(|b| predicate(b)).collect()
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Format a single `Value` as a String. **Cold path** — used by
/// non-hot consumers like GROUP_CONCAT result formatting. The
/// hot group-by key path inlines this logic into a shared buffer
/// in [`make_group_key`] to avoid per-row allocations.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::Node(id) => format!("node:{}", id),
        Value::Edge(id) => format!("edge:{}", id),
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Uri(u) => u.clone(),
        Value::Null => "null".to_string(),
    }
}

/// Build a group-by key for one row.
///
/// **Hot path** — called once per row in `execute_group_by`. The
/// previous implementation paid `N+2` String allocations per row
/// (one per `value_to_string`, one per `format!`, one for the final
/// `join("|")`). This version writes everything into a single
/// `String` buffer with one allocation.
///
/// On a 3-column GROUP BY the difference is ~5 allocations vs 1,
/// which on a 1M-row aggregation saves ~4M small allocations.
fn make_group_key(binding: &Binding, group_vars: &[Var]) -> String {
    use std::fmt::Write;
    // Tunable initial capacity. 64 bytes covers most numeric / short
    // text group keys in one allocation; longer text grows in place
    // through String's exponential growth.
    let mut key = String::with_capacity(64);
    for (i, var) in group_vars.iter().enumerate() {
        if i > 0 {
            key.push('|');
        }
        key.push_str(var.name());
        key.push('=');
        match binding.get(var) {
            None => key.push_str("NULL"),
            Some(Value::Null) => key.push_str("null"),
            Some(Value::String(s)) => key.push_str(s),
            Some(Value::Integer(n)) => {
                let _ = write!(key, "{n}");
            }
            Some(Value::Float(f)) => {
                let _ = write!(key, "{f}");
            }
            Some(Value::Boolean(b)) => {
                let _ = write!(key, "{b}");
            }
            Some(Value::Node(id)) => {
                key.push_str("node:");
                key.push_str(id);
            }
            Some(Value::Edge(id)) => {
                key.push_str("edge:");
                key.push_str(id);
            }
            Some(Value::Uri(u)) => key.push_str(u),
        }
    }
    key
}

fn value_to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

// ============================================================================
// Aggregator Factory
// ============================================================================

/// Create an aggregator by name
pub fn create_aggregator(name: &str) -> Option<Box<dyn Aggregator>> {
    match name.to_uppercase().as_str() {
        "COUNT" => Some(Box::new(CountAggregator::count_all())),
        "COUNT_COLUMN" => Some(Box::new(CountAggregator::count_column())),
        "COUNT_DISTINCT" => Some(Box::new(CountDistinctAggregator::new())),
        "SUM" => Some(Box::new(SumAggregator::new())),
        "AVG" => Some(Box::new(AvgAggregator::new())),
        "MIN" => Some(Box::new(MinAggregator::new())),
        "MAX" => Some(Box::new(MaxAggregator::new())),
        "STDDEV" => Some(Box::new(StdDevAggregator::new())),
        "VARIANCE" => Some(Box::new(VarianceAggregator::new())),
        "MEDIAN" => Some(Box::new(PercentileAggregator::median())),
        "SAMPLE" => Some(Box::new(SampleAggregator::new())),
        "GROUP_CONCAT" => Some(Box::new(GroupConcatAggregator::new(None))),
        _ => None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_binding(pairs: &[(&str, Value)]) -> Binding {
        if pairs.is_empty() {
            return Binding::empty();
        }

        let mut result = Binding::one(Var::new(pairs[0].0), pairs[0].1.clone());

        for (k, v) in pairs.iter().skip(1) {
            let next = Binding::one(Var::new(k), v.clone());
            result = result.merge(&next).unwrap_or(result);
        }

        result
    }

    #[test]
    fn test_count() {
        let mut counter = CountAggregator::count_all();
        counter.accumulate(Some(&Value::Integer(1)));
        counter.accumulate(Some(&Value::Integer(2)));
        counter.accumulate(None);
        counter.accumulate(Some(&Value::Null));

        assert_eq!(counter.finalize(), Value::Integer(4));
    }

    #[test]
    fn test_count_column() {
        let mut counter = CountAggregator::count_column();
        counter.accumulate(Some(&Value::Integer(1)));
        counter.accumulate(None);
        counter.accumulate(Some(&Value::Null));
        counter.accumulate(Some(&Value::Integer(2)));

        assert_eq!(counter.finalize(), Value::Integer(2)); // Only non-null values
    }

    #[test]
    fn test_sum() {
        let mut sum = SumAggregator::new();
        sum.accumulate(Some(&Value::Integer(10)));
        sum.accumulate(Some(&Value::Float(5.5)));
        sum.accumulate(Some(&Value::Integer(4)));

        assert_eq!(sum.finalize(), Value::Float(19.5));
    }

    #[test]
    fn test_avg() {
        let mut avg = AvgAggregator::new();
        avg.accumulate(Some(&Value::Integer(10)));
        avg.accumulate(Some(&Value::Integer(20)));
        avg.accumulate(Some(&Value::Integer(30)));

        assert_eq!(avg.finalize(), Value::Float(20.0));
    }

    #[test]
    fn test_min_max() {
        let mut min = MinAggregator::new();
        let mut max = MaxAggregator::new();

        for val in [5, 2, 8, 1, 9] {
            min.accumulate(Some(&Value::Integer(val)));
            max.accumulate(Some(&Value::Integer(val)));
        }

        assert_eq!(min.finalize(), Value::Integer(1));
        assert_eq!(max.finalize(), Value::Integer(9));
    }

    #[test]
    fn test_count_distinct() {
        let mut distinct = CountDistinctAggregator::new();
        distinct.accumulate(Some(&Value::String("a".to_string())));
        distinct.accumulate(Some(&Value::String("b".to_string())));
        distinct.accumulate(Some(&Value::String("a".to_string())));
        distinct.accumulate(Some(&Value::String("c".to_string())));

        assert_eq!(distinct.finalize(), Value::Integer(3));
    }

    #[test]
    fn test_group_by() {
        let bindings = vec![
            make_binding(&[
                ("dept", Value::String("Sales".to_string())),
                ("salary", Value::Integer(50000)),
            ]),
            make_binding(&[
                ("dept", Value::String("Sales".to_string())),
                ("salary", Value::Integer(60000)),
            ]),
            make_binding(&[
                ("dept", Value::String("Engineering".to_string())),
                ("salary", Value::Integer(80000)),
            ]),
            make_binding(&[
                ("dept", Value::String("Engineering".to_string())),
                ("salary", Value::Integer(90000)),
            ]),
        ];

        let aggs = vec![
            AggregationDef {
                source_var: Var::new("salary"),
                result_var: Var::new("total"),
                aggregator: Box::new(SumAggregator::new()),
            },
            AggregationDef {
                source_var: Var::new("salary"),
                result_var: Var::new("count"),
                aggregator: Box::new(CountAggregator::count_all()),
            },
        ];

        let results = execute_group_by(bindings, &[Var::new("dept")], &aggs);

        assert_eq!(results.len(), 2);

        // Find Sales result
        let sales = results
            .iter()
            .find(|b| b.get(&Var::new("dept")) == Some(&Value::String("Sales".to_string())))
            .expect("Sales group not found");

        assert_eq!(sales.get(&Var::new("total")), Some(&Value::Integer(110000)));
        assert_eq!(sales.get(&Var::new("count")), Some(&Value::Integer(2)));
    }

    #[test]
    fn test_percentile() {
        let mut p50 = PercentileAggregator::median();
        for v in [1, 2, 3, 4, 5, 6, 7, 8, 9] {
            p50.accumulate(Some(&Value::Integer(v)));
        }
        // Median of 1-9 is 5
        assert_eq!(p50.finalize(), Value::Float(5.0));
    }
}
