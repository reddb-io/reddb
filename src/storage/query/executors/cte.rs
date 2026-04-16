//! CTE (Common Table Expression) Executor
//!
//! Executes WITH clauses including recursive CTEs for hierarchical queries.
//!
//! # Architecture
//!
//! CTEs are materialized as temporary result sets that can be referenced
//! in the main query. Recursive CTEs use iterative execution:
//!
//! 1. Execute the base (non-recursive) part
//! 2. Repeat: Execute recursive part with previous iteration's results
//! 3. Stop when no new rows are produced (fixpoint)
//!
//! # Security Use Cases
//!
//! Recursive CTEs are powerful for security analysis:
//! - Attack path enumeration (multi-hop lateral movement)
//! - Credential chain traversal
//! - Privilege escalation paths
//! - Blast radius calculation
//!
//! # Example
//!
//! ```ignore
//! WITH RECURSIVE attack_path AS (
//!     -- Base case: start from compromised host
//!     SELECT id, ip, 0 as depth FROM hosts WHERE compromised = true
//!     UNION ALL
//!     -- Recursive case: find reachable hosts
//!     SELECT h.id, h.ip, ap.depth + 1
//!     FROM attack_path ap
//!     JOIN host_connections c ON ap.id = c.source_id
//!     JOIN hosts h ON c.target_id = h.id
//!     WHERE ap.depth < 5
//! )
//! SELECT * FROM attack_path
//! ```

use std::collections::{HashMap, HashSet};

