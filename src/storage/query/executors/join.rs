//! JOIN Executor Algorithms
//!
//! Provides multiple join strategies for different scenarios:
//! - Hash Join: O(n+m) for equi-joins, best for large datasets
//! - Nested Loop Join: O(n*m) fallback, works with any condition
//! - Merge Join: O(n log n + m log m) for sorted inputs
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    JoinExecutor                              │
//! ├─────────────────────────────────────────────────────────────┤
//! │  ┌───────────┐  ┌───────────────┐  ┌─────────────────────┐  │
//! │  │ Hash Join │  │ Nested Loop   │  │   Merge Join        │  │
//! │  │  (fast)   │  │  (fallback)   │  │   (sorted)          │  │
//! │  └─────┬─────┘  └───────┬───────┘  └──────────┬──────────┘  │
//! │        │                │                      │             │
//! │        └────────────────┼──────────────────────┘             │
//! │                         ▼                                    │
//! │              ┌──────────────────────┐                        │
//! │              │   JoinPlanner        │                        │
//! │              │   (cost-based)       │                        │
//! │              └──────────────────────┘                        │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use super::super::engine::binding::{Binding, Value, Var};

// ============================================================================
// Join Types
// ============================================================================

/// Type of JOIN operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    /// INNER JOIN - only matching rows
    Inner,
    /// LEFT JOIN - all left rows, matching right
    Left,
    /// RIGHT JOIN - all right rows, matching left
    Right,
    /// CROSS JOIN - Cartesian product
    Cross,
    /// FULL OUTER JOIN - all rows from both
    FullOuter,
}

/// Join condition for filtering matches
#[derive(Debug, Clone)]
pub enum JoinCondition {
    /// Equality on columns: left.col = right.col
    Eq(Var, Var),
    /// Multiple equality conditions (AND)
    And(Vec<JoinCondition>),
    /// No condition (cross join)
    None,
}

impl JoinCondition {
    /// Get all left-side variables
    pub fn left_vars(&self) -> Vec<Var> {
        match self {
            JoinCondition::Eq(left, _) => vec![left.clone()],
            JoinCondition::And(conditions) => {
                conditions.iter().flat_map(|c| c.left_vars()).collect()
            }
            JoinCondition::None => Vec::new(),
        }
    }

    /// Get all right-side variables
    pub fn right_vars(&self) -> Vec<Var> {
        match self {
            JoinCondition::Eq(_, right) => vec![right.clone()],
            JoinCondition::And(conditions) => {
                conditions.iter().flat_map(|c| c.right_vars()).collect()
            }
            JoinCondition::None => Vec::new(),
        }
    }
}

// ============================================================================
// Join Algorithm Selection
// ============================================================================

/// Strategy to use for executing the join
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStrategy {
    /// Hash join - build hash table on smaller side
    Hash,
    /// Nested loop - iterate all combinations
    NestedLoop,
    /// Merge join - for pre-sorted inputs
    Merge,
}

/// Statistics for choosing join strategy
#[derive(Debug, Clone)]
pub struct JoinStats {
    pub left_cardinality: usize,
    pub right_cardinality: usize,
    pub left_sorted: bool,
    pub right_sorted: bool,
    pub condition_selectivity: f64,
}

impl Default for JoinStats {
    fn default() -> Self {
        Self {
            left_cardinality: 0,
            right_cardinality: 0,
            left_sorted: false,
            right_sorted: false,
            condition_selectivity: 1.0,
        }
    }
}

