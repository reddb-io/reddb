//! Set Operations Executor
//!
//! Provides UNION, INTERSECT, and EXCEPT operations for query results.
//!
//! # Operations
//!
//! - **UNION**: Combines results from two queries (with deduplication)
//! - **UNION ALL**: Combines results without deduplication
//! - **INTERSECT**: Returns only rows that appear in both queries
//! - **EXCEPT**: Returns rows from the first query that don't appear in the second
//!
//! # Implementation
//!
//! Uses hash-based algorithms for O(n+m) performance where n and m are
//! the sizes of the input result sets.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::super::engine::binding::{Binding, Value};

/// Type of set operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOpType {
    /// Union with deduplication
    Union,
    /// Union without deduplication
    UnionAll,
    /// Intersection
    Intersect,
    /// Set difference (EXCEPT)
    Except,
}

/// Statistics for set operations
#[derive(Debug, Clone, Default)]
pub struct SetOpStats {
    /// Size of left input
    pub left_size: usize,
    /// Size of right input
    pub right_size: usize,
    /// Size of result
    pub result_size: usize,
    /// Number of duplicates removed (for UNION)
    pub duplicates_removed: usize,
}

/// Execute a set operation on two binding vectors
pub fn execute_set_op(
    left: Vec<Binding>,
    right: Vec<Binding>,
    op_type: SetOpType,
) -> (Vec<Binding>, SetOpStats) {
    let left_size = left.len();
    let right_size = right.len();

    let (result, duplicates_removed) = match op_type {
        SetOpType::Union => set_union(left, right, true),
        SetOpType::UnionAll => set_union(left, right, false),
        SetOpType::Intersect => (set_intersect(left, right), 0),
        SetOpType::Except => (set_except(left, right), 0),
    };

    let stats = SetOpStats {
        left_size,
        right_size,
        result_size: result.len(),
        duplicates_removed,
    };

    (result, stats)
}

/// UNION operation
///
/// If deduplicate is true, removes duplicate rows (UNION DISTINCT).
/// If deduplicate is false, keeps all rows (UNION ALL).
pub fn set_union(
    left: Vec<Binding>,
    right: Vec<Binding>,
    deduplicate: bool,
) -> (Vec<Binding>, usize) {
    if !deduplicate {
        // UNION ALL - just concatenate
        let mut result = left;
        result.extend(right);
        return (result, 0);
    }

    // UNION with deduplication
    let mut seen: HashSet<u64> = HashSet::new();
    let mut result: Vec<Binding> = Vec::new();
    let mut duplicates = 0;

    for binding in left.into_iter().chain(right) {
        let hash = binding_hash(&binding);
        if seen.insert(hash) {
            result.push(binding);
        } else {
            duplicates += 1;
        }
    }

    (result, duplicates)
}

/// INTERSECT operation
///
/// Returns only rows that appear in both left and right.
pub fn set_intersect(left: Vec<Binding>, right: Vec<Binding>) -> Vec<Binding> {
    // Build hash set of right side
    let right_hashes: HashSet<u64> = right.iter().map(binding_hash).collect();

    // Filter left to only include those in right
    let mut seen: HashSet<u64> = HashSet::new();
    let mut result: Vec<Binding> = Vec::new();

    for binding in left {
        let hash = binding_hash(&binding);
        if right_hashes.contains(&hash) && seen.insert(hash) {
            result.push(binding);
        }
    }

    result
}

/// EXCEPT operation (set difference)
///
/// Returns rows from left that don't appear in right.
pub fn set_except(left: Vec<Binding>, right: Vec<Binding>) -> Vec<Binding> {
    // Build hash set of right side
    let right_hashes: HashSet<u64> = right.iter().map(binding_hash).collect();

    // Filter left to only include those NOT in right
    let mut seen: HashSet<u64> = HashSet::new();
    let mut result: Vec<Binding> = Vec::new();

    for binding in left {
        let hash = binding_hash(&binding);
        if !right_hashes.contains(&hash) && seen.insert(hash) {
            result.push(binding);
        }
    }

    result
}

