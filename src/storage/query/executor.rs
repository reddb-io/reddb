//! Query Executor for RedDB
//!
//! Executes query plans against the storage engine, applying filters,
//! sorting, and limits to produce results.

use super::filter::Filter;
use super::sort::{OrderBy, QueryLimits};
use crate::storage::schema::{Row, Value};
use std::collections::HashMap;

/// Query result set
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Column names
    pub columns: Vec<String>,
    /// Result rows
    pub rows: Vec<Row>,
    /// Total rows before limits (if known)
    pub total_count: Option<usize>,
    /// Execution statistics
    pub stats: QueryStats,
}

impl QueryResult {
    /// Create empty result
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            total_count: Some(0),
            stats: QueryStats::default(),
        }
    }

    /// Create from rows
    pub fn from_rows(columns: Vec<String>, rows: Vec<Row>) -> Self {
        let count = rows.len();
        Self {
            columns,
            rows,
            total_count: Some(count),
            stats: QueryStats::default(),
        }
    }

    /// Get row count
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Get a column index by name
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// Get value from a row by column name
    pub fn get_value(&self, row_idx: usize, column: &str) -> Option<&Value> {
        let col_idx = self.column_index(column)?;
        self.rows.get(row_idx)?.get(col_idx)
    }

    /// Iterate over rows with column access
    pub fn iter_rows(&self) -> impl Iterator<Item = RowView<'_>> {
        self.rows.iter().map(|row| RowView {
            columns: &self.columns,
            row,
        })
    }
}

/// View of a single row with column name access
pub struct RowView<'a> {
    columns: &'a [String],
    row: &'a Row,
}

impl<'a> RowView<'a> {
    /// Get value by column name
    pub fn get(&self, column: &str) -> Option<&Value> {
        let idx = self.columns.iter().position(|c| c == column)?;
        self.row.get(idx)
    }

    /// Get value by index
    pub fn get_index(&self, idx: usize) -> Option<&Value> {
        self.row.get(idx)
    }

    /// Get all values
    pub fn values(&self) -> &[Value] {
        self.row.values()
    }
}

/// Query execution statistics
#[derive(Debug, Clone, Default)]
pub struct QueryStats {
    /// Rows scanned
    pub rows_scanned: usize,
    /// Rows matched filter
    pub rows_matched: usize,
    /// Rows returned
    pub rows_returned: usize,
    /// Execution time in microseconds
    pub execution_time_us: u64,
    /// Index used (if any)
    pub index_used: Option<String>,
}

/// Query plan representing a query to execute
#[derive(Debug, Clone)]
pub struct QueryPlan {
    /// Table to query
    pub table: String,
    /// Columns to select (empty = all)
    pub columns: Vec<String>,
    /// Filter conditions
    pub filter: Option<Filter>,
    /// Ordering
    pub order_by: OrderBy,
    /// Limits
    pub limits: QueryLimits,
    /// Whether to count total rows
    pub count_total: bool,
}

impl QueryPlan {
    /// Create a new query plan for a table
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            columns: Vec::new(),
            filter: None,
            order_by: OrderBy::new(),
            limits: QueryLimits::none(),
            count_total: false,
        }
    }

    /// Select specific columns
    pub fn select(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
        self
    }

    /// Add a filter
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and_filter(filter),
            None => filter,
        });
        self
    }

    /// Add ordering
    pub fn order_by(mut self, order: OrderBy) -> Self {
        self.order_by = order;
        self
    }

    /// Set limit
    pub fn limit(mut self, n: usize) -> Self {
        self.limits = self.limits.limit(n);
        self
    }

    /// Set offset
    pub fn offset(mut self, n: usize) -> Self {
        self.limits = self.limits.offset(n);
        self
    }

    /// Enable total count
    pub fn with_total_count(mut self) -> Self {
        self.count_total = true;
        self
    }
}

/// Query builder for fluent API
#[derive(Debug, Clone)]
pub struct Query {
    plan: QueryPlan,
}

impl Query {
    /// Create SELECT query for a table
    pub fn select(table: impl Into<String>) -> Self {
        Self {
            plan: QueryPlan::new(table),
        }
    }

    /// Select specific columns
    pub fn columns(mut self, columns: Vec<impl Into<String>>) -> Self {
        self.plan.columns = columns.into_iter().map(|c| c.into()).collect();
        self
    }

