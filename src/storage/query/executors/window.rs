//! Window Functions Executor
//!
//! Provides SQL standard window functions for analytical queries.
//!
//! # Window Function Types
//!
//! **Ranking Functions:**
//! - `ROW_NUMBER()`: Sequential number within partition
//! - `RANK()`: Rank with gaps for ties
//! - `DENSE_RANK()`: Rank without gaps for ties
//! - `NTILE(n)`: Divide partition into n buckets
//! - `PERCENT_RANK()`: Relative rank as percentage
//! - `CUME_DIST()`: Cumulative distribution
//!
//! **Value Functions:**
//! - `FIRST_VALUE(x)`: First value in frame
//! - `LAST_VALUE(x)`: Last value in frame
//! - `NTH_VALUE(x, n)`: Nth value in frame
//! - `LAG(x, n, default)`: Value n rows before current
//! - `LEAD(x, n, default)`: Value n rows after current
//!
//! **Aggregate Functions (with OVER):**
//! - All standard aggregates (SUM, AVG, COUNT, MIN, MAX, etc.)
//!
//! # Frame Specification
//!
//! Frames define the subset of partition rows for each computation:
//! - `ROWS`: Physical row-based boundaries
//! - `RANGE`: Value-based logical boundaries
//! - `GROUPS`: Groups of peer rows
//!
//! # Implementation
//!
//! Window functions are evaluated in three phases:
//! 1. **Partition**: Group rows by PARTITION BY columns
//! 2. **Order**: Sort each partition by ORDER BY columns
//! 3. **Compute**: Apply window function with frame for each row

use std::cmp::Ordering;
use std::collections::HashMap;

use super::super::engine::binding::{Binding, Value, Var};
use super::aggregation::create_aggregator;

// ============================================================================
// Window Function Types
// ============================================================================

/// Type of window function
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowFuncType {
    // Ranking functions
    RowNumber,
    Rank,
    DenseRank,
    Ntile(i64),
    PercentRank,
    CumeDist,

    // Value functions
    FirstValue(Var),
    LastValue(Var),
    NthValue(Var, i64),
    Lag(Var, i64, Option<Value>),
    Lead(Var, i64, Option<Value>),

    // Aggregate functions with OVER
    Aggregate(String, Var),
}

impl WindowFuncType {
    /// Create a ROW_NUMBER function
    pub fn row_number() -> Self {
        Self::RowNumber
    }

    /// Create a RANK function
    pub fn rank() -> Self {
        Self::Rank
    }

    /// Create a DENSE_RANK function
    pub fn dense_rank() -> Self {
        Self::DenseRank
    }

    /// Create an NTILE function
    pub fn ntile(n: i64) -> Self {
        Self::Ntile(n)
    }

    /// Create a LAG function
    pub fn lag(var: Var, offset: i64, default: Option<Value>) -> Self {
        Self::Lag(var, offset, default)
    }

    /// Create a LEAD function
    pub fn lead(var: Var, offset: i64, default: Option<Value>) -> Self {
        Self::Lead(var, offset, default)
    }

    /// Create a FIRST_VALUE function
    pub fn first_value(var: Var) -> Self {
        Self::FirstValue(var)
    }

    /// Create a LAST_VALUE function
    pub fn last_value(var: Var) -> Self {
        Self::LastValue(var)
    }

    /// Create an aggregate window function
    pub fn aggregate(name: &str, var: Var) -> Self {
        Self::Aggregate(name.to_uppercase(), var)
    }
}

// ============================================================================
// Frame Specification
// ============================================================================

/// Frame type for window functions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Physical row boundaries
    Rows,
    /// Value-based boundaries (for RANGE)
    Range,
    /// Groups of peer rows
    Groups,
}

