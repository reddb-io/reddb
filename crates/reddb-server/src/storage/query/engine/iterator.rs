//! Query Result Iterators
//!
//! Lazy evaluation of query results through binding streams.
//!
//! # Design
//!
//! - Pull-based iteration (demand-driven)
//! - Composable iterator wrappers
//! - Memory-efficient streaming
//! - Early termination support

use super::binding::{Binding, Value, Var};
use std::collections::HashSet;
use std::fmt::Debug;

/// Result from iterator operations
pub type IterResult = Result<Option<Binding>, IterError>;

/// Iterator errors
#[derive(Debug, Clone)]
pub enum IterError {
    /// Execution error
    Execution(String),
    /// Timeout
    Timeout,
    /// Cancelled
    Cancelled,
    /// Resource exhausted
    ResourceExhausted(String),
}

impl std::fmt::Display for IterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IterError::Execution(msg) => write!(f, "Execution error: {}", msg),
            IterError::Timeout => write!(f, "Query timeout"),
            IterError::Cancelled => write!(f, "Query cancelled"),
            IterError::ResourceExhausted(msg) => write!(f, "Resource exhausted: {}", msg),
        }
    }
}

impl std::error::Error for IterError {}

/// Core trait for binding iterators
pub trait BindingIterator: Debug + Send {
    /// Get next binding
    fn next_binding(&mut self) -> IterResult;

    /// Check if there are more results (optional hint)
    fn has_next(&self) -> bool {
        true // Default: unknown, caller should try next()
    }

    /// Get variables produced by this iterator
    fn vars(&self) -> Vec<Var>;

    /// Cancel iteration
    fn cancel(&mut self);

    /// Reset iterator to beginning (if supported)
    fn reset(&mut self) -> bool {
        false // Default: not supported
    }
}

/// Base query iterator wrapping a binding source
#[derive(Debug)]
pub struct QueryIterBase {
    bindings: Vec<Binding>,
    index: usize,
    vars: Vec<Var>,
    cancelled: bool,
}

impl QueryIterBase {
    /// Create from binding list
    pub fn new(bindings: Vec<Binding>) -> Self {
        let vars = if let Some(first) = bindings.first() {
            first.all_vars().into_iter().cloned().collect()
        } else {
            Vec::new()
        };

        Self {
            bindings,
            index: 0,
            vars,
            cancelled: false,
        }
    }

    /// Create empty iterator
    pub fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            index: 0,
            vars: Vec::new(),
            cancelled: false,
        }
    }

    /// Create single-result iterator
    pub fn single(binding: Binding) -> Self {
        let vars = binding.all_vars().into_iter().cloned().collect();
        Self {
            bindings: vec![binding],
            index: 0,
            vars,
            cancelled: false,
        }
    }
}

impl BindingIterator for QueryIterBase {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        if self.index < self.bindings.len() {
            let binding = self.bindings[self.index].clone();
            self.index += 1;
            Ok(Some(binding))
        } else {
            Ok(None)
        }
    }

    fn has_next(&self) -> bool {
        !self.cancelled && self.index < self.bindings.len()
    }

    fn vars(&self) -> Vec<Var> {
        self.vars.clone()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
    }

    fn reset(&mut self) -> bool {
        self.index = 0;
        self.cancelled = false;
        true
    }
}

/// Filter iterator - applies predicate to upstream
pub struct QueryIterFilter {
    upstream: Box<dyn BindingIterator>,
    predicate: Box<dyn Fn(&Binding) -> bool + Send + Sync>,
    cancelled: bool,
}

impl std::fmt::Debug for QueryIterFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryIterFilter")
            .field("upstream", &self.upstream)
            .field("predicate", &"<filter fn>")
            .field("cancelled", &self.cancelled)
            .finish()
    }
}

impl QueryIterFilter {
    /// Create filter iterator
    pub fn new<F>(upstream: Box<dyn BindingIterator>, predicate: F) -> Self
    where
        F: Fn(&Binding) -> bool + Send + Sync + 'static,
    {
        Self {
            upstream,
            predicate: Box::new(predicate),
            cancelled: false,
        }
    }
}

