//! Subquery Executor
//!
//! Provides support for nested queries within SQL expressions.
//!
//! # Subquery Types
//!
//! - **Scalar**: Single-value subquery `x = (SELECT max(y) FROM ...)`
//! - **EXISTS**: Existence check `EXISTS (SELECT * FROM ... WHERE ...)`
//! - **IN**: Set membership `x IN (SELECT y FROM ...)`
//! - **NOT IN**: Set non-membership
//! - **ANY/ALL**: Comparison with set `x > ANY (SELECT ...)`
//!
//! # Correlation
//!
//! - **Correlated**: References outer query columns, evaluated per row
//! - **Non-correlated**: Independent, can be evaluated once and cached
//!
//! # Optimization
//!
//! - Non-correlated subqueries use once-only evaluation
//! - IN subqueries build hash index for O(1) lookups
//! - EXISTS short-circuits on first match

use std::collections::HashSet;

use super::super::engine::binding::{Binding, Value, Var};
use super::value_compare::{partial_compare_values, values_equal};

// ============================================================================
// Subquery Types
// ============================================================================

/// Type of subquery
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubqueryType {
    /// Scalar subquery returning single value
    /// Example: `WHERE x = (SELECT max(y) FROM t)`
    Scalar,

    /// EXISTS check - true if any rows returned
    /// Example: `WHERE EXISTS (SELECT * FROM t WHERE ...)`
    Exists,

    /// NOT EXISTS check
    NotExists,

    /// IN membership test
    /// Example: `WHERE x IN (SELECT y FROM t)`
    In,

    /// NOT IN membership test
    NotIn,

    /// ANY comparison (at least one match)
    /// Example: `WHERE x > ANY (SELECT y FROM t)`
    Any(CompareOp),

    /// ALL comparison (all must match)
    /// Example: `WHERE x > ALL (SELECT y FROM t)`
    All(CompareOp),
}

/// Comparison operator for ANY/ALL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

// ============================================================================
// Subquery Definition
// ============================================================================

/// A subquery definition
#[derive(Debug, Clone)]
pub struct SubqueryDef {
    /// Unique identifier for this subquery
    pub id: usize,

    /// Type of subquery
    pub subquery_type: SubqueryType,

    /// Variable to compare against (for IN, ANY, ALL)
    pub compare_var: Option<Var>,

    /// Result variable name (for scalar subqueries)
    pub result_var: Option<Var>,

    /// Variables referenced from outer query (for correlation detection)
    pub outer_refs: Vec<Var>,

    /// Whether this subquery is correlated (references outer variables)
    pub is_correlated: bool,
}

impl SubqueryDef {
    /// Create a scalar subquery
    pub fn scalar(id: usize, result_var: Var) -> Self {
        Self {
            id,
            subquery_type: SubqueryType::Scalar,
            compare_var: None,
            result_var: Some(result_var),
            outer_refs: Vec::new(),
            is_correlated: false,
        }
    }

    /// Create an EXISTS subquery
    pub fn exists(id: usize, negated: bool) -> Self {
        Self {
            id,
            subquery_type: if negated {
                SubqueryType::NotExists
            } else {
                SubqueryType::Exists
            },
            compare_var: None,
            result_var: None,
            outer_refs: Vec::new(),
            is_correlated: false,
        }
    }

    /// Create an IN subquery
    pub fn in_list(id: usize, compare_var: Var, negated: bool) -> Self {
        Self {
            id,
            subquery_type: if negated {
                SubqueryType::NotIn
            } else {
                SubqueryType::In
            },
            compare_var: Some(compare_var),
            result_var: None,
            outer_refs: Vec::new(),
            is_correlated: false,
        }
    }

    /// Mark as correlated with outer references
    pub fn with_outer_refs(mut self, refs: Vec<Var>) -> Self {
        self.outer_refs = refs;
        self.is_correlated = !self.outer_refs.is_empty();
        self
    }
}

// ============================================================================
// Subquery Result Cache
// ============================================================================

/// Cached result of a non-correlated subquery
#[derive(Debug, Clone)]
pub enum SubqueryCache {
    /// Not yet evaluated
    Unevaluated,

    /// Scalar result
    Scalar(Option<Value>),

    /// Boolean result (EXISTS)
    Boolean(bool),

    /// Set of values (for IN)
    ValueSet(HashSet<ValueHash>),

    /// Ordered list of values (for ANY/ALL with ordering)
    ValueList(Vec<Value>),
}