/// Compute a deterministic hash for a binding
fn binding_hash(binding: &Binding) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Get sorted keys for deterministic ordering
    let mut vars: Vec<_> = binding.all_vars();
    vars.sort_by_key(|v| v.name());

    for var in vars {
        var.name().hash(&mut hasher);
        if let Some(value) = binding.get(var) {
            hash_value(value, &mut hasher);
        } else {
            "unbound".hash(&mut hasher);
        }
    }

    hasher.finish()
}

fn hash_value(value: &Value, hasher: &mut DefaultHasher) {
    match value {
        Value::Node(id) => {
            "node".hash(hasher);
            id.hash(hasher);
        }
        Value::Edge(id) => {
            "edge".hash(hasher);
            id.hash(hasher);
        }
        Value::String(s) => {
            "string".hash(hasher);
            s.hash(hasher);
        }
        Value::Integer(i) => {
            "int".hash(hasher);
            i.hash(hasher);
        }
        Value::Float(f) => {
            "float".hash(hasher);
            f.to_bits().hash(hasher);
        }
        Value::Boolean(b) => {
            "bool".hash(hasher);
            b.hash(hasher);
        }
        Value::Uri(u) => {
            "uri".hash(hasher);
            u.hash(hasher);
        }
        Value::Null => {
            "null".hash(hasher);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::super::engine::binding::Var;
    use super::*;

    fn make_binding(pairs: &[(&str, &str)]) -> Binding {
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
    fn test_union_all() {
        let left = vec![make_binding(&[("x", "a")]), make_binding(&[("x", "b")])];
        let right = vec![make_binding(&[("x", "b")]), make_binding(&[("x", "c")])];

        let (result, stats) = execute_set_op(left, right, SetOpType::UnionAll);
        assert_eq!(result.len(), 4);
        assert_eq!(stats.duplicates_removed, 0);
    }

    #[test]
    fn test_union_distinct() {
        let left = vec![make_binding(&[("x", "a")]), make_binding(&[("x", "b")])];
        let right = vec![make_binding(&[("x", "b")]), make_binding(&[("x", "c")])];

        let (result, stats) = execute_set_op(left, right, SetOpType::Union);
        assert_eq!(result.len(), 3); // a, b, c
        assert_eq!(stats.duplicates_removed, 1); // one 'b' removed
    }

    #[test]
    fn test_intersect() {
        let left = vec![
            make_binding(&[("x", "a")]),
            make_binding(&[("x", "b")]),
            make_binding(&[("x", "c")]),
        ];
        let right = vec![
            make_binding(&[("x", "b")]),
            make_binding(&[("x", "c")]),
            make_binding(&[("x", "d")]),
        ];

        let (result, stats) = execute_set_op(left, right, SetOpType::Intersect);
        assert_eq!(result.len(), 2); // b, c
        assert_eq!(stats.left_size, 3);
        assert_eq!(stats.right_size, 3);
    }

    #[test]
    fn test_except() {
        let left = vec![
            make_binding(&[("x", "a")]),
            make_binding(&[("x", "b")]),
            make_binding(&[("x", "c")]),
        ];
        let right = vec![make_binding(&[("x", "b")])];

        let (result, stats) = execute_set_op(left, right, SetOpType::Except);
        assert_eq!(result.len(), 2); // a, c
        assert_eq!(stats.left_size, 3);
        assert_eq!(stats.right_size, 1);
    }

    #[test]
    fn test_intersect_empty() {
        let left = vec![make_binding(&[("x", "a")])];
        let right = vec![make_binding(&[("x", "b")])];

        let (result, _) = execute_set_op(left, right, SetOpType::Intersect);
        assert!(result.is_empty());
    }

    #[test]
    fn test_except_complete() {
        let left = vec![make_binding(&[("x", "a")])];
        let right = vec![make_binding(&[("x", "a")])];

        let (result, _) = execute_set_op(left, right, SetOpType::Except);
        assert!(result.is_empty());
    }
}