/// Frame boundary specification
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FrameBound {
    /// UNBOUNDED PRECEDING
    UnboundedPreceding,
    /// UNBOUNDED FOLLOWING
    UnboundedFollowing,
    /// CURRENT ROW
    #[default]
    CurrentRow,
    /// n PRECEDING
    Preceding(i64),
    /// n FOLLOWING
    Following(i64),
}

/// Frame specification for window function
#[derive(Debug, Clone)]
pub struct FrameSpec {
    /// Frame type (ROWS, RANGE, GROUPS)
    pub frame_type: FrameType,
    /// Start boundary
    pub start: FrameBound,
    /// End boundary
    pub end: FrameBound,
    /// Exclude option (CURRENT ROW, GROUP, TIES, NO OTHERS)
    pub exclude: FrameExclude,
}

/// Frame exclusion option
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameExclude {
    /// EXCLUDE NO OTHERS (default)
    #[default]
    NoOthers,
    /// EXCLUDE CURRENT ROW
    CurrentRow,
    /// EXCLUDE GROUP
    Group,
    /// EXCLUDE TIES
    Ties,
}

impl Default for FrameSpec {
    /// Default frame: RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
    fn default() -> Self {
        Self {
            frame_type: FrameType::Range,
            start: FrameBound::UnboundedPreceding,
            end: FrameBound::CurrentRow,
            exclude: FrameExclude::NoOthers,
        }
    }
}

impl FrameSpec {
    /// Create ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
    pub fn entire_partition() -> Self {
        Self {
            frame_type: FrameType::Rows,
            start: FrameBound::UnboundedPreceding,
            end: FrameBound::UnboundedFollowing,
            exclude: FrameExclude::NoOthers,
        }
    }

    /// Create ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
    pub fn running() -> Self {
        Self {
            frame_type: FrameType::Rows,
            start: FrameBound::UnboundedPreceding,
            end: FrameBound::CurrentRow,
            exclude: FrameExclude::NoOthers,
        }
    }

    /// Create ROWS BETWEEN n PRECEDING AND CURRENT ROW (sliding window)
    pub fn sliding(n: i64) -> Self {
        Self {
            frame_type: FrameType::Rows,
            start: FrameBound::Preceding(n),
            end: FrameBound::CurrentRow,
            exclude: FrameExclude::NoOthers,
        }
    }
}

// ============================================================================
// Window Definition
// ============================================================================

/// Sort direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

/// Null handling in sorting
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NullsOrder {
    #[default]
    First,
    Last,
}

/// Order-by specification for window
#[derive(Debug, Clone)]
pub struct WindowOrderBy {
    /// Variable to sort by
    pub var: Var,
    /// Sort direction
    pub direction: SortDirection,
    /// Null handling
    pub nulls: NullsOrder,
}

impl WindowOrderBy {
    pub fn new(var: Var) -> Self {
        Self {
            var,
            direction: SortDirection::Asc,
            nulls: NullsOrder::Last,
        }
    }

    pub fn desc(mut self) -> Self {
        self.direction = SortDirection::Desc;
        self
    }

    pub fn nulls_first(mut self) -> Self {
        self.nulls = NullsOrder::First;
        self
    }
}

/// Complete window definition
#[derive(Debug, Clone, Default)]
pub struct WindowDef {
    /// Optional window name
    pub name: Option<String>,
    /// Partition by variables
    pub partition_by: Vec<Var>,
    /// Order by specifications
    pub order_by: Vec<WindowOrderBy>,
    /// Frame specification
    pub frame: FrameSpec,
}

impl WindowDef {
    /// Create a window with partition by
    pub fn partition_by(vars: Vec<Var>) -> Self {
        Self {
            partition_by: vars,
            ..Default::default()
        }
    }

    /// Add order by
    pub fn with_order_by(mut self, order: Vec<WindowOrderBy>) -> Self {
        self.order_by = order;
        self
    }

    /// Set frame specification
    pub fn with_frame(mut self, frame: FrameSpec) -> Self {
        self.frame = frame;
        self
    }
}

// ============================================================================
// Window Function Application
// ============================================================================