/// Hashable wrapper for Value (for HashSet storage)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValueHash(String);

impl From<&Value> for ValueHash {
    fn from(v: &Value) -> Self {
        ValueHash(value_to_key(v))
    }
}

fn value_to_key(value: &Value) -> String {
    match value {
        Value::Node(id) => format!("N:{}", id),
        Value::Edge(id) => format!("E:{}", id),
        Value::String(s) => format!("S:{}", s),
        Value::Integer(i) => format!("I:{}", i),
        Value::Float(f) => format!("F:{}", f.to_bits()),
        Value::Boolean(b) => format!("B:{}", b),
        Value::Uri(u) => format!("U:{}", u),
        Value::Null => "NULL".to_string(),
    }
}

// ============================================================================
// Subquery Executor
// ============================================================================

/// Subquery executor handles evaluation of nested queries
pub struct SubqueryExecutor {
    /// Cache for non-correlated subqueries
    cache: Vec<SubqueryCache>,
}

impl SubqueryExecutor {
    /// Create new executor with space for n subqueries
    pub fn new(num_subqueries: usize) -> Self {
        Self {
            cache: vec![SubqueryCache::Unevaluated; num_subqueries],
        }
    }

    /// Evaluate an EXISTS subquery
    pub fn eval_exists<F>(
        &mut self,
        def: &SubqueryDef,
        outer_binding: &Binding,
        execute_subquery: F,
    ) -> bool
    where
        F: FnOnce(&Binding) -> Vec<Binding>,
    {
        let negated = matches!(def.subquery_type, SubqueryType::NotExists);

        // Check cache for non-correlated
        if !def.is_correlated {
            if let SubqueryCache::Boolean(result) = &self.cache[def.id] {
                return if negated { !*result } else { *result };
            }
        }

        // Execute subquery
        let results = execute_subquery(outer_binding);
        let exists = !results.is_empty();

        // Cache if non-correlated
        if !def.is_correlated {
            self.cache[def.id] = SubqueryCache::Boolean(exists);
        }

        if negated {
            !exists
        } else {
            exists
        }
    }

    /// Evaluate a scalar subquery
    pub fn eval_scalar<F>(
        &mut self,
        def: &SubqueryDef,
        outer_binding: &Binding,
        result_var: &Var,
        execute_subquery: F,
    ) -> Option<Value>
    where
        F: FnOnce(&Binding) -> Vec<Binding>,
    {
        // Check cache for non-correlated
        if !def.is_correlated {
            if let SubqueryCache::Scalar(result) = &self.cache[def.id] {
                return result.clone();
            }
        }

        // Execute subquery
        let results = execute_subquery(outer_binding);

        // Get first result's value
        let value = results.first().and_then(|b| b.get(result_var)).cloned();

        // Cache if non-correlated
        if !def.is_correlated {
            self.cache[def.id] = SubqueryCache::Scalar(value.clone());
        }

        value
    }

    /// Evaluate an IN subquery
    pub fn eval_in<F>(
        &mut self,
        def: &SubqueryDef,
        outer_binding: &Binding,
        check_value: &Value,
        result_var: &Var,
        execute_subquery: F,
    ) -> bool
    where
        F: FnOnce(&Binding) -> Vec<Binding>,
    {
        let negated = matches!(def.subquery_type, SubqueryType::NotIn);

        // Check cache for non-correlated (build hash set once)
        if !def.is_correlated {
            if let SubqueryCache::ValueSet(set) = &self.cache[def.id] {
                let hash = ValueHash::from(check_value);
                let in_set = set.contains(&hash);
                return if negated { !in_set } else { in_set };
            }
        }

        // Execute subquery and build set
        let results = execute_subquery(outer_binding);
        let set: HashSet<ValueHash> = results
            .iter()
            .filter_map(|b| b.get(result_var))
            .map(ValueHash::from)
            .collect();

        let hash = ValueHash::from(check_value);
        let in_set = set.contains(&hash);

        // Cache if non-correlated
        if !def.is_correlated {
            self.cache[def.id] = SubqueryCache::ValueSet(set);
        }

        if negated {
            !in_set
        } else {
            in_set
        }
    }