/// Choose optimal join strategy based on statistics
pub fn choose_strategy(stats: &JoinStats, condition: &JoinCondition) -> JoinStrategy {
    // Cross join always uses nested loop (no condition to hash on)
    if matches!(condition, JoinCondition::None) {
        return JoinStrategy::NestedLoop;
    }

    // If both sides are sorted on join keys, use merge join
    if stats.left_sorted && stats.right_sorted {
        return JoinStrategy::Merge;
    }

    // For very small tables, nested loop is fine
    let total = stats.left_cardinality * stats.right_cardinality;
    if total < 1000 {
        return JoinStrategy::NestedLoop;
    }

    // Default to hash join for equi-joins
    if matches!(condition, JoinCondition::Eq(_, _) | JoinCondition::And(_)) {
        return JoinStrategy::Hash;
    }

    JoinStrategy::NestedLoop
}

// ============================================================================
// Hash Join Implementation
// ============================================================================

/// Hash key for join matching
#[derive(Clone, PartialEq, Eq)]
struct HashKey(Vec<Option<Value>>);

impl Hash for HashKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for value in &self.0 {
            match value {
                Some(Value::String(s)) => {
                    1u8.hash(state);
                    s.hash(state);
                }
                Some(Value::Integer(i)) => {
                    2u8.hash(state);
                    i.hash(state);
                }
                Some(Value::Float(f)) => {
                    3u8.hash(state);
                    f.to_bits().hash(state);
                }
                Some(Value::Boolean(b)) => {
                    4u8.hash(state);
                    b.hash(state);
                }
                Some(Value::Uri(u)) => {
                    5u8.hash(state);
                    u.hash(state);
                }
                Some(Value::Node(n)) => {
                    6u8.hash(state);
                    n.hash(state);
                }
                Some(Value::Edge(e)) => {
                    7u8.hash(state);
                    e.hash(state);
                }
                Some(Value::Null) | None => {
                    0u8.hash(state);
                }
            }
        }
    }
}

/// Execute a hash join
pub fn hash_join(
    left: Vec<Binding>,
    right: Vec<Binding>,
    condition: &JoinCondition,
    join_type: JoinType,
) -> Vec<Binding> {
    let left_keys = condition.left_vars();
    let right_keys = condition.right_vars();

    if left_keys.is_empty() {
        // No keys means cross join behavior
        return nested_loop_join(left, right, condition, join_type);
    }

    // Build phase: build hash table on the smaller side
    let (build_side, probe_side, build_keys, probe_keys, is_left_build) =
        if left.len() <= right.len() {
            (&left, &right, &left_keys, &right_keys, true)
        } else {
            (&right, &left, &right_keys, &left_keys, false)
        };

    // Build hash table
    let mut hash_table: HashMap<HashKey, Vec<&Binding>> = HashMap::new();
    for binding in build_side {
        let key = extract_key(binding, build_keys);
        hash_table.entry(key).or_default().push(binding);
    }

    // Probe phase
    let mut results = Vec::new();
    let mut matched_build: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (probe_idx, probe_binding) in probe_side.iter().enumerate() {
        let key = extract_key(probe_binding, probe_keys);
        let matches = hash_table.get(&key);

        let mut had_match = false;
        if let Some(build_bindings) = matches {
            for (build_idx, &build_binding) in build_bindings.iter().enumerate() {
                had_match = true;

                // Remember which build rows were matched (for full outer)
                if matches!(join_type, JoinType::FullOuter) {
                    // We need to track by original index
                    let original_idx = build_side
                        .iter()
                        .position(|b| std::ptr::eq(b, build_binding));
                    if let Some(idx) = original_idx {
                        matched_build.insert(idx);
                    }
                }

                // Merge bindings
                let merged = if is_left_build {
                    merge_bindings(build_binding, probe_binding)
                } else {
                    merge_bindings(probe_binding, build_binding)
                };
                results.push(merged);
            }
        }

        // Handle outer joins - add probe side with nulls if no match
        if !had_match {
            match join_type {
                JoinType::Left if !is_left_build => {
                    // probe_side is left, need to include unmatched left rows
                    results.push(probe_binding.clone());
                }
                JoinType::Right if is_left_build => {
                    // probe_side is right, need to include unmatched right rows
                    results.push(probe_binding.clone());
                }
                JoinType::FullOuter => {
                    results.push(probe_binding.clone());
                }
                _ => {}
            }
        }
    }

    // For full outer join, add unmatched build side rows
    if matches!(join_type, JoinType::FullOuter) {
        for (idx, binding) in build_side.iter().enumerate() {
            if !matched_build.contains(&idx) {
                results.push((*binding).clone());
            }
        }
    }

    // Handle LEFT/RIGHT join for the build side
    match (join_type, is_left_build) {
        (JoinType::Left, true) => {
            // Build side is left, need to add unmatched left rows
            let mut all_left_matched: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            for binding in &results {
                // Check which left rows are in results
                for (idx, left_binding) in left.iter().enumerate() {
                    if bindings_match(binding, left_binding, &left_keys) {
                        all_left_matched.insert(idx);
                    }
                }
            }
            for (idx, binding) in left.iter().enumerate() {
                if !all_left_matched.contains(&idx) {
                    results.push(binding.clone());
                }
            }
        }
        (JoinType::Right, false) => {
            // Build side is right, need to add unmatched right rows
            let mut all_right_matched: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            for binding in &results {
                for (idx, right_binding) in right.iter().enumerate() {
                    if bindings_match(binding, right_binding, &right_keys) {
                        all_right_matched.insert(idx);
                    }
                }
            }
            for (idx, binding) in right.iter().enumerate() {
                if !all_right_matched.contains(&idx) {
                    results.push(binding.clone());
                }
            }
        }
        _ => {}
    }

    results
}