impl BindingIterator for QueryIterFilter {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        loop {
            match self.upstream.next_binding()? {
                Some(binding) => {
                    if (self.predicate)(&binding) {
                        return Ok(Some(binding));
                    }
                    // Continue to next
                }
                None => return Ok(None),
            }
        }
    }

    fn vars(&self) -> Vec<Var> {
        self.upstream.vars()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.upstream.cancel();
    }
}

/// Join iterator - nested loop join
pub struct QueryIterJoin {
    left: Box<dyn BindingIterator>,
    right_factory: Box<dyn Fn() -> Box<dyn BindingIterator> + Send + Sync>,
    current_left: Option<Binding>,
    current_right: Option<Box<dyn BindingIterator>>,
    vars: Vec<Var>,
    cancelled: bool,
}

impl std::fmt::Debug for QueryIterJoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryIterJoin")
            .field("left", &self.left)
            .field("right_factory", &"<factory fn>")
            .field("current_left", &self.current_left)
            .field("current_right", &self.current_right)
            .field("vars", &self.vars)
            .field("cancelled", &self.cancelled)
            .finish()
    }
}

impl QueryIterJoin {
    /// Create join iterator
    pub fn new<F>(left: Box<dyn BindingIterator>, right_factory: F, right_vars: Vec<Var>) -> Self
    where
        F: Fn() -> Box<dyn BindingIterator> + Send + Sync + 'static,
    {
        let mut vars = left.vars();
        for v in right_vars {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }

        Self {
            left,
            right_factory: Box::new(right_factory),
            current_left: None,
            current_right: None,
            vars,
            cancelled: false,
        }
    }
}

impl BindingIterator for QueryIterJoin {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        loop {
            // Try to get next from current right
            if let Some(ref mut right) = self.current_right {
                if let Some(right_binding) = right.next_binding()? {
                    // Merge with current left
                    if let Some(ref left_binding) = self.current_left {
                        if let Some(merged) = left_binding.merge(&right_binding) {
                            return Ok(Some(merged));
                        }
                        // Conflict, continue to next right
                        continue;
                    }
                }
            }

            // Need new left binding
            match self.left.next_binding()? {
                Some(left_binding) => {
                    self.current_left = Some(left_binding);
                    self.current_right = Some((self.right_factory)());
                }
                None => return Ok(None),
            }
        }
    }

    fn vars(&self) -> Vec<Var> {
        self.vars.clone()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.left.cancel();
        if let Some(ref mut right) = self.current_right {
            right.cancel();
        }
    }
}

/// Union iterator - concatenates multiple iterators
#[derive(Debug)]
pub struct QueryIterUnion {
    iterators: Vec<Box<dyn BindingIterator>>,
    current_index: usize,
    vars: Vec<Var>,
    cancelled: bool,
}

impl QueryIterUnion {
    /// Create union iterator
    pub fn new(iterators: Vec<Box<dyn BindingIterator>>) -> Self {
        let mut vars = Vec::new();
        for iter in &iterators {
            for v in iter.vars() {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }

        Self {
            iterators,
            current_index: 0,
            vars,
            cancelled: false,
        }
    }
}

impl BindingIterator for QueryIterUnion {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        while self.current_index < self.iterators.len() {
            match self.iterators[self.current_index].next_binding()? {
                Some(binding) => return Ok(Some(binding)),
                None => {
                    self.current_index += 1;
                }
            }
        }

        Ok(None)
    }

    fn vars(&self) -> Vec<Var> {
        self.vars.clone()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        for iter in &mut self.iterators {
            iter.cancel();
        }
    }
}

/// Project iterator - selects subset of variables
#[derive(Debug)]
pub struct QueryIterProject {
    upstream: Box<dyn BindingIterator>,
    project_vars: Vec<Var>,
    cancelled: bool,
}

impl QueryIterProject {
    /// Create project iterator
    pub fn new(upstream: Box<dyn BindingIterator>, vars: Vec<Var>) -> Self {
        Self {
            upstream,
            project_vars: vars,
            cancelled: false,
        }
    }
}

