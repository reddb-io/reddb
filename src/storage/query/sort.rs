//! Sorting Operations for Query Engine
//!
//! Provides sorting, ordering, and limiting capabilities for query results.

use super::value_compare::total_compare_values;
use crate::storage::schema::Value;
use std::cmp::Ordering;

/// Sort direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Ascending order (smallest first)
    #[default]
    Asc,
    /// Descending order (largest first)
    Desc,
}

/// Null handling in sort
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NullsOrder {
    /// Nulls appear first
    First,
    /// Nulls appear last
    #[default]
    Last,
}

/// A single sort key (column + direction)
#[derive(Debug, Clone)]
pub struct SortKey {
    /// Column name to sort by
    pub column: String,
    /// Sort direction
    pub direction: Direction,
    /// Null handling
    pub nulls: NullsOrder,
}

impl SortKey {
    /// Create a new sort key with ascending order
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: Direction::Asc,
            nulls: NullsOrder::Last,
        }
    }

    /// Create a new sort key with descending order
    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: Direction::Desc,
            nulls: NullsOrder::Last,
        }
    }

    /// Set nulls to appear first
    pub fn nulls_first(mut self) -> Self {
        self.nulls = NullsOrder::First;
        self
    }

    /// Set nulls to appear last
    pub fn nulls_last(mut self) -> Self {
        self.nulls = NullsOrder::Last;
        self
    }

    /// Compare two values according to this sort key
    pub fn compare(&self, a: &Value, b: &Value) -> Ordering {
        // Handle nulls
        match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => match self.nulls {
                NullsOrder::First => Ordering::Less,
                NullsOrder::Last => Ordering::Greater,
            },
            (_, Value::Null) => match self.nulls {
                NullsOrder::First => Ordering::Greater,
                NullsOrder::Last => Ordering::Less,
            },
            _ => {
                let base_order = total_compare_values(a, b);
                match self.direction {
                    Direction::Asc => base_order,
                    Direction::Desc => base_order.reverse(),
                }
            }
        }
    }
}

/// Order by specification for queries
#[derive(Debug, Clone, Default)]
pub struct OrderBy {
    /// Sort keys in priority order
    keys: Vec<SortKey>,
}

impl OrderBy {
    /// Create an empty order by
    pub fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// Add an ascending sort key
    pub fn asc(mut self, column: impl Into<String>) -> Self {
        self.keys.push(SortKey::asc(column));
        self
    }

    /// Add a descending sort key
    pub fn desc(mut self, column: impl Into<String>) -> Self {
        self.keys.push(SortKey::desc(column));
        self
    }