fn extract_key(binding: &Binding, vars: &[Var]) -> HashKey {
    HashKey(vars.iter().map(|v| binding.get(v).cloned()).collect())
}

fn bindings_match(a: &Binding, b: &Binding, keys: &[Var]) -> bool {
    keys.iter().all(|k| match (a.get(k), b.get(k)) {
        (Some(v1), Some(v2)) => v1 == v2,
        _ => false,
    })
}

// ============================================================================
// Nested Loop Join Implementation
// ============================================================================

/// Execute a nested loop join (O(n*m) but works with any condition)
pub fn nested_loop_join(
    left: Vec<Binding>,
    right: Vec<Binding>,
    condition: &JoinCondition,
    join_type: JoinType,
) -> Vec<Binding> {
    let mut results = Vec::new();
    let mut left_matched = vec![false; left.len()];
    let mut right_matched = vec![false; right.len()];

    for (left_idx, left_binding) in left.iter().enumerate() {
        let mut found_match = false;

        for (right_idx, right_binding) in right.iter().enumerate() {
            if check_condition(left_binding, right_binding, condition) {
                found_match = true;
                left_matched[left_idx] = true;
                right_matched[right_idx] = true;

                let merged = merge_bindings(left_binding, right_binding);
                results.push(merged);
            }
        }

        // LEFT JOIN: include unmatched left rows
        if !found_match && matches!(join_type, JoinType::Left | JoinType::FullOuter) {
            results.push(left_binding.clone());
        }
    }

    // RIGHT JOIN / FULL OUTER: include unmatched right rows
    if matches!(join_type, JoinType::Right | JoinType::FullOuter) {
        for (right_idx, right_binding) in right.iter().enumerate() {
            if !right_matched[right_idx] {
                results.push(right_binding.clone());
            }
        }
    }

    results
}

fn check_condition(left: &Binding, right: &Binding, condition: &JoinCondition) -> bool {
    match condition {
        JoinCondition::Eq(left_var, right_var) => {
            match (left.get(left_var), right.get(right_var)) {
                (Some(l), Some(r)) => l == r,
                _ => false,
            }
        }
        JoinCondition::And(conditions) => {
            conditions.iter().all(|c| check_condition(left, right, c))
        }
        JoinCondition::None => true,
    }
}

// ============================================================================
// Merge Join Implementation
// ============================================================================