impl BindingIterator for QueryIterProject {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        match self.upstream.next_binding()? {
            Some(binding) => Ok(Some(binding.project(&self.project_vars))),
            None => Ok(None),
        }
    }

    fn vars(&self) -> Vec<Var> {
        self.project_vars.clone()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.upstream.cancel();
    }
}

/// Slice iterator - limit and offset
#[derive(Debug)]
pub struct QueryIterSlice {
    upstream: Box<dyn BindingIterator>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    returned: u64,
    cancelled: bool,
}

impl QueryIterSlice {
    /// Create slice iterator
    pub fn new(upstream: Box<dyn BindingIterator>, offset: u64, limit: Option<u64>) -> Self {
        Self {
            upstream,
            offset,
            limit,
            skipped: 0,
            returned: 0,
            cancelled: false,
        }
    }

    /// Create limit-only iterator
    pub fn limit(upstream: Box<dyn BindingIterator>, limit: u64) -> Self {
        Self::new(upstream, 0, Some(limit))
    }

    /// Create offset-only iterator
    pub fn offset(upstream: Box<dyn BindingIterator>, offset: u64) -> Self {
        Self::new(upstream, offset, None)
    }
}

impl BindingIterator for QueryIterSlice {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        // Check limit
        if let Some(limit) = self.limit {
            if self.returned >= limit {
                return Ok(None);
            }
        }

        // Skip offset
        while self.skipped < self.offset {
            match self.upstream.next_binding()? {
                Some(_) => {
                    self.skipped += 1;
                }
                None => return Ok(None),
            }
        }

        // Return result
        match self.upstream.next_binding()? {
            Some(binding) => {
                self.returned += 1;
                Ok(Some(binding))
            }
            None => Ok(None),
        }
    }

    fn vars(&self) -> Vec<Var> {
        self.upstream.vars()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.upstream.cancel();
    }
}

/// Sort iterator - orders results
#[derive(Debug)]
pub struct QueryIterSort {
    upstream: Box<dyn BindingIterator>,
    comparators: Vec<SortKey>,
    sorted: Option<Vec<Binding>>,
    index: usize,
    cancelled: bool,
}

/// Sort key specification
#[derive(Debug, Clone)]
pub struct SortKey {
    /// Variable to sort by
    pub var: Var,
    /// Ascending order
    pub ascending: bool,
}

impl SortKey {
    /// Create ascending sort key
    pub fn asc(var: Var) -> Self {
        Self {
            var,
            ascending: true,
        }
    }

    /// Create descending sort key
    pub fn desc(var: Var) -> Self {
        Self {
            var,
            ascending: false,
        }
    }
}

impl QueryIterSort {
    /// Create sort iterator
    pub fn new(upstream: Box<dyn BindingIterator>, comparators: Vec<SortKey>) -> Self {
        Self {
            upstream,
            comparators,
            sorted: None,
            index: 0,
            cancelled: false,
        }
    }

    /// Materialize and sort all results
    fn materialize(&mut self) -> Result<(), IterError> {
        if self.sorted.is_some() {
            return Ok(());
        }

        let mut bindings = Vec::new();
        while let Some(b) = self.upstream.next_binding()? {
            bindings.push(b);
        }

        // Sort by comparators
        let comparators = self.comparators.clone();
        bindings.sort_by(|a, b| {
            for key in &comparators {
                let a_val = a.get(&key.var);
                let b_val = b.get(&key.var);

                let ordering = compare_values(a_val, b_val);
                if ordering != std::cmp::Ordering::Equal {
                    return if key.ascending {
                        ordering
                    } else {
                        ordering.reverse()
                    };
                }
            }
            std::cmp::Ordering::Equal
        });

        self.sorted = Some(bindings);
        Ok(())
    }
}

impl BindingIterator for QueryIterSort {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        self.materialize()?;

        if let Some(ref sorted) = self.sorted {
            if self.index < sorted.len() {
                let binding = sorted[self.index].clone();
                self.index += 1;
                return Ok(Some(binding));
            }
        }