    /// Evaluate an ANY subquery
    pub fn eval_any<F>(
        &mut self,
        def: &SubqueryDef,
        outer_binding: &Binding,
        check_value: &Value,
        op: CompareOp,
        result_var: &Var,
        execute_subquery: F,
    ) -> bool
    where
        F: FnOnce(&Binding) -> Vec<Binding>,
    {
        // Check cache for non-correlated
        if !def.is_correlated {
            if let SubqueryCache::ValueList(list) = &self.cache[def.id] {
                return list.iter().any(|v| compare_with_op(check_value, v, op));
            }
        }

        // Execute subquery
        let results = execute_subquery(outer_binding);
        let list: Vec<Value> = results
            .iter()
            .filter_map(|b| b.get(result_var).cloned())
            .collect();

        let result = list.iter().any(|v| compare_with_op(check_value, v, op));

        // Cache if non-correlated
        if !def.is_correlated {
            self.cache[def.id] = SubqueryCache::ValueList(list);
        }

        result
    }

    /// Evaluate an ALL subquery
    pub fn eval_all<F>(
        &mut self,
        def: &SubqueryDef,
        outer_binding: &Binding,
        check_value: &Value,
        op: CompareOp,
        result_var: &Var,
        execute_subquery: F,
    ) -> bool
    where
        F: FnOnce(&Binding) -> Vec<Binding>,
    {
        // Check cache for non-correlated
        if !def.is_correlated {
            if let SubqueryCache::ValueList(list) = &self.cache[def.id] {
                // Empty set: ALL is vacuously true
                if list.is_empty() {
                    return true;
                }
                return list.iter().all(|v| compare_with_op(check_value, v, op));
            }
        }

        // Execute subquery
        let results = execute_subquery(outer_binding);
        let list: Vec<Value> = results
            .iter()
            .filter_map(|b| b.get(result_var).cloned())
            .collect();

        // Empty set: ALL is vacuously true
        let result = if list.is_empty() {
            true
        } else {
            list.iter().all(|v| compare_with_op(check_value, v, op))
        };

        // Cache if non-correlated
        if !def.is_correlated {
            self.cache[def.id] = SubqueryCache::ValueList(list);
        }

        result
    }

    /// Reset cache (for new execution)
    pub fn reset(&mut self) {
        for cache in &mut self.cache {
            *cache = SubqueryCache::Unevaluated;
        }
    }

    /// Check if a subquery is cached
    pub fn is_cached(&self, id: usize) -> bool {
        !matches!(self.cache.get(id), Some(SubqueryCache::Unevaluated) | None)
    }
}

// ============================================================================
// Correlation Detection
// ============================================================================

/// Detect outer variable references in a subquery
pub fn detect_correlation(subquery_vars: &[Var], outer_vars: &[Var]) -> Vec<Var> {
    subquery_vars
        .iter()
        .filter(|v| outer_vars.contains(v))
        .cloned()
        .collect()
}

/// Check if a binding satisfies correlation constraints
pub fn bind_outer_refs(outer_binding: &Binding, outer_refs: &[Var]) -> Binding {
    let mut result = Binding::empty();
    for var in outer_refs {
        if let Some(value) = outer_binding.get(var) {
            let partial = Binding::one(var.clone(), value.clone());
            result = result.merge(&partial).unwrap_or(result);
        }
    }
    result
}

// ============================================================================
// Helper Functions
// ============================================================================

