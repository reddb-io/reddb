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

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};

use super::super::engine::binding::{Binding, Value, Var};
use super::value_compare::total_compare_values;

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
pub(crate) struct HashKey(Vec<Option<Value>>);

impl Hash for HashKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for value in &self.0 {
            hash_key_value(value.as_ref(), state);
        }
    }
}

/// Hash one join-key column. `NULL` and an absent binding hash alike (tag 0) —
/// they are also equal-by-absence nowhere, so a null key still finds its bucket
/// and is then rejected by the element-wise equality check.
fn hash_key_value<H: Hasher>(value: Option<&Value>, state: &mut H) {
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

/// Hash a row's join key *in place* — without materializing an owned `HashKey`.
/// This is the probe-side hot path: one hash per probe row, zero allocations.
///
/// The caller supplies a `RandomState` (one per join) so bucket hashes stay
/// randomly seeded like the `HashMap<HashKey, _>` this replaced — a fixed-key
/// hasher would let precomputed collisions degrade the build to O(n²).
pub(crate) fn key_hash(state: &RandomState, binding: &Binding, vars: &[Var]) -> u64 {
    let mut hasher = state.build_hasher();
    for var in vars {
        hash_key_value(binding.get(var), &mut hasher);
    }
    hasher.finish()
}

/// Element-wise equality between a stored build key and a probe row's key,
/// compared through references so the probe key is never cloned. Identical to
/// `extract_key(binding, vars) == *key`, including `None`/`NULL` columns.
pub(crate) fn key_matches(key: &HashKey, binding: &Binding, vars: &[Var]) -> bool {
    key.0.len() == vars.len()
        && key
            .0
            .iter()
            .zip(vars)
            .all(|(stored, var)| stored.as_ref() == binding.get(var))
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

    // Build hash table, bucketed by the key's precomputed hash. The build side
    // owns its keys; collisions inside a bucket are resolved by element-wise
    // equality, exactly as the `HashMap<HashKey, _>` it replaces did. Keeping
    // the hash explicit is what lets the probe side look a row up *without*
    // cloning its key values (see `key_hash` / `key_matches`).
    #[allow(clippy::type_complexity)]
    let mut hash_table: HashMap<u64, Vec<(HashKey, Vec<&Binding>)>> = HashMap::new();
    let hash_state = RandomState::new();
    for binding in build_side {
        let hash = key_hash(&hash_state, binding, build_keys);
        let bucket = hash_table.entry(hash).or_default();
        match bucket
            .iter_mut()
            .find(|(key, _)| key_matches(key, binding, build_keys))
        {
            Some((_, bindings)) => bindings.push(binding),
            None => bucket.push((extract_key(binding, build_keys), vec![binding])),
        }
    }

    // Probe phase
    let mut results = Vec::new();
    let mut matched_build: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (probe_idx, probe_binding) in probe_side.iter().enumerate() {
        // Probe with a borrowed key: hash the row's join columns in place, then
        // confirm the match element-wise. No `Value` is cloned per probe row.
        let matches = hash_table
            .get(&key_hash(&hash_state, probe_binding, probe_keys))
            .and_then(|bucket: &Vec<(HashKey, Vec<&Binding>)>| {
                bucket
                    .iter()
                    .find(|(key, _)| key_matches(key, probe_binding, probe_keys))
                    .map(|(_, bindings)| bindings)
            });

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

pub(crate) fn extract_key(binding: &Binding, vars: &[Var]) -> HashKey {
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
                let cmp = total_compare_values(av, bv);
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
                let cmp = total_compare_values(av, bv);
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
            let next = Binding::one(Var::new(k), Value::String(v.to_string()));
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

    // ===================== borrowed probe-key equivalence (#2013) ============

    /// Bind `k` to the given value (or leave `k` unbound when `None`), plus a
    /// side-local tag var so merged rows stay distinguishable and never conflict.
    fn key_row(k: Option<Value>, tag_var: &str, tag: &str) -> Binding {
        let row = Binding::one(Var::new(tag_var), Value::String(tag.to_string()));
        match k {
            Some(value) => row
                .merge(&Binding::one(Var::new("k"), value))
                .expect("no conflicting binding"),
            None => row,
        }
    }

    fn left_row(k: Option<Value>, tag: &str) -> Binding {
        key_row(k, "lt", tag)
    }

    fn right_row(k: Option<Value>, tag: &str) -> Binding {
        key_row(k, "rt", tag)
    }

    /// Order-independent fingerprint of a joined row (`Binding` is backed by a
    /// `HashMap`, so its `Debug` order is not stable).
    fn fingerprint(binding: &Binding) -> String {
        let field = |name: &str| match binding.get(&Var::new(name)) {
            Some(value) => format!("{value:?}"),
            None => "-".to_string(),
        };
        format!("lt={} rt={} k={}", field("lt"), field("rt"), field("k"))
    }

    /// Reference semantics for an inner equi-join: every (left, right) pair
    /// whose *owned* join keys compare equal. This is deliberately independent
    /// of the hash table under test.
    fn reference_inner_matches(left: &[Binding], right: &[Binding], keys: &[Var]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in left {
            for r in right {
                if extract_key(l, keys) == extract_key(r, keys) {
                    out.push(fingerprint(&merge_bindings(l, r)));
                }
            }
        }
        out.sort();
        out
    }

    /// An absent column and an explicit `NULL` hash to the same bucket, so the
    /// probe path *must* resolve the collision by element-wise equality — which
    /// keeps them distinct, exactly as the old owned-`HashKey` map did.
    #[test]
    fn null_and_absent_keys_collide_in_hash_but_stay_distinct_in_equality() {
        let keys = [Var::new("k")];
        let null_row = left_row(Some(Value::Null), "null");
        let absent_row = left_row(None, "absent");

        let state = RandomState::new();
        assert_eq!(
            key_hash(&state, &null_row, &keys),
            key_hash(&state, &absent_row, &keys),
            "NULL and an absent column are expected to share a hash bucket"
        );
        assert!(key_matches(
            &extract_key(&null_row, &keys),
            &null_row,
            &keys
        ));
        assert!(!key_matches(
            &extract_key(&null_row, &keys),
            &absent_row,
            &keys
        ));
        assert!(!key_matches(
            &extract_key(&absent_row, &keys),
            &null_row,
            &keys
        ));
    }

    /// The borrowed probe key produces exactly the matches the owned key does —
    /// over string keys, explicit `NULL`s, absent columns, and rows that share a
    /// hash bucket while holding different values.
    #[test]
    fn hash_join_matches_are_identical_to_the_owned_key_reference() {
        let left = vec![
            left_row(Some(Value::String("a".into())), "L-a"),
            left_row(Some(Value::Integer(1)), "L-1"),
            left_row(Some(Value::Null), "L-null"),
            left_row(None, "L-absent"),
            left_row(Some(Value::String("a".into())), "L-a2"),
        ];
        let right = vec![
            right_row(Some(Value::String("a".into())), "R-a"),
            right_row(Some(Value::Null), "R-null"),
            right_row(None, "R-absent"),
            right_row(Some(Value::Boolean(true)), "R-true"),
            right_row(Some(Value::Integer(1)), "R-1"),
            right_row(Some(Value::Integer(1)), "R-1b"),
        ];
        let keys = [Var::new("k")];
        let condition = JoinCondition::Eq(Var::new("k"), Var::new("k"));

        // Cover both build-side choices: `left` is the smaller side (built), the
        // swapped call builds on the right input, and equal-size inputs exercise
        // the `<=` tie rule.
        for (l, r) in [
            (left.clone(), right.clone()),
            (right.clone(), left.clone()),
            (left.clone(), right[..left.len()].to_vec()),
        ] {
            let expected = reference_inner_matches(&l, &r, &keys);

            let mut actual: Vec<String> = hash_join(l, r, &condition, JoinType::Inner)
                .iter()
                .map(fingerprint)
                .collect();
            actual.sort();

            assert_eq!(
                actual, expected,
                "hash join diverged from the owned-key reference"
            );
        }
    }
}