    /// Add WHERE filter
    pub fn filter(mut self, filter: Filter) -> Self {
        self.plan = self.plan.filter(filter);
        self
    }

    /// Add ORDER BY ascending
    pub fn order_by_asc(mut self, column: impl Into<String>) -> Self {
        self.plan.order_by = self.plan.order_by.asc(column);
        self
    }

    /// Add ORDER BY descending
    pub fn order_by_desc(mut self, column: impl Into<String>) -> Self {
        self.plan.order_by = self.plan.order_by.desc(column);
        self
    }

    /// Set LIMIT
    pub fn limit(mut self, n: usize) -> Self {
        self.plan = self.plan.limit(n);
        self
    }

    /// Set OFFSET
    pub fn offset(mut self, n: usize) -> Self {
        self.plan = self.plan.offset(n);
        self
    }

    /// Enable total count
    pub fn with_total_count(mut self) -> Self {
        self.plan = self.plan.with_total_count();
        self
    }

    /// Get the query plan
    pub fn plan(self) -> QueryPlan {
        self.plan
    }
}

/// Query executor trait
pub trait QueryExecutor {
    /// Execute a query plan
    fn execute(&self, plan: &QueryPlan) -> Result<QueryResult, QueryError>;

    /// Count rows matching filter
    fn count(&self, table: &str, filter: Option<&Filter>) -> Result<usize, QueryError>;

    /// Check if table exists
    fn table_exists(&self, table: &str) -> bool;

    /// Get table column names
    fn table_columns(&self, table: &str) -> Option<Vec<String>>;
}

/// Query execution error
#[derive(Debug, Clone)]
pub enum QueryError {
    /// Table not found
    TableNotFound(String),
    /// Column not found
    ColumnNotFound(String),
    /// Invalid filter
    InvalidFilter(String),
    /// Type mismatch
    TypeMismatch(String),
    /// Storage error
    StorageError(String),
    /// Query timeout
    Timeout,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::TableNotFound(t) => write!(f, "Table not found: {}", t),
            QueryError::ColumnNotFound(c) => write!(f, "Column not found: {}", c),
            QueryError::InvalidFilter(msg) => write!(f, "Invalid filter: {}", msg),
            QueryError::TypeMismatch(msg) => write!(f, "Type mismatch: {}", msg),
            QueryError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            QueryError::Timeout => write!(f, "Query timeout"),
        }
    }
}

impl std::error::Error for QueryError {}

/// In-memory query executor for testing and simple use cases
pub struct MemoryExecutor {
    /// Tables: name -> (columns, rows)
    tables: HashMap<String, (Vec<String>, Vec<Row>)>,
}

impl MemoryExecutor {
    /// Create a new memory executor
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
        }
    }

    /// Add a table
    pub fn add_table(&mut self, name: impl Into<String>, columns: Vec<String>, rows: Vec<Row>) {
        self.tables.insert(name.into(), (columns, rows));
    }

    /// Insert a row
    pub fn insert(&mut self, table: &str, row: Row) -> bool {
        if let Some((_, rows)) = self.tables.get_mut(table) {
            rows.push(row);
            true
        } else {
            false
        }
    }

    /// Get value from row by column index
    fn get_row_value(&self, row: &Row, columns: &[String], column: &str) -> Value {
        if let Some(idx) = columns.iter().position(|c| c == column) {
            row.get(idx).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    }
}