fn compare_with_op(left: &Value, right: &Value, op: CompareOp) -> bool {
    match op {
        CompareOp::Eq => values_equal(left, right),
        CompareOp::Ne => !values_equal(left, right),
        CompareOp::Lt => partial_compare_values(left, right) == Some(std::cmp::Ordering::Less),
        CompareOp::Le => matches!(
            partial_compare_values(left, right),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        CompareOp::Gt => partial_compare_values(left, right) == Some(std::cmp::Ordering::Greater),
        CompareOp::Ge => matches!(
            partial_compare_values(left, right),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
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
    fn test_exists_uncorrelated() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef::exists(0, false);
        let outer = Binding::empty();

        // First call - executes subquery
        let result = executor.eval_exists(&def, &outer, |_| {
            vec![make_binding(&[("x", Value::Integer(1))])]
        });
        assert!(result);

        // Second call - uses cache (won't call closure)
        let result2 = executor.eval_exists(&def, &outer, |_| {
            panic!("Should use cache!");
        });
        assert!(result2);
    }

    #[test]
    fn test_not_exists() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef::exists(0, true); // NOT EXISTS

        let result = executor.eval_exists(&def, &Binding::empty(), |_| {
            vec![] // Empty result
        });
        assert!(result); // NOT EXISTS of empty = true
    }

    #[test]
    fn test_in_subquery() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef::in_list(0, Var::new("x"), false);

        let check = Value::Integer(2);
        let result_var = Var::new("y");

        let in_set = executor.eval_in(&def, &Binding::empty(), &check, &result_var, |_| {
            vec![
                make_binding(&[("y", Value::Integer(1))]),
                make_binding(&[("y", Value::Integer(2))]),
                make_binding(&[("y", Value::Integer(3))]),
            ]
        });
        assert!(in_set);

        // Check not in set
        let check2 = Value::Integer(5);
        let not_in = executor.eval_in(&def, &Binding::empty(), &check2, &result_var, |_| {
            panic!("Should use cache!");
        });
        assert!(!not_in);
    }

    #[test]
    fn test_scalar_subquery() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef::scalar(0, Var::new("result"));
        let result_var = Var::new("max_val");

        let value = executor.eval_scalar(&def, &Binding::empty(), &result_var, |_| {
            vec![make_binding(&[("max_val", Value::Integer(100))])]
        });

        assert_eq!(value, Some(Value::Integer(100)));
    }

    #[test]
    fn test_any_subquery() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef {
            id: 0,
            subquery_type: SubqueryType::Any(CompareOp::Gt),
            compare_var: Some(Var::new("x")),
            result_var: None,
            outer_refs: Vec::new(),
            is_correlated: false,
        };

        let check = Value::Integer(5);
        let result_var = Var::new("y");

        // 5 > ANY (1, 3, 10) = true (5 > 1 and 5 > 3)
        let result = executor.eval_any(
            &def,
            &Binding::empty(),
            &check,
            CompareOp::Gt,
            &result_var,
            |_| {
                vec![
                    make_binding(&[("y", Value::Integer(1))]),
                    make_binding(&[("y", Value::Integer(3))]),
                    make_binding(&[("y", Value::Integer(10))]),
                ]
            },
        );
        assert!(result);
    }

    #[test]
    fn test_all_subquery() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef {
            id: 0,
            subquery_type: SubqueryType::All(CompareOp::Gt),
            compare_var: Some(Var::new("x")),
            result_var: None,
            outer_refs: Vec::new(),
            is_correlated: false,
        };

        let check = Value::Integer(5);
        let result_var = Var::new("y");

        // 5 > ALL (1, 2, 3) = true
        let result = executor.eval_all(
            &def,
            &Binding::empty(),
            &check,
            CompareOp::Gt,
            &result_var,
            |_| {
                vec![
                    make_binding(&[("y", Value::Integer(1))]),
                    make_binding(&[("y", Value::Integer(2))]),
                    make_binding(&[("y", Value::Integer(3))]),
                ]
            },
        );
        assert!(result);

        // Reset and try with value that fails
        executor.reset();
        let check2 = Value::Integer(2);
        let result2 = executor.eval_all(
            &def,
            &Binding::empty(),
            &check2,
            CompareOp::Gt,
            &result_var,
            |_| {
                vec![
                    make_binding(&[("y", Value::Integer(1))]),
                    make_binding(&[("y", Value::Integer(3))]), // 2 > 3 = false
                ]
            },
        );
        assert!(!result2);
    }

    #[test]
    fn test_correlated_no_cache() {
        let mut executor = SubqueryExecutor::new(1);
        let def = SubqueryDef::exists(0, false).with_outer_refs(vec![Var::new("outer_id")]);

        let mut call_count = 0;

        // First call
        let outer1 = make_binding(&[("outer_id", Value::Integer(1))]);
        let _ = executor.eval_exists(&def, &outer1, |_| {
            call_count += 1;
            vec![make_binding(&[("x", Value::Integer(1))])]
        });

        // Second call - should NOT use cache (correlated)
        let outer2 = make_binding(&[("outer_id", Value::Integer(2))]);
        let _ = executor.eval_exists(&def, &outer2, |_| {
            call_count += 1;
            vec![]
        });

        assert_eq!(call_count, 2); // Both calls executed
    }

    #[test]
    fn test_correlation_detection() {
        let subquery_vars = vec![Var::new("x"), Var::new("outer_id"), Var::new("y")];
        let outer_vars = vec![Var::new("outer_id"), Var::new("z")];

        let refs = detect_correlation(&subquery_vars, &outer_vars);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name(), "outer_id");
    }
}