        Ok(None)
    }

    fn vars(&self) -> Vec<Var> {
        self.upstream.vars()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.upstream.cancel();
    }
}

/// Compare two optional values
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a_val), Some(b_val)) => compare_value(a_val, b_val),
    }
}

/// Compare two values
fn compare_value(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        (Value::Node(a), Value::Node(b)) => a.cmp(b),
        (Value::Edge(a), Value::Edge(b)) => a.cmp(b),
        (Value::Uri(a), Value::Uri(b)) => a.cmp(b),
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        // Cross-type comparison: types differ, so we return a consistent ordering
        _ => {
            let type_order = |v: &Value| -> u8 {
                match v {
                    Value::Null => 0,
                    Value::Boolean(_) => 1,
                    Value::Integer(_) => 2,
                    Value::Float(_) => 3,
                    Value::String(_) => 4,
                    Value::Node(_) => 5,
                    Value::Edge(_) => 6,
                    Value::Uri(_) => 7,
                }
            };
            type_order(a).cmp(&type_order(b))
        }
    }
}

/// Distinct iterator - removes duplicates
#[derive(Debug)]
pub struct QueryIterDistinct {
    upstream: Box<dyn BindingIterator>,
    seen: HashSet<u64>,
    cancelled: bool,
}

impl QueryIterDistinct {
    /// Create distinct iterator
    pub fn new(upstream: Box<dyn BindingIterator>) -> Self {
        Self {
            upstream,
            seen: HashSet::new(),
            cancelled: false,
        }
    }

    /// Hash a binding for deduplication
    fn hash_binding(binding: &Binding) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        binding.hash(&mut hasher);
        hasher.finish()
    }
}

impl BindingIterator for QueryIterDistinct {
    fn next_binding(&mut self) -> IterResult {
        if self.cancelled {
            return Err(IterError::Cancelled);
        }

        loop {
            match self.upstream.next_binding()? {
                Some(binding) => {
                    let hash = Self::hash_binding(&binding);
                    if self.seen.insert(hash) {
                        return Ok(Some(binding));
                    }
                    // Already seen, continue
                }
                None => return Ok(None),
            }
        }
    }

    fn vars(&self) -> Vec<Var> {
        self.upstream.vars()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.upstream.cancel();
    }
}

/// Wrapper for boxed iterator to implement Iterator trait
pub struct QueryIter {
    inner: Box<dyn BindingIterator>,
}

impl QueryIter {
    /// Create from binding iterator
    pub fn new(inner: Box<dyn BindingIterator>) -> Self {
        Self { inner }
    }

    /// Get variables
    pub fn vars(&self) -> Vec<Var> {
        self.inner.vars()
    }

    /// Cancel iteration
    pub fn cancel(&mut self) {
        self.inner.cancel();
    }
}