/// Execute a merge join (for sorted inputs)
pub fn merge_join(
    left: Vec<Binding>,
    right: Vec<Binding>,
    condition: &JoinCondition,
    join_type: JoinType,
) -> Vec<Binding> {
    // For simplicity, fall back to hash join if not simple equality
    // A full merge join would require sorted inputs and careful cursor management
    let left_keys = condition.left_vars();
    let right_keys = condition.right_vars();

    if left_keys.is_empty() || right_keys.is_empty() {
        return nested_loop_join(left, right, condition, join_type);
    }

    // Sort both sides by join keys
    let mut left_sorted = left;
    let mut right_sorted = right;

    left_sorted.sort_by(|a, b| compare_by_keys(a, b, &left_keys));
    right_sorted.sort_by(|a, b| compare_by_keys(a, b, &right_keys));

    let mut results = Vec::new();
    let mut left_idx = 0;
    let mut right_idx = 0;
    let mut left_matched = vec![false; left_sorted.len()];
    let mut right_matched = vec![false; right_sorted.len()];

    while left_idx < left_sorted.len() && right_idx < right_sorted.len() {
        let left_key = extract_key(&left_sorted[left_idx], &left_keys);
        let right_key = extract_key(&right_sorted[right_idx], &right_keys);

        match compare_keys(&left_key, &right_key) {
            std::cmp::Ordering::Less => {
                // Left row has no match
                if matches!(join_type, JoinType::Left | JoinType::FullOuter)
                    && !left_matched[left_idx]
                {
                    results.push(left_sorted[left_idx].clone());
                }
                left_idx += 1;
            }
            std::cmp::Ordering::Greater => {
                // Right row has no match
                if matches!(join_type, JoinType::Right | JoinType::FullOuter)
                    && !right_matched[right_idx]
                {
                    results.push(right_sorted[right_idx].clone());
                }
                right_idx += 1;
            }
            std::cmp::Ordering::Equal => {
                // Match found - need to handle duplicates
                let match_start_right = right_idx;

                // Find all matching right rows
                while right_idx < right_sorted.len() {
                    let current_right_key = extract_key(&right_sorted[right_idx], &right_keys);
                    if compare_keys(&left_key, &current_right_key) != std::cmp::Ordering::Equal {
                        break;
                    }

                    left_matched[left_idx] = true;
                    right_matched[right_idx] = true;

                    let merged = merge_bindings(&left_sorted[left_idx], &right_sorted[right_idx]);
                    results.push(merged);
                    right_idx += 1;
                }

                // Check for more left rows with same key
                left_idx += 1;
                while left_idx < left_sorted.len() {
                    let current_left_key = extract_key(&left_sorted[left_idx], &left_keys);
                    if compare_keys(&current_left_key, &left_key) != std::cmp::Ordering::Equal {
                        break;
                    }

                    // Match with all right rows in the group
                    for right_row in right_sorted.iter().take(right_idx).skip(match_start_right) {
                        left_matched[left_idx] = true;
                        let merged = merge_bindings(&left_sorted[left_idx], right_row);
                        results.push(merged);
                    }
                    left_idx += 1;
                }

                // Reset right index for next left group
                right_idx = match_start_right;
                if left_idx >= left_sorted.len() || {
                    let next_left_key = extract_key(
                        &left_sorted[left_idx.min(left_sorted.len() - 1)],
                        &left_keys,
                    );
                    compare_keys(&next_left_key, &left_key) != std::cmp::Ordering::Equal
                } {
                    // Advance past the matching right rows
                    while right_idx < right_sorted.len() {
                        let current_right_key = extract_key(&right_sorted[right_idx], &right_keys);
                        if compare_keys(&left_key, &current_right_key) != std::cmp::Ordering::Equal
                        {
                            break;
                        }
                        right_idx += 1;
                    }
                }
            }
        }
    }

    // Handle remaining unmatched rows
    while left_idx < left_sorted.len() {
        if matches!(join_type, JoinType::Left | JoinType::FullOuter) && !left_matched[left_idx] {
            results.push(left_sorted[left_idx].clone());
        }
        left_idx += 1;
    }

    while right_idx < right_sorted.len() {
        if matches!(join_type, JoinType::Right | JoinType::FullOuter) && !right_matched[right_idx] {
            results.push(right_sorted[right_idx].clone());
        }
        right_idx += 1;
    }

    results
}