    /// Add a sort key
    pub fn then(mut self, key: SortKey) -> Self {
        self.keys.push(key);
        self
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Get sort keys
    pub fn keys(&self) -> &[SortKey] {
        &self.keys
    }

    /// Compare two rows according to all sort keys
    ///
    /// The `get_value` closure takes a row and column name, returning the value.
    pub fn compare<R>(&self, a: &R, b: &R, get_value: impl Fn(&R, &str) -> Value) -> Ordering {
        for key in &self.keys {
            let val_a = get_value(a, &key.column);
            let val_b = get_value(b, &key.column);
            let ord = key.compare(&val_a, &val_b);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }

    /// Sort a slice of rows in place
    pub fn sort_rows<R>(&self, rows: &mut [R], get_value: impl Fn(&R, &str) -> Value) {
        if self.is_empty() {
            return;
        }
        rows.sort_by(|a, b| self.compare(a, b, &get_value));
    }

    /// Phase 3.2 dispatch entry point. Combines `OrderBy::sort_rows`
    /// and `incremental_sort_top_k` behind a single callable that
    /// the planner / executor invokes after deciding the strategy
    /// via `planner::pathkeys::plan_sort`.
    ///
    /// `prefix_keys` is the number of leading sort keys the input
    /// already satisfies (0 if unknown). `limit` is the LIMIT k for
    /// top-k early termination, or `None` for an unbounded sort.
    ///
    /// Behavior matrix:
    /// - `prefix_keys == 0` && `limit == None` → full `sort_rows`.
    /// - `prefix_keys == 0` && `limit == Some(k)` → full sort then
    ///   truncate to k.
    /// - `prefix_keys > 0` && `limit == Some(k)` → call
    ///   `incremental_sort_top_k(prefix_keys, k)`.
    /// - `prefix_keys > 0` && `limit == None` → walk groups and
    ///   sort within each, no early termination (still cheaper than
    ///   full sort because each group is independent).
    pub fn dispatch_sort<R>(
        &self,
        rows: Vec<R>,
        prefix_keys: usize,
        limit: Option<usize>,
        get_value: impl Fn(&R, &str) -> Value,
    ) -> Vec<R> {
        if self.is_empty() {
            return rows;
        }
        match (prefix_keys, limit) {
            (0, None) => {
                let mut all = rows;
                self.sort_rows(&mut all, &get_value);
                all
            }
            (0, Some(k)) => {
                let mut all = rows;
                self.sort_rows(&mut all, &get_value);
                all.truncate(k);
                all
            }
            (_, Some(k)) => self.incremental_sort_top_k(rows, prefix_keys, k, get_value),
            (_, None) => {
                // Group-by-prefix sort with no early termination.
                // Equivalent to incremental_sort_top_k with k = usize::MAX.
                self.incremental_sort_top_k(rows, prefix_keys, usize::MAX, get_value)
            }
        }
    }

    /// Incremental top-K sort.
    ///
    /// Fase 4 P3 win: when the upstream operator already returns
    /// rows in `prefix_keys` order (e.g. an index scan whose key
    /// is a prefix of the requested ORDER BY), this method walks
    /// the input in chunks of equal-prefix rows, sorts each chunk
    /// by the *remaining* keys, emits up to `k` rows total, and
    /// terminates as soon as the budget is met.
    ///
    /// Mirrors PG's `nodeIncrementalSort.c` algorithm, simplified:
    ///
    /// 1. Walk `rows` left-to-right, grouping by equal-prefix.
    /// 2. For each group, full-sort by `self.keys[prefix_keys..]`
    ///    (the suffix not already covered by upstream order).
    /// 3. Append at most `k - emitted` rows from the sorted group
    ///    to the output, then advance to the next group.
    /// 4. Stop iteration entirely once `emitted == k`.
    ///
    /// **Caller contract**: rows MUST already be sorted by
    /// `self.keys[..prefix_keys]`. Violating this produces wrong
    /// results — the planner is responsible for verifying input
    /// pathkey order before choosing this operator.
    ///
    /// When `prefix_keys == 0` this degenerates to a regular
    /// top-k sort using `sort_rows` + truncate. When
    /// `prefix_keys >= self.keys.len()` the input is already
    /// fully ordered and the method just truncates.
    pub fn incremental_sort_top_k<R>(
        &self,
        rows: Vec<R>,
        prefix_keys: usize,
        k: usize,
        get_value: impl Fn(&R, &str) -> Value,
    ) -> Vec<R> {
        if k == 0 || rows.is_empty() {
            return Vec::new();
        }
        // No prefix order known — fall back to full sort + truncate.
        if prefix_keys == 0 {
            let mut all = rows;
            self.sort_rows(&mut all, &get_value);
            all.truncate(k);
            return all;
        }
        // Input is already fully ordered — just truncate.
        if prefix_keys >= self.keys.len() {
            let mut out = rows;
            out.truncate(k);
            return out;
        }

        let suffix_keys = &self.keys[prefix_keys..];
        let prefix_slice = &self.keys[..prefix_keys];
        let mut out: Vec<R> = Vec::with_capacity(k);
        let mut group: Vec<R> = Vec::new();

        // Closure to flush the current group into `out`, sorting
        // by suffix keys first. Returns `true` when the budget is
        // exhausted and the caller should stop iteration.
        let flush =
            |group: &mut Vec<R>, out: &mut Vec<R>, get_value: &dyn Fn(&R, &str) -> Value| -> bool {
                if group.is_empty() {
                    return false;
                }
                // Sort the group by the suffix keys only — prefix is
                // already equal across the whole group.
                group.sort_by(|a, b| {
                    for key in suffix_keys {
                        let ord =
                            key.compare(&get_value(a, &key.column), &get_value(b, &key.column));
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    Ordering::Equal
                });
                let remaining = k - out.len();
                if group.len() <= remaining {
                    out.append(group);
                } else {
                    out.extend(group.drain(..remaining));
                    group.clear();
                }
                out.len() >= k
            };

        // Helper to compare two rows by prefix keys. Inline closure
        // would shadow `get_value`; use an inner fn-style binding.
        let prefix_eq = |a: &R, b: &R| -> bool {
            for key in prefix_slice {
                if key.compare(&get_value(a, &key.column), &get_value(b, &key.column))
                    != Ordering::Equal
                {
                    return false;
                }
            }
            true
        };

        // Wrap get_value so the flush closure can take `&dyn Fn`.
        // The wrapper has a stable address inside this function.
        let get_value_dyn: &dyn Fn(&R, &str) -> Value = &get_value;

        for row in rows {
            if let Some(first) = group.first() {
                if !prefix_eq(first, &row) {
                    if flush(&mut group, &mut out, get_value_dyn) {
                        return out;
                    }
                }
            }
            group.push(row);
        }
        // Flush the final group.
        flush(&mut group, &mut out, get_value_dyn);
        out
    }

    /// Get all referenced columns
    pub fn referenced_columns(&self) -> Vec<&str> {
        self.keys.iter().map(|k| k.column.as_str()).collect()
    }
}

/// Query limits (LIMIT and OFFSET)
#[derive(Debug, Clone, Default)]
pub struct QueryLimits {
    /// Maximum number of rows to return
    pub limit: Option<usize>,
    /// Number of rows to skip
    pub offset: usize,
}

impl QueryLimits {
    /// Create with no limits
    pub fn none() -> Self {
        Self {
            limit: None,
            offset: 0,
        }
    }

    /// Set limit
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set offset
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = n;
        self
    }

    /// Apply limits to a vector of results
    pub fn apply<T>(&self, mut items: Vec<T>) -> Vec<T> {
        if self.offset >= items.len() {
            return Vec::new();
        }

        let start = self.offset;
        let end = match self.limit {
            Some(limit) => (start + limit).min(items.len()),
            None => items.len(),
        };

        items.drain(start..end).collect()
    }

    /// Apply limits to an iterator
    pub fn apply_iter<T: 'static, I: Iterator<Item = T> + 'static>(
        &self,
        iter: I,
    ) -> Box<dyn Iterator<Item = T>> {
        let iter = iter.skip(self.offset);
        match self.limit {
            Some(limit) => Box::new(iter.take(limit)),
            None => Box::new(iter),
        }
    }

    /// Calculate the effective range for pagination
    pub fn range(&self, total: usize) -> std::ops::Range<usize> {
        let start = self.offset.min(total);
        let end = match self.limit {
            Some(limit) => (start + limit).min(total),
            None => total,
        };
        start..end
    }
}

/// Top-K tracking structure for incremental sorting
pub struct TopK<T> {
    /// Maximum items to keep
    k: usize,
    /// Items stored
    items: Vec<T>,
    /// Comparison function
    compare: Box<dyn Fn(&T, &T) -> Ordering>,
}

impl<T> TopK<T> {
    /// Create a new top-k tracker
    pub fn new<F>(k: usize, compare: F) -> Self
    where
        F: Fn(&T, &T) -> Ordering + 'static,
    {
        Self {
            k,
            items: Vec::with_capacity(k + 1),
            compare: Box::new(compare),
        }
    }

    /// Push an item, maintaining only top k
    pub fn push(&mut self, item: T) {
        self.items.push(item);
        self.items.sort_by(&self.compare);
        if self.items.len() > self.k {
            self.items.pop();
        }
    }

    /// Get current items
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Take the items
    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    /// Number of items
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_key_asc() {
        let key = SortKey::asc("age");

        assert_eq!(
            key.compare(&Value::Integer(25), &Value::Integer(30)),
            Ordering::Less
        );
        assert_eq!(
            key.compare(&Value::Integer(30), &Value::Integer(25)),
            Ordering::Greater
        );
        assert_eq!(
            key.compare(&Value::Integer(25), &Value::Integer(25)),
            Ordering::Equal
        );
    }

    #[test]
    fn test_sort_key_desc() {
        let key = SortKey::desc("age");

        assert_eq!(
            key.compare(&Value::Integer(25), &Value::Integer(30)),
            Ordering::Greater
        );
        assert_eq!(
            key.compare(&Value::Integer(30), &Value::Integer(25)),
            Ordering::Less
        );
    }

    #[test]
    fn test_sort_key_nulls_first() {
        let key = SortKey::asc("age").nulls_first();

        assert_eq!(
            key.compare(&Value::Null, &Value::Integer(30)),
            Ordering::Less
        );
        assert_eq!(
            key.compare(&Value::Integer(30), &Value::Null),
            Ordering::Greater
        );
    }

    #[test]
    fn test_sort_key_nulls_last() {
        let key = SortKey::asc("age").nulls_last();

        assert_eq!(
            key.compare(&Value::Null, &Value::Integer(30)),
            Ordering::Greater
        );
        assert_eq!(
            key.compare(&Value::Integer(30), &Value::Null),
            Ordering::Less
        );
    }

    #[test]
    fn test_order_by_single() {
        let order = OrderBy::new().asc("age");

        // Simple row type: (name, age)
        type Row = (String, i64);
        let get_value = |row: &Row, col: &str| -> Value {
            match col {
                "name" => Value::text(row.0.clone()),
                "age" => Value::Integer(row.1),
                _ => Value::Null,
            }
        };

        let mut rows = vec![
            ("Charlie".to_string(), 30),
            ("Alice".to_string(), 25),
            ("Bob".to_string(), 35),
        ];

        order.sort_rows(&mut rows, get_value);

        assert_eq!(rows[0].0, "Alice");
        assert_eq!(rows[1].0, "Charlie");
        assert_eq!(rows[2].0, "Bob");
    }

    #[test]
    fn test_order_by_multiple() {
        let order = OrderBy::new().asc("department").desc("salary");

        type Row = (String, String, i64); // (name, department, salary)
        let get_value = |row: &Row, col: &str| -> Value {
            match col {
                "name" => Value::text(row.0.clone()),
                "department" => Value::text(row.1.clone()),
                "salary" => Value::Integer(row.2),
                _ => Value::Null,
            }
        };

        let mut rows = vec![
            ("Alice".to_string(), "Engineering".to_string(), 100000),
            ("Bob".to_string(), "Engineering".to_string(), 120000),
            ("Charlie".to_string(), "Sales".to_string(), 90000),
            ("Diana".to_string(), "Engineering".to_string(), 110000),
        ];

        order.sort_rows(&mut rows, get_value);

        // Engineering first (alphabetically), then by salary descending
        assert_eq!(rows[0].0, "Bob"); // Eng, 120k
        assert_eq!(rows[1].0, "Diana"); // Eng, 110k
        assert_eq!(rows[2].0, "Alice"); // Eng, 100k
        assert_eq!(rows[3].0, "Charlie"); // Sales, 90k
    }

    #[test]
    fn test_query_limits_apply() {
        let items: Vec<i32> = (0..10).collect();

        // Limit only
        let limited = QueryLimits::none().limit(3).apply(items.clone());
        assert_eq!(limited, vec![0, 1, 2]);

        // Offset only
        let offset = QueryLimits::none().offset(3).apply(items.clone());
        assert_eq!(offset, vec![3, 4, 5, 6, 7, 8, 9]);

        // Both
        let both = QueryLimits::none().offset(2).limit(3).apply(items.clone());
        assert_eq!(both, vec![2, 3, 4]);

        // Offset beyond length
        let empty = QueryLimits::none().offset(20).apply(items.clone());
        assert!(empty.is_empty());
    }

    #[test]
    fn test_query_limits_range() {
        let limits = QueryLimits::none().offset(5).limit(10);

        assert_eq!(limits.range(100), 5..15);
        assert_eq!(limits.range(8), 5..8); // Limit by total
        assert_eq!(limits.range(3), 3..3); // Offset beyond total
    }

    #[test]
    fn test_top_k() {
        let mut topk = TopK::new(3, |a: &i32, b: &i32| a.cmp(b));

        topk.push(5);
        topk.push(2);
        topk.push(8);
        topk.push(1);
        topk.push(9);

        let items = topk.into_items();
        assert_eq!(items, vec![1, 2, 5]); // Top 3 smallest
    }

    #[test]
    fn test_top_k_desc() {
        let mut topk = TopK::new(3, |a: &i32, b: &i32| b.cmp(a)); // Reverse for largest

        topk.push(5);
        topk.push(2);
        topk.push(8);
        topk.push(1);
        topk.push(9);

        let items = topk.into_items();
        assert_eq!(items, vec![9, 8, 5]); // Top 3 largest
    }

    #[test]
    fn test_compare_cross_type() {
        assert_eq!(
            total_compare_values(&Value::Integer(10), &Value::Float(10.0)),
            Ordering::Equal
        );
        assert_eq!(
            total_compare_values(&Value::Integer(9), &Value::Float(10.0)),
            Ordering::Less
        );
    }

    #[test]
    fn test_order_by_empty() {
        let order = OrderBy::new();
        assert!(order.is_empty());

        let mut rows = vec![3, 1, 2];
        order.sort_rows(&mut rows, |r, _| Value::Integer(*r));
        // Should not change order
        assert_eq!(rows, vec![3, 1, 2]);
    }

    #[test]
    fn test_sort_key_text() {
        let key = SortKey::asc("name");

        assert_eq!(
            key.compare(
                &Value::text("Alice".to_string()),
                &Value::text("Bob".to_string())
            ),
            Ordering::Less
        );
    }

    #[test]
    fn test_sort_key_timestamp() {
        let key = SortKey::desc("created_at");

        // Later timestamp should come first in desc order
        assert_eq!(
            key.compare(&Value::Timestamp(1000), &Value::Timestamp(500)),
            Ordering::Less // 1000 is "smaller" in desc = comes first
        );
    }
}