impl Iterator for QueryIter {
    type Item = Result<Binding, IterError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next_binding() {
            Ok(Some(binding)) => Some(Ok(binding)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::engine::binding::BindingBuilder;

    fn make_binding(x: i64) -> Binding {
        BindingBuilder::new()
            .add_named("x", Value::Integer(x))
            .build()
    }

    #[test]
    fn test_base_iterator() {
        let bindings = vec![make_binding(1), make_binding(2), make_binding(3)];
        let mut iter = QueryIterBase::new(bindings);

        assert!(iter.has_next());
        assert!(iter.next_binding().unwrap().is_some());
        assert!(iter.next_binding().unwrap().is_some());
        assert!(iter.next_binding().unwrap().is_some());
        assert!(iter.next_binding().unwrap().is_none());
    }

    #[test]
    fn test_filter_iterator() {
        let bindings = vec![make_binding(1), make_binding(2), make_binding(3)];
        let base = Box::new(QueryIterBase::new(bindings));

        let mut iter = QueryIterFilter::new(base, |b| {
            b.get(&Var::new("x"))
                .and_then(|v| v.as_integer())
                .map(|i| i > 1)
                .unwrap_or(false)
        });

        // Should skip 1, return 2 and 3
        let b1 = iter.next_binding().unwrap().unwrap();
        assert_eq!(b1.get(&Var::new("x")), Some(&Value::Integer(2)));

        let b2 = iter.next_binding().unwrap().unwrap();
        assert_eq!(b2.get(&Var::new("x")), Some(&Value::Integer(3)));

        assert!(iter.next_binding().unwrap().is_none());
    }

    #[test]
    fn test_slice_iterator() {
        let bindings: Vec<_> = (1..=10).map(make_binding).collect();
        let base = Box::new(QueryIterBase::new(bindings));

        // Offset 2, limit 3
        let mut iter = QueryIterSlice::new(base, 2, Some(3));

        let b1 = iter.next_binding().unwrap().unwrap();
        assert_eq!(b1.get(&Var::new("x")), Some(&Value::Integer(3)));

        let b2 = iter.next_binding().unwrap().unwrap();
        assert_eq!(b2.get(&Var::new("x")), Some(&Value::Integer(4)));

        let b3 = iter.next_binding().unwrap().unwrap();
        assert_eq!(b3.get(&Var::new("x")), Some(&Value::Integer(5)));

        assert!(iter.next_binding().unwrap().is_none());
    }

    #[test]
    fn test_project_iterator() {
        let binding = BindingBuilder::new()
            .add_named("x", Value::Integer(1))
            .add_named("y", Value::Integer(2))
            .add_named("z", Value::Integer(3))
            .build();

        let base = Box::new(QueryIterBase::single(binding));
        let mut iter = QueryIterProject::new(base, vec![Var::new("x"), Var::new("z")]);

        let result = iter.next_binding().unwrap().unwrap();
        assert!(result.contains(&Var::new("x")));
        assert!(!result.contains(&Var::new("y")));
        assert!(result.contains(&Var::new("z")));
    }

    #[test]
    fn test_union_iterator() {
        let iter1 = Box::new(QueryIterBase::new(vec![make_binding(1), make_binding(2)]));
        let iter2 = Box::new(QueryIterBase::new(vec![make_binding(3), make_binding(4)]));

        let mut union = QueryIterUnion::new(vec![iter1, iter2]);

        let mut results = Vec::new();
        while let Some(b) = union.next_binding().unwrap() {
            results.push(b.get(&Var::new("x")).unwrap().as_integer().unwrap());
        }

        assert_eq!(results, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_sort_iterator() {
        let bindings = vec![make_binding(3), make_binding(1), make_binding(2)];
        let base = Box::new(QueryIterBase::new(bindings));

        let mut iter = QueryIterSort::new(base, vec![SortKey::asc(Var::new("x"))]);

        let mut results = Vec::new();
        while let Some(b) = iter.next_binding().unwrap() {
            results.push(b.get(&Var::new("x")).unwrap().as_integer().unwrap());
        }

        assert_eq!(results, vec![1, 2, 3]);
    }

    #[test]
    fn test_distinct_iterator() {
        let bindings = vec![
            make_binding(1),
            make_binding(2),
            make_binding(1),
            make_binding(3),
            make_binding(2),
        ];
        let base = Box::new(QueryIterBase::new(bindings));

        let mut iter = QueryIterDistinct::new(base);

        let mut results = Vec::new();
        while let Some(b) = iter.next_binding().unwrap() {
            results.push(b.get(&Var::new("x")).unwrap().as_integer().unwrap());
        }

        assert_eq!(results, vec![1, 2, 3]);
    }

    #[test]
    fn test_cancel_iterator() {
        let bindings: Vec<_> = (1..=100).map(make_binding).collect();
        let mut iter = QueryIterBase::new(bindings);

        // Read a few
        iter.next_binding().unwrap();
        iter.next_binding().unwrap();

        // Cancel
        iter.cancel();

        // Should return cancelled error
        assert!(matches!(iter.next_binding(), Err(IterError::Cancelled)));
    }

    #[test]
    fn test_query_iter_wrapper() {
        let bindings = vec![make_binding(1), make_binding(2)];
        let base = Box::new(QueryIterBase::new(bindings));

        let iter = QueryIter::new(base);
        let results: Vec<_> = iter.collect();

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
    }
}