fn compare_by_keys(a: &Binding, b: &Binding, keys: &[Var]) -> std::cmp::Ordering {
    for key in keys {
        match (a.get(key), b.get(key)) {
            (Some(av), Some(bv)) => {
                let cmp = compare_values(av, bv);
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (None, None) => {}
        }
    }
    std::cmp::Ordering::Equal
}

fn compare_keys(a: &HashKey, b: &HashKey) -> std::cmp::Ordering {
    for (av, bv) in a.0.iter().zip(b.0.iter()) {
        match (av, bv) {
            (Some(av), Some(bv)) => {
                let cmp = compare_values(av, bv);
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (None, None) => {}
        }
    }
    std::cmp::Ordering::Equal
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Uri(a), Value::Uri(b)) => a.cmp(b),
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        (Value::Node(a), Value::Node(b)) => a.cmp(b),
        (Value::Edge(a), Value::Edge(b)) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    }
}

// ============================================================================
// Binding Merge
// ============================================================================

/// Merge two bindings, preferring values from left
fn merge_bindings(left: &Binding, right: &Binding) -> Binding {
    // Start with left binding, then try to merge right
    // The Binding::merge method handles this properly
    if let Some(merged) = left.merge(right) {
        merged
    } else {
        // If conflict, just return left (shouldn't happen in proper joins)
        left.clone()
    }
}

// ============================================================================
// Unified Join Executor
// ============================================================================