use super::super::ast::{CteDefinition, QueryExpr, QueryWithCte};
use super::super::unified::{ExecutionError, UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;

/// Maximum recursion depth to prevent infinite loops
const MAX_RECURSION_DEPTH: usize = 1000;

/// Maximum total rows across all iterations
const MAX_RECURSIVE_ROWS: usize = 100_000;

/// CTE execution context holding materialized CTE results
#[derive(Debug, Clone, Default)]
pub struct CteContext {
    /// Materialized CTE results by name
    tables: HashMap<String, UnifiedResult>,
    /// Track which CTEs are currently being evaluated (for cycle detection)
    evaluating: HashSet<String>,
    /// Statistics
    stats: CteStats,
}

impl CteContext {
    /// Create a new CTE context
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a materialized CTE result by name
    pub fn get(&self, name: &str) -> Option<&UnifiedResult> {
        self.tables.get(name)
    }

    /// Store a materialized CTE result
    pub fn store(&mut self, name: String, result: UnifiedResult) {
        self.tables.insert(name, result);
    }

    /// Check if a CTE is being evaluated (for recursion detection)
    pub fn is_evaluating(&self, name: &str) -> bool {
        self.evaluating.contains(name)
    }

    /// Mark a CTE as being evaluated
    pub fn start_evaluating(&mut self, name: &str) {
        self.evaluating.insert(name.to_string());
    }

    /// Mark a CTE as done evaluating
    pub fn done_evaluating(&mut self, name: &str) {
        self.evaluating.remove(name);
    }

    /// Get execution statistics
    pub fn stats(&self) -> &CteStats {
        &self.stats
    }
}

/// Statistics about CTE execution
#[derive(Debug, Clone, Default)]
pub struct CteStats {
    /// Number of CTEs executed
    pub ctes_executed: usize,
    /// Number of recursive iterations
    pub recursive_iterations: usize,
    /// Total rows produced by CTEs
    pub rows_produced: usize,
    /// Execution time in microseconds
    pub exec_time_us: u64,
}

/// CTE Executor
pub struct CteExecutor<F>
where
    F: Fn(&QueryExpr, &CteContext) -> Result<UnifiedResult, ExecutionError>,
{
    /// Function to execute a query with CTE context
    execute_fn: F,
}

impl<F> CteExecutor<F>
where
    F: Fn(&QueryExpr, &CteContext) -> Result<UnifiedResult, ExecutionError>,
{
    /// Create a new CTE executor
    pub fn new(execute_fn: F) -> Self {
        Self { execute_fn }
    }

    /// Execute a query with CTEs
    pub fn execute(&self, query: &QueryWithCte) -> Result<UnifiedResult, ExecutionError> {
        let start = std::time::Instant::now();
        let mut ctx = CteContext::new();

        // Materialize all CTEs in order
        if let Some(ref with_clause) = query.with_clause {
            for cte in &with_clause.ctes {
                self.materialize_cte(cte, &mut ctx)?;
            }
        }

        // Execute the main query with CTE context
        let result = (self.execute_fn)(&query.query, &ctx)?;

        ctx.stats.exec_time_us = start.elapsed().as_micros() as u64;
        Ok(result)
    }

    /// Materialize a single CTE
    fn materialize_cte(
        &self,
        cte: &CteDefinition,
        ctx: &mut CteContext,
    ) -> Result<(), ExecutionError> {
        if ctx.is_evaluating(&cte.name) {
            return Err(ExecutionError::new(format!(
                "Circular CTE reference: {}",
                cte.name
            )));
        }

        // Check if already materialized
        if ctx.get(&cte.name).is_some() {
            return Ok(());
        }

        ctx.start_evaluating(&cte.name);

        let result = if cte.recursive {
            self.execute_recursive_cte(cte, ctx)?
        } else {
            // Simple CTE: execute once
            let result = (self.execute_fn)(&cte.query, ctx)?;
            self.project_columns(&result, &cte.columns)
        };

        ctx.stats.ctes_executed += 1;
        ctx.stats.rows_produced += result.len();
        ctx.store(cte.name.clone(), result);
        ctx.done_evaluating(&cte.name);

        Ok(())
    }

    /// Execute a recursive CTE using iterative fixpoint
    fn execute_recursive_cte(
        &self,
        cte: &CteDefinition,
        ctx: &mut CteContext,
    ) -> Result<UnifiedResult, ExecutionError> {
        // For recursive CTEs, we need to handle UNION ALL structure
        // The query should be: base_query UNION ALL recursive_query
        //
        // Algorithm:
        // 1. Execute base query -> working_table
        // 2. result_table = working_table
        // 3. While working_table not empty:
        //    a. Execute recursive query with CTE = working_table
        //    b. new_rows = result - already_seen
        //    c. working_table = new_rows
        //    d. result_table += new_rows
        // 4. Return result_table

        // For simplicity in first implementation, we execute the full query
        // iteratively, building up the working table

        let mut all_results = UnifiedResult::with_columns(cte.columns.clone());
        let mut working_table = UnifiedResult::with_columns(cte.columns.clone());
        let mut seen_rows: HashSet<u64> = HashSet::new();
        let mut iteration = 0;

        // First iteration: execute the full query (base case)
        let initial = (self.execute_fn)(&cte.query, ctx)?;
        let initial = self.project_columns(&initial, &cte.columns);

        for record in &initial.records {
            let hash = self.hash_record(record);
            if seen_rows.insert(hash) {
                working_table.push(record.clone());
                all_results.push(record.clone());
            }
        }

        // Store initial results so recursive references can see them
        ctx.store(cte.name.clone(), working_table.clone());

        // Iterate until fixpoint
        while !working_table.is_empty() && iteration < MAX_RECURSION_DEPTH {
            iteration += 1;
            ctx.stats.recursive_iterations += 1;

            if all_results.len() > MAX_RECURSIVE_ROWS {
                return Err(ExecutionError::new(format!(
                    "Recursive CTE '{}' exceeded maximum rows ({})",
                    cte.name, MAX_RECURSIVE_ROWS
                )));
            }

            // Execute query with current CTE contents
            let new_results = (self.execute_fn)(&cte.query, ctx)?;
            let new_results = self.project_columns(&new_results, &cte.columns);

            // Find genuinely new rows
            let mut new_working_table = UnifiedResult::with_columns(cte.columns.clone());
            for record in &new_results.records {
                let hash = self.hash_record(record);
                if seen_rows.insert(hash) {
                    new_working_table.push(record.clone());
                    all_results.push(record.clone());
                }
            }

            working_table = new_working_table;

            // Update CTE table for next iteration
            ctx.store(cte.name.clone(), all_results.clone());
        }

        if iteration >= MAX_RECURSION_DEPTH && !working_table.is_empty() {
            return Err(ExecutionError::new(format!(
                "Recursive CTE '{}' exceeded maximum recursion depth ({})",
                cte.name, MAX_RECURSION_DEPTH
            )));
        }

        Ok(all_results)
    }

    /// Project result columns according to CTE column list
    fn project_columns(&self, result: &UnifiedResult, columns: &[String]) -> UnifiedResult {
        if columns.is_empty() {
            return result.clone();
        }

        let mut projected = UnifiedResult::with_columns(columns.to_vec());

        for record in &result.records {
            let mut new_record = UnifiedRecord::new();

            // Map result columns to CTE columns
            for (i, col) in columns.iter().enumerate() {
                // Try to find value by position first, then by name
                let value = result
                    .columns
                    .get(i)
                    .and_then(|orig_col| record.get(orig_col))
                    .cloned()
                    .or_else(|| record.get(col).cloned())
                    .unwrap_or(Value::Null);

                new_record.set(col, value);
            }

            projected.push(new_record);
        }

        projected
    }

    /// Hash a record for deduplication
    fn hash_record(&self, record: &UnifiedRecord) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        // Hash all values in deterministic order
        let mut keys: Vec<_> = record.values.keys().collect();
        keys.sort();

        for key in keys {
            key.hash(&mut hasher);
            if let Some(value) = record.values.get(key) {
                Self::hash_value(value, &mut hasher);
            }
        }

        hasher.finish()
    }

    /// Hash a Value for deduplication
    fn hash_value(value: &Value, hasher: &mut impl std::hash::Hasher) {
        use std::hash::Hash;

        match value {
            Value::Null => 0u8.hash(hasher),
            Value::Boolean(b) => {
                1u8.hash(hasher);
                b.hash(hasher);
            }
            Value::Integer(i) => {
                2u8.hash(hasher);
                i.hash(hasher);
            }
            Value::UnsignedInteger(u) => {
                3u8.hash(hasher);
                u.hash(hasher);
            }
            Value::Float(f) => {
                4u8.hash(hasher);
                f.to_bits().hash(hasher);
            }
            Value::Text(s) => {
                5u8.hash(hasher);
                s.hash(hasher);
            }
            Value::Blob(b) => {
                6u8.hash(hasher);
                b.hash(hasher);
            }
            Value::Timestamp(t) => {
                7u8.hash(hasher);
                t.hash(hasher);
            }
            Value::Duration(d) => {
                8u8.hash(hasher);
                d.hash(hasher);
            }
            Value::IpAddr(addr) => {
                9u8.hash(hasher);
                match addr {
                    std::net::IpAddr::V4(v4) => v4.octets().hash(hasher),
                    std::net::IpAddr::V6(v6) => v6.octets().hash(hasher),
                }
            }
            Value::MacAddr(mac) => {
                10u8.hash(hasher);
                mac.hash(hasher);
            }
            Value::Vector(v) => {
                11u8.hash(hasher);
                v.len().hash(hasher);
                for f in v {
                    f.to_bits().hash(hasher);
                }
            }
            Value::Json(j) => {
                12u8.hash(hasher);
                j.hash(hasher);
            }
            Value::Uuid(u) => {
                13u8.hash(hasher);
                u.hash(hasher);
            }
            Value::NodeRef(n) => {
                14u8.hash(hasher);
                n.hash(hasher);
            }
            Value::EdgeRef(e) => {
                15u8.hash(hasher);
                e.hash(hasher);
            }
            Value::VectorRef(coll, id) => {
                16u8.hash(hasher);
                coll.hash(hasher);
                id.hash(hasher);
            }
            Value::RowRef(table, id) => {
                17u8.hash(hasher);
                table.hash(hasher);
                id.hash(hasher);
            }
            Value::Color(rgb) => {
                18u8.hash(hasher);
                rgb.hash(hasher);
            }
            Value::Email(s) => {
                19u8.hash(hasher);
                s.hash(hasher);
            }
            Value::Url(s) => {
                20u8.hash(hasher);
                s.hash(hasher);
            }
            Value::Phone(n) => {
                21u8.hash(hasher);
                n.hash(hasher);
            }
            Value::Semver(v) => {
                22u8.hash(hasher);
                v.hash(hasher);
            }
            Value::Cidr(ip, prefix) => {
                23u8.hash(hasher);
                ip.hash(hasher);
                prefix.hash(hasher);
            }
            Value::Date(d) => {
                24u8.hash(hasher);
                d.hash(hasher);
            }
            Value::Time(t) => {
                25u8.hash(hasher);
                t.hash(hasher);
            }
            Value::Decimal(v) => {
                26u8.hash(hasher);
                v.hash(hasher);
            }
            Value::EnumValue(i) => {
                27u8.hash(hasher);
                i.hash(hasher);
            }
            Value::Array(elems) => {
                28u8.hash(hasher);
                elems.len().hash(hasher);
                for elem in elems {
                    Self::hash_value(elem, hasher);
                }
            }
            Value::TimestampMs(v) => {
                29u8.hash(hasher);
                v.hash(hasher);
            }
            Value::Ipv4(v) => {
                30u8.hash(hasher);
                v.hash(hasher);
            }
            Value::Ipv6(bytes) => {
                31u8.hash(hasher);
                bytes.hash(hasher);
            }
            Value::Subnet(ip, mask) => {
                32u8.hash(hasher);
                ip.hash(hasher);
                mask.hash(hasher);
            }
            Value::Port(v) => {
                33u8.hash(hasher);
                v.hash(hasher);
            }
            Value::Latitude(v) => {
                34u8.hash(hasher);
                v.hash(hasher);
            }
            Value::Longitude(v) => {
                35u8.hash(hasher);
                v.hash(hasher);
            }
            Value::GeoPoint(lat, lon) => {
                36u8.hash(hasher);
                lat.hash(hasher);
                lon.hash(hasher);
            }
            Value::Country2(c) => {
                37u8.hash(hasher);
                c.hash(hasher);
            }
            Value::Country3(c) => {
                38u8.hash(hasher);
                c.hash(hasher);
            }
            Value::Lang2(c) => {
                39u8.hash(hasher);
                c.hash(hasher);
            }
            Value::Lang5(c) => {
                40u8.hash(hasher);
                c.hash(hasher);
            }
            Value::Currency(c) => {
                41u8.hash(hasher);
                c.hash(hasher);
            }
            Value::AssetCode(code) => {
                50u8.hash(hasher);
                code.hash(hasher);
            }
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => {
                51u8.hash(hasher);
                asset_code.hash(hasher);
                minor_units.hash(hasher);
                scale.hash(hasher);
            }
            Value::ColorAlpha(rgba) => {
                42u8.hash(hasher);
                rgba.hash(hasher);
            }
            Value::BigInt(v) => {
                43u8.hash(hasher);
                v.hash(hasher);
            }
            Value::KeyRef(col, key) => {
                44u8.hash(hasher);
                col.hash(hasher);
                key.hash(hasher);
            }
            Value::DocRef(col, id) => {
                45u8.hash(hasher);
                col.hash(hasher);
                id.hash(hasher);
            }
            Value::TableRef(name) => {
                46u8.hash(hasher);
                name.hash(hasher);
            }
            Value::PageRef(page_id) => {
                47u8.hash(hasher);
                page_id.hash(hasher);
            }
            Value::Secret(bytes) => {
                48u8.hash(hasher);
                bytes.hash(hasher);
            }
            Value::Password(hash) => {
                49u8.hash(hasher);
                hash.hash(hasher);
            }
        }
    }
}