impl Default for MemoryExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryExecutor for MemoryExecutor {
    fn execute(&self, plan: &QueryPlan) -> Result<QueryResult, QueryError> {
        let start = std::time::Instant::now();

        // Get table
        let (columns, rows) = self
            .tables
            .get(&plan.table)
            .ok_or_else(|| QueryError::TableNotFound(plan.table.clone()))?;

        let mut stats = QueryStats::default();
        stats.rows_scanned = rows.len();

        // Filter rows
        let mut matched_rows: Vec<Row> = rows
            .iter()
            .filter(|row| {
                if let Some(filter) = &plan.filter {
                    filter.evaluate(&|col| {
                        if let Some(idx) = columns.iter().position(|c| c == col) {
                            row.get(idx).cloned()
                        } else {
                            None
                        }
                    })
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        stats.rows_matched = matched_rows.len();
        let total_count = if plan.count_total {
            Some(matched_rows.len())
        } else {
            None
        };

        // Sort
        if !plan.order_by.is_empty() {
            plan.order_by.sort_rows(&mut matched_rows, |row, col| {
                self.get_row_value(row, columns, col)
            });
        }

        // Apply limits
        let result_rows = plan.limits.apply(matched_rows);
        stats.rows_returned = result_rows.len();

        // Select columns
        let result_columns = if plan.columns.is_empty() {
            columns.clone()
        } else {
            // Validate columns exist
            for col in &plan.columns {
                if !columns.contains(col) {
                    return Err(QueryError::ColumnNotFound(col.clone()));
                }
            }

            // Project rows to selected columns
            let col_indices: Vec<usize> = plan
                .columns
                .iter()
                .filter_map(|c| columns.iter().position(|col| col == c))
                .collect();

            let projected_rows: Vec<Row> = result_rows
                .into_iter()
                .map(|row| {
                    let projected_values: Vec<Value> = col_indices
                        .iter()
                        .map(|&idx| row.get(idx).cloned().unwrap_or(Value::Null))
                        .collect();
                    Row::new(projected_values)
                })
                .collect();

            stats.rows_returned = projected_rows.len();
            stats.execution_time_us = start.elapsed().as_micros() as u64;

            return Ok(QueryResult {
                columns: plan.columns.clone(),
                rows: projected_rows,
                total_count,
                stats,
            });
        };

        stats.execution_time_us = start.elapsed().as_micros() as u64;

        Ok(QueryResult {
            columns: result_columns,
            rows: result_rows,
            total_count,
            stats,
        })
    }

    fn count(&self, table: &str, filter: Option<&Filter>) -> Result<usize, QueryError> {
        let (columns, rows) = self
            .tables
            .get(table)
            .ok_or_else(|| QueryError::TableNotFound(table.to_string()))?;

        let count = rows
            .iter()
            .filter(|row| {
                if let Some(filter) = filter {
                    filter.evaluate(&|col| {
                        if let Some(idx) = columns.iter().position(|c| c == col) {
                            row.get(idx).cloned()
                        } else {
                            None
                        }
                    })
                } else {
                    true
                }
            })
            .count();

        Ok(count)
    }

    fn table_exists(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    fn table_columns(&self, table: &str) -> Option<Vec<String>> {
        self.tables.get(table).map(|(cols, _)| cols.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_executor() -> MemoryExecutor {
        let mut executor = MemoryExecutor::new();

        let columns = vec![
            "id".to_string(),
            "name".to_string(),
            "age".to_string(),
            "department".to_string(),
        ];

        let rows = vec![
            Row::new(vec![
                Value::Integer(1),
                Value::Text("Alice".to_string()),
                Value::Integer(30),
                Value::Text("Engineering".to_string()),
            ]),
            Row::new(vec![
                Value::Integer(2),
                Value::Text("Bob".to_string()),
                Value::Integer(25),
                Value::Text("Sales".to_string()),
            ]),
            Row::new(vec![
                Value::Integer(3),
                Value::Text("Charlie".to_string()),
                Value::Integer(35),
                Value::Text("Engineering".to_string()),
            ]),
            Row::new(vec![
                Value::Integer(4),
                Value::Text("Diana".to_string()),
                Value::Integer(28),
                Value::Text("HR".to_string()),
            ]),
            Row::new(vec![
                Value::Integer(5),
                Value::Text("Eve".to_string()),
                Value::Integer(32),
                Value::Text("Engineering".to_string()),
            ]),
        ];

        executor.add_table("employees", columns, rows);
        executor
    }

    #[test]
    fn test_simple_select() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees");
        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 5);
        assert_eq!(result.columns.len(), 4);
    }

    #[test]
    fn test_select_columns() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").select(vec!["name".to_string(), "age".to_string()]);

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.columns, vec!["name", "age"]);
        assert_eq!(result.rows[0].len(), 2);
    }

    #[test]
    fn test_filter_eq() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").filter(Filter::eq(
            "department",
            Value::Text("Engineering".to_string()),
        ));

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 3); // Alice, Charlie, Eve
    }

    #[test]
    fn test_filter_gt() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").filter(Filter::gt("age", Value::Integer(30)));

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 2); // Charlie (35), Eve (32)
    }

    #[test]
    fn test_filter_and() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees")
            .filter(Filter::eq(
                "department",
                Value::Text("Engineering".to_string()),
            ))
            .filter(Filter::ge("age", Value::Integer(30)));

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 3); // Alice (30), Charlie (35), Eve (32)
    }

    #[test]
    fn test_order_by() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").order_by(OrderBy::new().asc("age"));

        let result = executor.execute(&plan).unwrap();

        // Should be ordered: Bob (25), Diana (28), Alice (30), Eve (32), Charlie (35)
        assert_eq!(
            result.get_value(0, "name"),
            Some(&Value::Text("Bob".to_string()))
        );
        assert_eq!(
            result.get_value(4, "name"),
            Some(&Value::Text("Charlie".to_string()))
        );
    }

    #[test]
    fn test_order_by_desc() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").order_by(OrderBy::new().desc("age"));

        let result = executor.execute(&plan).unwrap();

        assert_eq!(
            result.get_value(0, "name"),
            Some(&Value::Text("Charlie".to_string()))
        );
        assert_eq!(
            result.get_value(4, "name"),
            Some(&Value::Text("Bob".to_string()))
        );
    }

    #[test]
    fn test_limit() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").limit(2);

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_offset() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees")
            .order_by(OrderBy::new().asc("id"))
            .offset(2);

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 3); // Skip first 2
        assert_eq!(result.get_value(0, "id"), Some(&Value::Integer(3)));
    }

    #[test]
    fn test_limit_offset() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees")
            .order_by(OrderBy::new().asc("id"))
            .offset(1)
            .limit(2);

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get_value(0, "id"), Some(&Value::Integer(2)));
        assert_eq!(result.get_value(1, "id"), Some(&Value::Integer(3)));
    }

    #[test]
    fn test_count() {
        let executor = create_test_executor();

        let count = executor.count("employees", None).unwrap();
        assert_eq!(count, 5);

        let filter = Filter::eq("department", Value::Text("Engineering".to_string()));
        let count = executor.count("employees", Some(&filter)).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_total_count() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees").limit(2).with_total_count();

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.total_count, Some(5));
    }

    #[test]
    fn test_table_not_found() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("nonexistent");
        let result = executor.execute(&plan);

        assert!(matches!(result, Err(QueryError::TableNotFound(_))));
    }

    #[test]
    fn test_column_not_found() {
        let executor = create_test_executor();

        let plan =
            QueryPlan::new("employees").select(vec!["name".to_string(), "nonexistent".to_string()]);

        let result = executor.execute(&plan);

        assert!(matches!(result, Err(QueryError::ColumnNotFound(_))));
    }

    #[test]
    fn test_query_builder() {
        let executor = create_test_executor();

        let query = Query::select("employees")
            .columns(vec!["name", "age"])
            .filter(Filter::gt("age", Value::Integer(28)))
            .order_by_desc("age")
            .limit(3);

        let result = executor.execute(&query.plan()).unwrap();

        assert_eq!(result.columns, vec!["name", "age"]);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_row_view() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees")
            .order_by(OrderBy::new().asc("id"))
            .limit(1);

        let result = executor.execute(&plan).unwrap();

        for row in result.iter_rows() {
            assert_eq!(row.get("name"), Some(&Value::Text("Alice".to_string())));
            assert_eq!(row.get("age"), Some(&Value::Integer(30)));
        }
    }

    #[test]
    fn test_stats() {
        let executor = create_test_executor();

        let plan = QueryPlan::new("employees")
            .filter(Filter::eq(
                "department",
                Value::Text("Engineering".to_string()),
            ))
            .limit(2);

        let result = executor.execute(&plan).unwrap();

        assert_eq!(result.stats.rows_scanned, 5);
        assert_eq!(result.stats.rows_matched, 3);
        assert_eq!(result.stats.rows_returned, 2);
    }

    #[test]
    fn test_insert() {
        let mut executor = create_test_executor();

        let new_row = Row::new(vec![
            Value::Integer(6),
            Value::Text("Frank".to_string()),
            Value::Integer(40),
            Value::Text("Legal".to_string()),
        ]);

        assert!(executor.insert("employees", new_row));

        let count = executor.count("employees", None).unwrap();
        assert_eq!(count, 6);
    }

    #[test]
    fn test_table_exists() {
        let executor = create_test_executor();

        assert!(executor.table_exists("employees"));
        assert!(!executor.table_exists("nonexistent"));
    }

    #[test]
    fn test_table_columns() {
        let executor = create_test_executor();

        let columns = executor.table_columns("employees").unwrap();
        assert_eq!(columns, vec!["id", "name", "age", "department"]);

        assert!(executor.table_columns("nonexistent").is_none());
    }
}