/// Execute a join operation using the optimal strategy
pub fn execute_join(
    left: Vec<Binding>,
    right: Vec<Binding>,
    condition: JoinCondition,
    join_type: JoinType,
    stats: Option<JoinStats>,
) -> Vec<Binding> {
    // Determine strategy
    let actual_stats = stats.unwrap_or(JoinStats {
        left_cardinality: left.len(),
        right_cardinality: right.len(),
        left_sorted: false,
        right_sorted: false,
        condition_selectivity: 1.0,
    });

    let strategy = choose_strategy(&actual_stats, &condition);

    match strategy {
        JoinStrategy::Hash => hash_join(left, right, &condition, join_type),
        JoinStrategy::NestedLoop => nested_loop_join(left, right, &condition, join_type),
        JoinStrategy::Merge => merge_join(left, right, &condition, join_type),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_binding(pairs: &[(&str, &str)]) -> Binding {
        // Build the binding using Binding::one and then merge
        if pairs.is_empty() {
            return Binding::empty();
        }

        let mut result = Binding::one(Var::new(pairs[0].0), Value::String(pairs[0].1.to_string()));

        for (k, v) in pairs.iter().skip(1) {
            let next = Binding::one(Var::new(*k), Value::String(v.to_string()));
            result = result.merge(&next).unwrap_or(result);
        }

        result
    }

    #[test]
    fn test_inner_join() {
        let left = vec![
            make_binding(&[("id", "1"), ("name", "Alice")]),
            make_binding(&[("id", "2"), ("name", "Bob")]),
            make_binding(&[("id", "3"), ("name", "Charlie")]),
        ];

        let right = vec![
            make_binding(&[("user_id", "1"), ("score", "100")]),
            make_binding(&[("user_id", "2"), ("score", "90")]),
            make_binding(&[("user_id", "4"), ("score", "80")]),
        ];

        let condition = JoinCondition::Eq(Var::new("id"), Var::new("user_id"));
        let results = execute_join(left, right, condition, JoinType::Inner, None);

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|b| b.get(&Var::new("name")) == Some(&Value::String("Alice".to_string()))));
        assert!(results
            .iter()
            .any(|b| b.get(&Var::new("name")) == Some(&Value::String("Bob".to_string()))));
    }

    #[test]
    fn test_left_join() {
        let left = vec![
            make_binding(&[("id", "1"), ("name", "Alice")]),
            make_binding(&[("id", "2"), ("name", "Bob")]),
            make_binding(&[("id", "3"), ("name", "Charlie")]),
        ];

        let right = vec![make_binding(&[("user_id", "1"), ("score", "100")])];

        let condition = JoinCondition::Eq(Var::new("id"), Var::new("user_id"));
        let results = execute_join(left, right, condition, JoinType::Left, None);

        assert_eq!(results.len(), 3); // All left rows
        assert!(results
            .iter()
            .any(|b| b.get(&Var::new("name")) == Some(&Value::String("Charlie".to_string()))));
    }

    #[test]
    fn test_right_join() {
        let left = vec![make_binding(&[("id", "1"), ("name", "Alice")])];

        let right = vec![
            make_binding(&[("user_id", "1"), ("score", "100")]),
            make_binding(&[("user_id", "2"), ("score", "90")]),
            make_binding(&[("user_id", "3"), ("score", "80")]),
        ];

        let condition = JoinCondition::Eq(Var::new("id"), Var::new("user_id"));
        let results = execute_join(left, right, condition, JoinType::Right, None);

        assert_eq!(results.len(), 3); // All right rows
    }

    #[test]
    fn test_cross_join() {
        let left = vec![make_binding(&[("a", "1")]), make_binding(&[("a", "2")])];

        let right = vec![
            make_binding(&[("b", "x")]),
            make_binding(&[("b", "y")]),
            make_binding(&[("b", "z")]),
        ];

        let results = execute_join(left, right, JoinCondition::None, JoinType::Cross, None);

        assert_eq!(results.len(), 6); // 2 * 3 = 6
    }

    #[test]
    fn test_merge_join() {
        let left = vec![
            make_binding(&[("id", "1"), ("name", "Alice")]),
            make_binding(&[("id", "2"), ("name", "Bob")]),
        ];

        let right = vec![
            make_binding(&[("id", "1"), ("dept", "Eng")]),
            make_binding(&[("id", "2"), ("dept", "Sales")]),
        ];

        let condition = JoinCondition::Eq(Var::new("id"), Var::new("id"));
        let stats = JoinStats {
            left_cardinality: 2,
            right_cardinality: 2,
            left_sorted: true,
            right_sorted: true,
            condition_selectivity: 1.0,
        };

        let results = execute_join(left, right, condition, JoinType::Inner, Some(stats));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_strategy_selection() {
        // Small tables -> nested loop
        let stats = JoinStats {
            left_cardinality: 10,
            right_cardinality: 10,
            left_sorted: false,
            right_sorted: false,
            condition_selectivity: 1.0,
        };
        assert_eq!(
            choose_strategy(&stats, &JoinCondition::Eq(Var::new("a"), Var::new("b"))),
            JoinStrategy::NestedLoop
        );

        // Large tables -> hash join
        let stats = JoinStats {
            left_cardinality: 10000,
            right_cardinality: 10000,
            left_sorted: false,
            right_sorted: false,
            condition_selectivity: 1.0,
        };
        assert_eq!(
            choose_strategy(&stats, &JoinCondition::Eq(Var::new("a"), Var::new("b"))),
            JoinStrategy::Hash
        );

        // Sorted tables -> merge join
        let stats = JoinStats {
            left_cardinality: 1000,
            right_cardinality: 1000,
            left_sorted: true,
            right_sorted: true,
            condition_selectivity: 1.0,
        };
        assert_eq!(
            choose_strategy(&stats, &JoinCondition::Eq(Var::new("a"), Var::new("b"))),
            JoinStrategy::Merge
        );
    }
}