/// Helper to parse UNION structure for recursive CTEs
pub fn split_union_parts(query: &QueryExpr) -> Option<(QueryExpr, QueryExpr)> {
    // UNION support is not represented in the current AST; recursive queries execute
    // the full body expression each iteration.
    let _ = query;
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::CteQueryBuilder;
    use crate::storage::query::WithClause;

    fn mock_execute(
        _query: &QueryExpr,
        _ctx: &CteContext,
    ) -> Result<UnifiedResult, ExecutionError> {
        // Simple mock that returns empty result
        Ok(UnifiedResult::empty())
    }

    #[test]
    fn test_cte_context() {
        let mut ctx = CteContext::new();

        // Test empty context
        assert!(ctx.get("test").is_none());
        assert!(!ctx.is_evaluating("test"));

        // Test storing results
        let result = UnifiedResult::with_columns(vec!["col1".to_string()]);
        ctx.store("test".to_string(), result);
        assert!(ctx.get("test").is_some());

        // Test evaluation tracking
        ctx.start_evaluating("other");
        assert!(ctx.is_evaluating("other"));
        ctx.done_evaluating("other");
        assert!(!ctx.is_evaluating("other"));
    }

    #[test]
    fn test_simple_cte_execution() {
        let executor = CteExecutor::new(|_query, _ctx| {
            let mut result = UnifiedResult::with_columns(vec!["id".to_string()]);
            let mut record = UnifiedRecord::new();
            record.set("id", Value::Integer(1));
            result.push(record);
            Ok(result)
        });

        // Create a simple CTE query
        let cte = CteDefinition {
            name: "test_cte".to_string(),
            columns: vec!["id".to_string()],
            query: Box::new(QueryExpr::table("dummy").build()),
            recursive: false,
        };

        let with_clause = WithClause::new().add(cte);
        let query = QueryWithCte::with_ctes(with_clause, QueryExpr::table("test_cte").build());

        let result = executor.execute(&query);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cte_builder() {
        let query = CteQueryBuilder::new()
            .cte_with_columns(
                "nums",
                vec!["n".to_string()],
                QueryExpr::table("numbers").build(),
            )
            .build(QueryExpr::table("nums").build());

        assert!(query.with_clause.is_some());
        let with_clause = query.with_clause.unwrap();
        assert_eq!(with_clause.ctes.len(), 1);
        assert_eq!(with_clause.ctes[0].name, "nums");
    }

    #[test]
    fn test_recursive_cte_builder() {
        let query = CteQueryBuilder::new()
            .recursive_cte("paths", QueryExpr::table("connections").build())
            .build(QueryExpr::table("paths").build());

        assert!(query.with_clause.is_some());
        let with_clause = query.with_clause.unwrap();
        assert!(with_clause.has_recursive);
        assert!(with_clause.ctes[0].recursive);
    }

    #[test]
    fn test_circular_reference_detection() {
        let mut ctx = CteContext::new();
        ctx.start_evaluating("cte_a");

        // Simulate trying to evaluate cte_a while it's being evaluated
        assert!(ctx.is_evaluating("cte_a"));
    }

    #[test]
    fn test_cte_stats() {
        let ctx = CteContext::new();
        let stats = ctx.stats();

        assert_eq!(stats.ctes_executed, 0);
        assert_eq!(stats.recursive_iterations, 0);
        assert_eq!(stats.rows_produced, 0);
    }

    #[test]
    fn test_hash_record() {
        let executor = CteExecutor::new(mock_execute);

        let mut record1 = UnifiedRecord::new();
        record1.set("id", Value::Integer(1));
        record1.set("name", Value::Text("test".to_string()));

        let mut record2 = UnifiedRecord::new();
        record2.set("id", Value::Integer(1));
        record2.set("name", Value::Text("test".to_string()));

        let mut record3 = UnifiedRecord::new();
        record3.set("id", Value::Integer(2));
        record3.set("name", Value::Text("test".to_string()));

        // Same content should have same hash
        assert_eq!(
            executor.hash_record(&record1),
            executor.hash_record(&record2)
        );

        // Different content should have different hash
        assert_ne!(
            executor.hash_record(&record1),
            executor.hash_record(&record3)
        );
    }

    #[test]
    fn test_hash_various_value_types() {
        let executor = CteExecutor::new(mock_execute);

        // Test hashing different value types
        let mut record = UnifiedRecord::new();
        record.set("null_val", Value::Null);
        record.set("bool_val", Value::Boolean(true));
        record.set("int_val", Value::Integer(42));
        record.set("float_val", Value::Float(2.5));
        record.set("text_val", Value::Text("hello".to_string()));
        record.set("blob_val", Value::Blob(vec![1, 2, 3]));
        record.set("timestamp_val", Value::Timestamp(1234567890));
        record.set("duration_val", Value::Duration(5000));

        // Should not panic
        let hash = executor.hash_record(&record);
        assert!(hash > 0);
    }

    #[test]
    fn test_project_columns() {
        let executor = CteExecutor::new(mock_execute);

        let mut original =
            UnifiedResult::with_columns(vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        let mut record = UnifiedRecord::new();
        record.set("a", Value::Integer(1));
        record.set("b", Value::Integer(2));
        record.set("c", Value::Integer(3));
        original.push(record);

        // Project to different column names
        let projected = executor.project_columns(&original, &["x".to_string(), "y".to_string()]);

        assert_eq!(projected.columns, vec!["x", "y"]);
        assert_eq!(projected.len(), 1);
    }

    #[test]
    fn test_empty_columns_projection() {
        let executor = CteExecutor::new(mock_execute);

        let original = UnifiedResult::with_columns(vec!["a".to_string()]);

        // Empty columns should return original
        let projected = executor.project_columns(&original, &[]);
        assert_eq!(projected.columns, original.columns);
    }

    #[test]
    fn test_cte_with_multiple_definitions() {
        let executor = CteExecutor::new(|query, ctx| {
            // Return different results based on which CTE is being queried
            match query {
                QueryExpr::Table(t) if t.table == "base" => {
                    let mut result = UnifiedResult::with_columns(vec!["id".to_string()]);
                    let mut record = UnifiedRecord::new();
                    record.set("id", Value::Integer(1));
                    result.push(record);
                    Ok(result)
                }
                QueryExpr::Table(t) if t.table == "cte1" => {
                    // Should be able to see cte1 in context
                    if ctx.get("cte1").is_some() {
                        Ok(ctx.get("cte1").unwrap().clone())
                    } else {
                        Ok(UnifiedResult::empty())
                    }
                }
                _ => Ok(UnifiedResult::empty()),
            }
        });

        let cte1 = CteDefinition {
            name: "cte1".to_string(),
            columns: vec!["id".to_string()],
            query: Box::new(QueryExpr::table("base").build()),
            recursive: false,
        };

        let cte2 = CteDefinition {
            name: "cte2".to_string(),
            columns: vec!["id".to_string()],
            query: Box::new(QueryExpr::table("cte1").build()),
            recursive: false,
        };

        let with_clause = WithClause::new().add(cte1).add(cte2);
        let query = QueryWithCte::with_ctes(with_clause, QueryExpr::table("cte2").build());

        let result = executor.execute(&query);
        assert!(result.is_ok());
    }
}