/// A window function to apply
#[derive(Debug, Clone)]
pub struct WindowFunc {
    /// The function type
    pub func_type: WindowFuncType,
    /// Result variable name
    pub result_var: Var,
    /// Window definition
    pub window: WindowDef,
}

impl WindowFunc {
    /// Create a new window function
    pub fn new(func_type: WindowFuncType, result_var: Var, window: WindowDef) -> Self {
        Self {
            func_type,
            result_var,
            window,
        }
    }
}

// ============================================================================
// Window Executor
// ============================================================================

/// Window function executor
pub struct WindowExecutor;

#[derive(Debug, Clone)]
struct IndexedBinding {
    index: usize,
    binding: Binding,
}

impl WindowExecutor {
    /// Execute window functions on bindings
    pub fn execute(bindings: Vec<Binding>, functions: &[WindowFunc]) -> Vec<Binding> {
        if bindings.is_empty() || functions.is_empty() {
            return bindings;
        }

        // We need to execute each window function
        let mut result = bindings;

        for func in functions {
            result = Self::apply_window_function(&result, func);
        }

        result
    }

    /// Apply a single window function
    fn apply_window_function(bindings: &[Binding], func: &WindowFunc) -> Vec<Binding> {
        // Step 1: Partition the data
        let partitions = Self::partition_bindings(bindings, &func.window.partition_by);

        // Step 2: For each partition, sort and compute
        let mut result: Vec<Option<Binding>> = vec![None; bindings.len()];

        for (_key, mut partition) in partitions {
            // Sort within partition
            Self::sort_partition(&mut partition, &func.window.order_by);

            // Compute window function for each row
            let computed = Self::compute_for_partition(&partition, func);
            for entry in computed {
                if entry.index < result.len() {
                    result[entry.index] = Some(entry.binding);
                }
            }
        }

        result
            .into_iter()
            .enumerate()
            .map(|(idx, binding)| binding.unwrap_or_else(|| bindings[idx].clone()))
            .collect()
    }

    /// Partition bindings by partition-by variables
    fn partition_bindings(
        bindings: &[Binding],
        partition_by: &[Var],
    ) -> Vec<(Vec<Option<Value>>, Vec<IndexedBinding>)> {
        if partition_by.is_empty() {
            // No partitioning - single partition with all rows
            let entries = bindings
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, binding)| IndexedBinding { index, binding })
                .collect();
            return vec![(vec![], entries)];
        }

        let mut partitions: HashMap<Vec<Option<Value>>, Vec<IndexedBinding>> = HashMap::new();
        let mut key_order: Vec<Vec<Option<Value>>> = Vec::new();

        for (index, binding) in bindings.iter().cloned().enumerate() {
            let key_values: Vec<Option<Value>> = partition_by
                .iter()
                .map(|v| binding.get(v).cloned())
                .collect();

            if !partitions.contains_key(&key_values) {
                key_order.push(key_values.clone());
            }

            partitions
                .entry(key_values)
                .or_default()
                .push(IndexedBinding { index, binding });
        }

        // Maintain insertion order
        key_order
            .into_iter()
            .filter_map(|values| partitions.remove(&values).map(|rows| (values, rows)))
            .collect()
    }

    /// Sort partition by order-by specifications
    fn sort_partition(partition: &mut [IndexedBinding], order_by: &[WindowOrderBy]) {
        if order_by.is_empty() {
            return;
        }

        partition.sort_by(|a, b| {
            for spec in order_by {
                let val_a = a.binding.get(&spec.var);
                let val_b = b.binding.get(&spec.var);

                let cmp = match (val_a, val_b) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => match spec.nulls {
                        NullsOrder::First => Ordering::Less,
                        NullsOrder::Last => Ordering::Greater,
                    },
                    (Some(_), None) => match spec.nulls {
                        NullsOrder::First => Ordering::Greater,
                        NullsOrder::Last => Ordering::Less,
                    },
                    (Some(a), Some(b)) => compare_values(a, b),
                };

                if cmp != Ordering::Equal {
                    return match spec.direction {
                        SortDirection::Asc => cmp,
                        SortDirection::Desc => cmp.reverse(),
                    };
                }
            }
            a.index.cmp(&b.index)
        });
    }

    /// Compute window function for each row in a partition
    fn compute_for_partition(
        partition: &[IndexedBinding],
        func: &WindowFunc,
    ) -> Vec<IndexedBinding> {
        let partition_size = partition.len();

        // Pre-compute peer groups for ranking functions
        let peer_groups = Self::compute_peer_groups(partition, &func.window.order_by);

        partition
            .iter()
            .enumerate()
            .map(|(row_idx, indexed)| {
                let value =
                    Self::compute_value(partition, row_idx, &peer_groups, partition_size, func);

                // Add result to binding
                let result_binding = Binding::one(func.result_var.clone(), value);
                let binding = indexed
                    .binding
                    .merge(&result_binding)
                    .unwrap_or_else(|| indexed.binding.clone());
                IndexedBinding {
                    index: indexed.index,
                    binding,
                }
            })
            .collect()
    }

    /// Compute peer groups (rows with same ORDER BY values)
    fn compute_peer_groups(partition: &[IndexedBinding], order_by: &[WindowOrderBy]) -> Vec<usize> {
        if order_by.is_empty() {
            // No ordering - all rows are peers (single group)
            return vec![0; partition.len()];
        }

        let mut groups = Vec::with_capacity(partition.len());
        let mut current_group = 0;

        for (idx, indexed) in partition.iter().enumerate() {
            if idx == 0 {
                groups.push(0);
                continue;
            }

            let prev = &partition[idx - 1].binding;
            let binding = &indexed.binding;
            let is_peer = order_by.iter().all(|spec| {
                let a = prev.get(&spec.var);
                let b = binding.get(&spec.var);
                match (a, b) {
                    (None, None) => true,
                    (Some(va), Some(vb)) => values_equal(va, vb),
                    _ => false,
                }
            });

            if !is_peer {
                current_group += 1;
            }
            groups.push(current_group);
        }

        groups
    }

    /// Compute value for a single row
    fn compute_value(
        partition: &[IndexedBinding],
        row_idx: usize,
        peer_groups: &[usize],
        partition_size: usize,
        func: &WindowFunc,
    ) -> Value {
        match &func.func_type {
            // Ranking functions
            WindowFuncType::RowNumber => Value::Integer((row_idx + 1) as i64),

            WindowFuncType::Rank => {
                // Rank = position of first row in current peer group + 1
                let current_group = peer_groups[row_idx];
                let first_in_group = peer_groups
                    .iter()
                    .position(|&g| g == current_group)
                    .unwrap();
                Value::Integer((first_in_group + 1) as i64)
            }

            WindowFuncType::DenseRank => {
                // Dense rank = peer group number + 1
                Value::Integer((peer_groups[row_idx] + 1) as i64)
            }

            WindowFuncType::Ntile(n) => {
                // Divide into n buckets
                let n = *n as usize;
                if n == 0 || partition_size == 0 {
                    return Value::Null;
                }
                let bucket_size = partition_size / n;
                let remainder = partition_size % n;

                // Rows are distributed: first `remainder` buckets get one extra row
                let mut row = 0;
                let mut bucket = 1;
                for i in 0..n {
                    let size = bucket_size + if i < remainder { 1 } else { 0 };
                    if row_idx < row + size {
                        bucket = i + 1;
                        break;
                    }
                    row += size;
                }
                Value::Integer(bucket as i64)
            }

            WindowFuncType::PercentRank => {
                // (rank - 1) / (partition_size - 1)
                if partition_size <= 1 {
                    return Value::Float(0.0);
                }
                let current_group = peer_groups[row_idx];
                let first_in_group = peer_groups
                    .iter()
                    .position(|&g| g == current_group)
                    .unwrap();
                let rank = first_in_group as f64;
                Value::Float(rank / (partition_size - 1) as f64)
            }

            WindowFuncType::CumeDist => {
                // count of rows <= current row / partition_size
                let current_group = peer_groups[row_idx];
                // Count all rows up to and including current peer group
                let count = peer_groups.iter().filter(|&&g| g <= current_group).count();
                Value::Float(count as f64 / partition_size as f64)
            }

            // Value functions
            WindowFuncType::FirstValue(var) => {
                // Get frame bounds
                let (start, _) = Self::get_frame_bounds(
                    row_idx,
                    partition_size,
                    peer_groups,
                    &func.window.frame,
                );
                partition
                    .get(start)
                    .and_then(|b| b.binding.get(var))
                    .cloned()
                    .unwrap_or(Value::Null)
            }

            WindowFuncType::LastValue(var) => {
                let (_, end) = Self::get_frame_bounds(
                    row_idx,
                    partition_size,
                    peer_groups,
                    &func.window.frame,
                );
                // End is exclusive, so use end - 1
                if end > 0 {
                    partition
                        .get(end - 1)
                        .and_then(|b| b.binding.get(var))
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }

            WindowFuncType::NthValue(var, n) => {
                let (start, end) = Self::get_frame_bounds(
                    row_idx,
                    partition_size,
                    peer_groups,
                    &func.window.frame,
                );
                let n = *n as usize;
                if n == 0 {
                    return Value::Null;
                }
                let target_idx = start + n - 1;
                if target_idx < end {
                    partition
                        .get(target_idx)
                        .and_then(|b| b.binding.get(var))
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }

            WindowFuncType::Lag(var, offset, default) => {
                let offset = *offset as usize;
                if row_idx >= offset {
                    partition
                        .get(row_idx - offset)
                        .and_then(|b| b.binding.get(var))
                        .cloned()
                        .unwrap_or_else(|| default.clone().unwrap_or(Value::Null))
                } else {
                    default.clone().unwrap_or(Value::Null)
                }
            }

            WindowFuncType::Lead(var, offset, default) => {
                let offset = *offset as usize;
                let target = row_idx + offset;
                if target < partition_size {
                    partition
                        .get(target)
                        .and_then(|b| b.binding.get(var))
                        .cloned()
                        .unwrap_or_else(|| default.clone().unwrap_or(Value::Null))
                } else {
                    default.clone().unwrap_or(Value::Null)
                }
            }

            // Aggregate functions
            WindowFuncType::Aggregate(agg_name, var) => {
                let (start, end) = Self::get_frame_bounds(
                    row_idx,
                    partition_size,
                    peer_groups,
                    &func.window.frame,
                );

                if let Some(mut aggregator) = create_aggregator(agg_name) {
                    for i in start..end {
                        if let Some(binding) = partition.get(i) {
                            let value = binding.binding.get(var);
                            aggregator.accumulate(value);
                        }
                    }
                    aggregator.finalize()
                } else {
                    Value::Null
                }
            }
        }
    }

    /// Get frame bounds (start, end) for current row
    /// Returns (inclusive start, exclusive end)
    fn get_frame_bounds(
        row_idx: usize,
        partition_size: usize,
        peer_groups: &[usize],
        frame: &FrameSpec,
    ) -> (usize, usize) {
        let start = match &frame.start {
            FrameBound::UnboundedPreceding => 0,
            FrameBound::CurrentRow => {
                match frame.frame_type {
                    FrameType::Rows => row_idx,
                    FrameType::Range | FrameType::Groups => {
                        // Start of current peer group
                        let group = peer_groups[row_idx];
                        peer_groups
                            .iter()
                            .position(|&g| g == group)
                            .unwrap_or(row_idx)
                    }
                }
            }
            FrameBound::Preceding(n) => {
                match frame.frame_type {
                    FrameType::Rows => row_idx.saturating_sub(*n as usize),
                    FrameType::Groups => {
                        // n groups preceding
                        let current_group = peer_groups[row_idx];
                        let target_group = current_group.saturating_sub(*n as usize);
                        peer_groups
                            .iter()
                            .position(|&g| g == target_group)
                            .unwrap_or(0)
                    }
                    FrameType::Range => row_idx.saturating_sub(*n as usize),
                }
            }
            FrameBound::Following(n) => match frame.frame_type {
                FrameType::Rows => (row_idx + *n as usize).min(partition_size),
                FrameType::Groups => {
                    let current_group = peer_groups[row_idx];
                    let target_group = current_group + *n as usize;
                    peer_groups
                        .iter()
                        .position(|&g| g >= target_group)
                        .unwrap_or(partition_size)
                }
                FrameType::Range => (row_idx + *n as usize).min(partition_size),
            },
            FrameBound::UnboundedFollowing => partition_size,
        };

        let end = match &frame.end {
            FrameBound::UnboundedFollowing => partition_size,
            FrameBound::CurrentRow => {
                match frame.frame_type {
                    FrameType::Rows => row_idx + 1,
                    FrameType::Range | FrameType::Groups => {
                        // End of current peer group (exclusive)
                        let group = peer_groups[row_idx];
                        peer_groups
                            .iter()
                            .position(|&g| g > group)
                            .unwrap_or(partition_size)
                    }
                }
            }
            FrameBound::Preceding(n) => match frame.frame_type {
                FrameType::Rows => row_idx.saturating_sub(*n as usize) + 1,
                FrameType::Groups => {
                    let current_group = peer_groups[row_idx];
                    let target_group = current_group.saturating_sub(*n as usize);
                    peer_groups
                        .iter()
                        .position(|&g| g > target_group)
                        .unwrap_or(partition_size)
                }
                FrameType::Range => row_idx.saturating_sub(*n as usize) + 1,
            },
            FrameBound::Following(n) => match frame.frame_type {
                FrameType::Rows => (row_idx + *n as usize + 1).min(partition_size),
                FrameType::Groups => {
                    let current_group = peer_groups[row_idx];
                    let target_group = current_group + *n as usize;
                    peer_groups
                        .iter()
                        .position(|&g| g > target_group)
                        .unwrap_or(partition_size)
                }
                FrameType::Range => (row_idx + *n as usize + 1).min(partition_size),
            },
            FrameBound::UnboundedPreceding => 0, // Invalid but handle gracefully
        };

        (
            start.min(partition_size),
            end.min(partition_size).max(start),
        )
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Integer(a), Value::Float(b)) => {
            (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal)
        }
        (Value::Float(a), Value::Integer(b)) => {
            a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal)
        }
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        _ => Ordering::Equal,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => (a - b).abs() < f64::EPSILON,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Boolean(a), Value::Boolean(b)) => a == b,
        (Value::Integer(a), Value::Float(b)) | (Value::Float(b), Value::Integer(a)) => {
            (*a as f64 - b).abs() < f64::EPSILON
        }
        (Value::Null, Value::Null) => true,
        _ => false,
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
            let next = Binding::one(Var::new(*k), v.clone());
            result = result.merge(&next).unwrap_or(result);
        }

        result
    }

    fn get_values(bindings: &[Binding], var: &str) -> Vec<i64> {
        let v = Var::new(var);
        bindings
            .iter()
            .filter_map(|b| b.get(&v))
            .filter_map(|v| match v {
                Value::Integer(i) => Some(*i),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_row_number() {
        let bindings = vec![
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("salary", Value::Integer(100)),
            ]),
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("salary", Value::Integer(200)),
            ]),
            make_binding(&[
                ("dept", Value::String("B".to_string())),
                ("salary", Value::Integer(150)),
            ]),
        ];

        let func = WindowFunc::new(
            WindowFuncType::RowNumber,
            Var::new("rn"),
            WindowDef::partition_by(vec![Var::new("dept")])
                .with_order_by(vec![WindowOrderBy::new(Var::new("salary"))]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);

        // Each partition should have row numbers starting from 1
        let rns = get_values(&result, "rn");
        assert_eq!(rns, vec![1, 2, 1]); // A: 1,2 and B: 1
    }

    #[test]
    fn test_rank_with_ties() {
        let bindings = vec![
            make_binding(&[("score", Value::Integer(100))]),
            make_binding(&[("score", Value::Integer(100))]), // Tie
            make_binding(&[("score", Value::Integer(90))]),
            make_binding(&[("score", Value::Integer(80))]),
        ];

        let func = WindowFunc::new(
            WindowFuncType::Rank,
            Var::new("rank"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("score")).desc()]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let ranks = get_values(&result, "rank");

        // 100 = rank 1, 100 = rank 1 (tie), 90 = rank 3 (skip 2), 80 = rank 4
        assert_eq!(ranks, vec![1, 1, 3, 4]);
    }

    #[test]
    fn test_dense_rank() {
        let bindings = vec![
            make_binding(&[("score", Value::Integer(100))]),
            make_binding(&[("score", Value::Integer(100))]),
            make_binding(&[("score", Value::Integer(90))]),
        ];

        let func = WindowFunc::new(
            WindowFuncType::DenseRank,
            Var::new("drank"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("score")).desc()]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let ranks = get_values(&result, "drank");

        // No gaps: 1, 1, 2
        assert_eq!(ranks, vec![1, 1, 2]);
    }

    #[test]
    fn test_ntile() {
        let bindings: Vec<Binding> = (1..=10)
            .map(|i| make_binding(&[("val", Value::Integer(i))]))
            .collect();

        let func = WindowFunc::new(
            WindowFuncType::Ntile(4),
            Var::new("bucket"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("val"))]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let buckets = get_values(&result, "bucket");

        // 10 rows into 4 buckets: 3, 3, 2, 2 rows per bucket
        assert_eq!(buckets, vec![1, 1, 1, 2, 2, 2, 3, 3, 4, 4]);
    }

    #[test]
    fn test_lag_lead() {
        let bindings: Vec<Binding> = (1..=5)
            .map(|i| make_binding(&[("val", Value::Integer(i))]))
            .collect();

        let lag_func = WindowFunc::new(
            WindowFuncType::Lag(Var::new("val"), 1, Some(Value::Integer(0))),
            Var::new("prev"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("val"))]),
        );

        let lead_func = WindowFunc::new(
            WindowFuncType::Lead(Var::new("val"), 1, Some(Value::Integer(0))),
            Var::new("next"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("val"))]),
        );

        let result = WindowExecutor::execute(bindings, &[lag_func, lead_func]);
        let prevs = get_values(&result, "prev");
        let nexts = get_values(&result, "next");

        assert_eq!(prevs, vec![0, 1, 2, 3, 4]); // LAG(val, 1, 0)
        assert_eq!(nexts, vec![2, 3, 4, 5, 0]); // LEAD(val, 1, 0)
    }

    #[test]
    fn test_running_sum() {
        let bindings: Vec<Binding> = (1..=5)
            .map(|i| make_binding(&[("val", Value::Integer(i))]))
            .collect();

        let func = WindowFunc::new(
            WindowFuncType::Aggregate("SUM".to_string(), Var::new("val")),
            Var::new("running_sum"),
            WindowDef::default()
                .with_order_by(vec![WindowOrderBy::new(Var::new("val"))])
                .with_frame(FrameSpec::running()),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let sums = get_values(&result, "running_sum");

        // Running sum: 1, 1+2=3, 1+2+3=6, ...
        assert_eq!(sums, vec![1, 3, 6, 10, 15]);
    }

    #[test]
    fn test_first_last_value() {
        let bindings: Vec<Binding> = (1..=5)
            .map(|i| make_binding(&[("val", Value::Integer(i))]))
            .collect();

        let first_func = WindowFunc::new(
            WindowFuncType::FirstValue(Var::new("val")),
            Var::new("first"),
            WindowDef::default()
                .with_order_by(vec![WindowOrderBy::new(Var::new("val"))])
                .with_frame(FrameSpec::entire_partition()),
        );

        let last_func = WindowFunc::new(
            WindowFuncType::LastValue(Var::new("val")),
            Var::new("last"),
            WindowDef::default()
                .with_order_by(vec![WindowOrderBy::new(Var::new("val"))])
                .with_frame(FrameSpec::entire_partition()),
        );

        let result = WindowExecutor::execute(bindings, &[first_func, last_func]);
        let firsts = get_values(&result, "first");
        let lasts = get_values(&result, "last");

        assert_eq!(firsts, vec![1, 1, 1, 1, 1]); // First value of partition
        assert_eq!(lasts, vec![5, 5, 5, 5, 5]); // Last value of partition
    }

    #[test]
    fn test_partitioned_sum() {
        let bindings = vec![
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("salary", Value::Integer(100)),
            ]),
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("salary", Value::Integer(200)),
            ]),
            make_binding(&[
                ("dept", Value::String("B".to_string())),
                ("salary", Value::Integer(150)),
            ]),
            make_binding(&[
                ("dept", Value::String("B".to_string())),
                ("salary", Value::Integer(250)),
            ]),
        ];

        let func = WindowFunc::new(
            WindowFuncType::Aggregate("SUM".to_string(), Var::new("salary")),
            Var::new("dept_total"),
            WindowDef::partition_by(vec![Var::new("dept")])
                .with_frame(FrameSpec::entire_partition()),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let totals = get_values(&result, "dept_total");

        // A: 100+200=300, B: 150+250=400
        assert_eq!(totals, vec![300, 300, 400, 400]);
    }

    #[test]
    fn test_window_preserves_input_order() {
        let bindings = vec![
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("seq", Value::Integer(1)),
            ]),
            make_binding(&[
                ("dept", Value::String("B".to_string())),
                ("seq", Value::Integer(1)),
            ]),
            make_binding(&[
                ("dept", Value::String("A".to_string())),
                ("seq", Value::Integer(2)),
            ]),
            make_binding(&[
                ("dept", Value::String("B".to_string())),
                ("seq", Value::Integer(2)),
            ]),
        ];

        let func = WindowFunc::new(
            WindowFuncType::RowNumber,
            Var::new("rn"),
            WindowDef::partition_by(vec![Var::new("dept")])
                .with_order_by(vec![WindowOrderBy::new(Var::new("seq"))]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);
        let dept_var = Var::new("dept");
        let depts: Vec<String> = result
            .iter()
            .filter_map(|b| b.get(&dept_var))
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(depts, vec!["A", "B", "A", "B"]);
        assert_eq!(get_values(&result, "rn"), vec![1, 1, 2, 2]);
    }

    #[test]
    fn test_percent_rank() {
        let bindings: Vec<Binding> = (1..=4)
            .map(|i| make_binding(&[("val", Value::Integer(i))]))
            .collect();

        let func = WindowFunc::new(
            WindowFuncType::PercentRank,
            Var::new("prank"),
            WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("val"))]),
        );

        let result = WindowExecutor::execute(bindings, &[func]);

        // Check percent ranks: 0, 0.333..., 0.666..., 1.0
        for (i, binding) in result.iter().enumerate() {
            if let Some(Value::Float(pr)) = binding.get(&Var::new("prank")) {
                let expected = i as f64 / 3.0;
                assert!(
                    (pr - expected).abs() < 0.001,
                    "Row {}: expected {}, got {}",
                    i,
                    expected,
                    pr
                );
            }
        }
    }
}
