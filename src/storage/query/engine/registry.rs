//! Query Engine Registry
//!
//! Central registry for query execution engines.
//!
//! # Design
//!
//! - Factory pattern for creating engines
//! - Registry for engine lookup by name
//! - Engine abstraction over different backends

use super::binding::{Binding, Value, Var};
use super::iterator::{
    BindingIterator, IterError, QueryIter, QueryIterBase, QueryIterDistinct, QueryIterFilter,
    QueryIterJoin, QueryIterProject, QueryIterSlice, QueryIterSort, QueryIterUnion, SortKey,
};
use super::op::*;
use super::transform::{transform_op, OpStats, TransformPushFilter};
use crate::storage::query::executors::{
    Aggregator, AvgAggregator, CountAggregator, CountDistinctAggregator, GroupConcatAggregator,
    MaxAggregator, MinAggregator, SampleAggregator, SumAggregator,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Query execution context
#[derive(Debug, Clone)]
pub struct QueryContext {
    /// Query timeout
    pub timeout: Option<Duration>,
    /// Maximum results
    pub limit: Option<u64>,
    /// Optimization level (0 = none, 1 = basic, 2 = aggressive)
    pub optimization_level: u8,
    /// Collect statistics
    pub collect_stats: bool,
    /// Custom parameters
    pub params: HashMap<String, Value>,
}

impl QueryContext {
    /// Create default context
    pub fn new() -> Self {
        Self {
            timeout: Some(Duration::from_secs(60)),
            limit: None,
            optimization_level: 1,
            collect_stats: false,
            params: HashMap::new(),
        }
    }

    /// Set timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set limit
    pub fn with_limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set optimization level
    pub fn with_optimization(mut self, level: u8) -> Self {
        self.optimization_level = level;
        self
    }

    /// Enable statistics collection
    pub fn with_stats(mut self) -> Self {
        self.collect_stats = true;
        self
    }

    /// Add parameter
    pub fn with_param(mut self, name: &str, value: Value) -> Self {
        self.params.insert(name.to_string(), value);
        self
    }
}

impl Default for QueryContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Query execution statistics
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    /// Planning time
    pub planning_time: Duration,
    /// Execution time
    pub execution_time: Duration,
    /// Result count
    pub result_count: u64,
    /// Bindings processed
    pub bindings_processed: u64,
    /// Join operations
    pub join_count: u64,
    /// Filter operations
    pub filter_count: u64,
    /// Cache hits
    pub cache_hits: u64,
    /// Index lookups
    pub index_lookups: u64,
}

impl ExecutionStats {
    /// Create new stats
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge with another stats
    pub fn merge(&mut self, other: &ExecutionStats) {
        self.planning_time += other.planning_time;
        self.execution_time += other.execution_time;
        self.result_count += other.result_count;
        self.bindings_processed += other.bindings_processed;
        self.join_count += other.join_count;
        self.filter_count += other.filter_count;
        self.cache_hits += other.cache_hits;
        self.index_lookups += other.index_lookups;
    }
}

/// Query execution result
pub struct QueryResult {
    /// Result iterator
    pub iter: QueryIter,
    /// Execution statistics
    pub stats: Option<ExecutionStats>,
}

impl QueryResult {
    /// Create result
    pub fn new(iter: QueryIter) -> Self {
        Self { iter, stats: None }
    }

    /// Create result with stats
    pub fn with_stats(iter: QueryIter, stats: ExecutionStats) -> Self {
        Self {
            iter,
            stats: Some(stats),
        }
    }

    /// Collect all results
    pub fn collect(self) -> Result<Vec<Binding>, IterError> {
        self.iter.collect()
    }

    /// Get first result
    pub fn first(mut self) -> Result<Option<Binding>, IterError> {
        self.iter.next().transpose()
    }

    /// Get statistics
    pub fn statistics(&self) -> Option<&ExecutionStats> {
        self.stats.as_ref()
    }
}

/// Query engine trait
pub trait QueryEngine: Send + Sync {
    /// Engine name
    fn name(&self) -> &str;

    /// Execute an Op tree
    fn execute(&self, op: Op, ctx: &QueryContext) -> Result<QueryResult, EngineError>;

    /// Optimize an Op tree
    fn optimize(&self, op: Op, level: u8) -> Op {
        if level == 0 {
            return op;
        }

        // Apply standard optimizations
        let mut push_filter = TransformPushFilter::new();
        transform_op(&mut push_filter, op)
    }

    /// Get engine capabilities
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::default()
    }
}

/// Engine capabilities
#[derive(Debug, Clone, Default)]
pub struct EngineCapabilities {
    /// Supports graph patterns
    pub graph_patterns: bool,
    /// Supports aggregation
    pub aggregation: bool,
    /// Supports subqueries
    pub subqueries: bool,
    /// Supports property paths
    pub property_paths: bool,
    /// Supports updates
    pub updates: bool,
    /// Supports transactions
    pub transactions: bool,
}

/// Engine errors
#[derive(Debug, Clone)]
pub enum EngineError {
    /// Unsupported operation
    Unsupported(String),
    /// Execution error
    Execution(String),
    /// Timeout
    Timeout,
    /// Invalid query
    InvalidQuery(String),
    /// Resource limit exceeded
    ResourceLimit(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Unsupported(msg) => write!(f, "Unsupported operation: {}", msg),
            EngineError::Execution(msg) => write!(f, "Execution error: {}", msg),
            EngineError::Timeout => write!(f, "Query timeout"),
            EngineError::InvalidQuery(msg) => write!(f, "Invalid query: {}", msg),
            EngineError::ResourceLimit(msg) => write!(f, "Resource limit: {}", msg),
        }
    }
}

impl std::error::Error for EngineError {}

/// Factory for creating query engines
pub trait QueryEngineFactory: Send + Sync {
    /// Factory name
    fn name(&self) -> &str;

    /// Create engine instance
    fn create(&self) -> Box<dyn QueryEngine>;

    /// Check if factory can create engine for this context
    fn accepts(&self, _ctx: &QueryContext) -> bool {
        true
    }
}

/// Engine registry
pub struct QueryEngineRegistry {
    /// Registered factories
    factories: HashMap<String, Box<dyn QueryEngineFactory>>,
    /// Default engine name
    default_engine: Option<String>,
}

mod query_registry_impl;

impl Default for QueryEngineRegistry {
    fn default() -> Self {
        Self::with_default()
    }
}

/// In-memory query engine
pub struct InMemoryEngine {
    /// Data store (BGP patterns map to bindings)
    data: Arc<HashMap<String, Vec<Binding>>>,
}

mod in_memory_impl;
fn bindings_share_vars(left: &Binding, right: &Binding) -> bool {
    left.all_vars().iter().any(|var| right.contains(var))
}

fn bindings_compatible(left: &Binding, right: &Binding) -> bool {
    left.all_vars().iter().all(|var| {
        if right.contains(var) {
            left.get(var) == right.get(var)
        } else {
            true
        }
    })
}

impl Clone for InMemoryEngine {
    fn clone(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
        }
    }
}

impl Default for InMemoryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryEngine for InMemoryEngine {
    fn name(&self) -> &str {
        "memory"
    }

    fn execute(&self, op: Op, ctx: &QueryContext) -> Result<QueryResult, EngineError> {
        let start = Instant::now();

        // Optimize if requested
        let optimized = if ctx.optimization_level > 0 {
            self.optimize(op, ctx.optimization_level)
        } else {
            op
        };

        let planning_time = start.elapsed();

        // Execute
        let exec_start = Instant::now();
        let iter = self.execute_op(&optimized);

        // Apply context limit if set
        let iter: Box<dyn BindingIterator> = if let Some(limit) = ctx.limit {
            Box::new(QueryIterSlice::limit(iter, limit))
        } else {
            iter
        };

        let query_iter = QueryIter::new(iter);

        let mut stats = ExecutionStats::new();
        stats.planning_time = planning_time;
        stats.execution_time = exec_start.elapsed();

        // Collect op statistics
        let op_stats = OpStats::collect(&optimized);
        stats.join_count = op_stats.join_count as u64;
        stats.filter_count = op_stats.filter_count as u64;

        if ctx.collect_stats {
            Ok(QueryResult::with_stats(query_iter, stats))
        } else {
            Ok(QueryResult::new(query_iter))
        }
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            graph_patterns: true,
            aggregation: false, // Not fully implemented
            subqueries: true,
            property_paths: false,
            updates: false,
            transactions: false,
        }
    }
}

/// Factory for in-memory engine
pub struct InMemoryEngineFactory;

impl QueryEngineFactory for InMemoryEngineFactory {
    fn name(&self) -> &str {
        "memory"
    }

    fn create(&self) -> Box<dyn QueryEngine> {
        Box::new(InMemoryEngine::new())
    }
}

/// Compute a hash for a binding (for set operations)
fn binding_hash(binding: &Binding) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Get sorted keys for deterministic ordering
    let mut vars: Vec<_> = binding.all_vars();
    vars.sort_by_key(|v| v.name());

    for var in vars {
        var.name().hash(&mut hasher);
        // Hash value based on type
        if let Some(value) = binding.get(var) {
            match value {
                Value::Node(id) => {
                    "node".hash(&mut hasher);
                    id.hash(&mut hasher);
                }
                Value::Edge(id) => {
                    "edge".hash(&mut hasher);
                    id.hash(&mut hasher);
                }
                Value::String(s) => {
                    "string".hash(&mut hasher);
                    s.hash(&mut hasher);
                }
                Value::Integer(i) => {
                    "int".hash(&mut hasher);
                    i.hash(&mut hasher);
                }
                Value::Float(f) => {
                    "float".hash(&mut hasher);
                    f.to_bits().hash(&mut hasher);
                }
                Value::Boolean(b) => {
                    "bool".hash(&mut hasher);
                    b.hash(&mut hasher);
                }
                Value::Uri(u) => {
                    "uri".hash(&mut hasher);
                    u.hash(&mut hasher);
                }
                Value::Null => {
                    "null".hash(&mut hasher);
                }
            }
        } else {
            "unbound".hash(&mut hasher);
        }
    }

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_context() {
        let ctx = QueryContext::new()
            .with_timeout(Duration::from_secs(30))
            .with_limit(100)
            .with_optimization(2)
            .with_stats();

        assert_eq!(ctx.timeout, Some(Duration::from_secs(30)));
        assert_eq!(ctx.limit, Some(100));
        assert_eq!(ctx.optimization_level, 2);
        assert!(ctx.collect_stats);
    }

    #[test]
    fn test_registry() {
        let registry = QueryEngineRegistry::with_default();
        assert!(registry.get("memory").is_some());
        assert!(registry.get_default().is_some());
    }

    #[test]
    fn test_in_memory_engine_empty() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let bgp = OpBGP::new();
        let result = engine.execute(Op::BGP(bgp), &ctx).unwrap();

        let bindings: Vec<_> = result.collect().unwrap();
        assert!(bindings.is_empty());
    }

    #[test]
    fn test_in_memory_engine_table() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x"), Var::new("y")],
            vec![
                vec![Some(Value::Integer(1)), Some(Value::Integer(2))],
                vec![Some(Value::Integer(3)), Some(Value::Integer(4))],
            ],
        );

        let result = engine.execute(Op::Table(table), &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 2);
    }

    #[test]
    fn test_in_memory_engine_filter() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x")],
            vec![
                vec![Some(Value::Integer(1))],
                vec![Some(Value::Integer(5))],
                vec![Some(Value::Integer(10))],
            ],
        );

        let filter = FilterExpr::Gt(
            ExprTerm::Var(Var::new("x")),
            ExprTerm::Const(Value::Integer(3)),
        );

        let op = Op::Filter(OpFilter::new(filter, Op::Table(table)));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 2); // 5 and 10
    }

    #[test]
    fn test_in_memory_engine_group() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("dept"), Var::new("salary")],
            vec![
                vec![
                    Some(Value::String("A".to_string())),
                    Some(Value::Integer(100)),
                ],
                vec![
                    Some(Value::String("A".to_string())),
                    Some(Value::Integer(200)),
                ],
                vec![
                    Some(Value::String("B".to_string())),
                    Some(Value::Integer(150)),
                ],
            ],
        );

        let group = OpGroup::new(Op::Table(table), vec![Var::new("dept")]).with_aggregate(
            Var::new("total"),
            Aggregate::Sum(ExprTerm::Var(Var::new("salary"))),
        );

        let result = engine.execute(Op::Group(group), &ctx).unwrap();
        let mut bindings: Vec<_> = result.collect().unwrap();
        bindings.sort_by(|a, b| {
            let a_val = a
                .get(&Var::new("dept"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            let b_val = b
                .get(&Var::new("dept"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            a_val.cmp(b_val)
        });

        assert_eq!(bindings.len(), 2);
        let total_a = bindings[0]
            .get(&Var::new("total"))
            .cloned()
            .unwrap_or(Value::Null);
        let total_b = bindings[1]
            .get(&Var::new("total"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(total_a, Value::Integer(300));
        assert_eq!(total_b, Value::Integer(150));
    }

    #[test]
    fn test_in_memory_engine_extend() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x")],
            vec![vec![Some(Value::Integer(1))], vec![Some(Value::Integer(2))]],
        );

        let extend = OpExtend::new(
            Op::Table(table),
            Var::new("xs"),
            ExprTerm::Str(Box::new(ExprTerm::Var(Var::new("x")))),
        );

        let result = engine.execute(Op::Extend(extend), &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings[0].get(&Var::new("xs")),
            Some(&Value::String("1".to_string()))
        );
        assert_eq!(
            bindings[1].get(&Var::new("xs")),
            Some(&Value::String("2".to_string()))
        );
    }

    #[test]
    fn test_in_memory_engine_minus() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let left = OpTable::new(
            vec![Var::new("x")],
            vec![
                vec![Some(Value::Integer(1))],
                vec![Some(Value::Integer(2))],
                vec![Some(Value::Integer(3))],
            ],
        );

        let right = OpTable::new(vec![Var::new("x")], vec![vec![Some(Value::Integer(2))]]);

        let minus = OpMinus::new(Op::Table(left), Op::Table(right));
        let result = engine.execute(Op::Minus(minus), &ctx).unwrap();
        let mut bindings: Vec<_> = result.collect().unwrap();
        bindings.sort_by(|a, b| {
            let a_val = a
                .get(&Var::new("x"))
                .and_then(|v| match v {
                    Value::Integer(i) => Some(*i),
                    _ => None,
                })
                .unwrap_or(0);
            let b_val = b
                .get(&Var::new("x"))
                .and_then(|v| match v {
                    Value::Integer(i) => Some(*i),
                    _ => None,
                })
                .unwrap_or(0);
            a_val.cmp(&b_val)
        });

        let values: Vec<i64> = bindings
            .iter()
            .filter_map(|b| b.get(&Var::new("x")))
            .filter_map(|v| match v {
                Value::Integer(i) => Some(*i),
                _ => None,
            })
            .collect();

        assert_eq!(values, vec![1, 3]);
    }

    #[test]
    fn test_in_memory_engine_minus_shared_vars() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let left = OpTable::new(
            vec![Var::new("x"), Var::new("y")],
            vec![
                vec![Some(Value::Integer(1)), Some(Value::Integer(10))],
                vec![Some(Value::Integer(2)), Some(Value::Integer(20))],
            ],
        );

        let right = OpTable::new(vec![Var::new("x")], vec![vec![Some(Value::Integer(1))]]);

        let minus = OpMinus::new(Op::Table(left), Op::Table(right));
        let result = engine.execute(Op::Minus(minus), &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].get(&Var::new("x")), Some(&Value::Integer(2)));
    }

    #[test]
    fn test_in_memory_engine_extend_conflict() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(vec![Var::new("x")], vec![vec![Some(Value::Integer(1))]]);

        let extend = OpExtend::new(
            Op::Table(table),
            Var::new("x"),
            ExprTerm::Const(Value::Integer(2)),
        );

        let result = engine.execute(Op::Extend(extend), &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert!(bindings.is_empty());
    }

    #[test]
    fn test_in_memory_engine_extend_unbound_keeps_row() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(vec![Var::new("x")], vec![vec![Some(Value::Integer(1))]]);

        let extend = OpExtend::new(
            Op::Table(table),
            Var::new("z"),
            ExprTerm::Var(Var::new("missing")),
        );

        let result = engine.execute(Op::Extend(extend), &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].get(&Var::new("x")), Some(&Value::Integer(1)));
        assert_eq!(bindings[0].get(&Var::new("z")), None);
    }

    #[test]
    fn test_in_memory_engine_slice() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x")],
            (1..=10).map(|i| vec![Some(Value::Integer(i))]).collect(),
        );

        let op = Op::Slice(OpSlice::new(Op::Table(table), 2, Some(3)));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].get(&Var::new("x")), Some(&Value::Integer(3)));
    }

    #[test]
    fn test_in_memory_engine_project() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x"), Var::new("y"), Var::new("z")],
            vec![vec![
                Some(Value::Integer(1)),
                Some(Value::Integer(2)),
                Some(Value::Integer(3)),
            ]],
        );

        let op = Op::Project(OpProject::new(
            vec![Var::new("x"), Var::new("z")],
            Op::Table(table),
        ));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].contains(&Var::new("x")));
        assert!(!bindings[0].contains(&Var::new("y")));
        assert!(bindings[0].contains(&Var::new("z")));
    }

    #[test]
    fn test_in_memory_engine_distinct() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x")],
            vec![
                vec![Some(Value::Integer(1))],
                vec![Some(Value::Integer(2))],
                vec![Some(Value::Integer(1))],
                vec![Some(Value::Integer(3))],
                vec![Some(Value::Integer(2))],
            ],
        );

        let op = Op::Distinct(OpDistinct::new(Op::Table(table)));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 3);
    }

    #[test]
    fn test_in_memory_engine_union() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table1 = OpTable::new(
            vec![Var::new("x")],
            vec![vec![Some(Value::Integer(1))], vec![Some(Value::Integer(2))]],
        );

        let table2 = OpTable::new(
            vec![Var::new("x")],
            vec![vec![Some(Value::Integer(3))], vec![Some(Value::Integer(4))]],
        );

        let op = Op::Union(OpUnion::new(Op::Table(table1), Op::Table(table2)));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 4);
    }

    #[test]
    fn test_in_memory_engine_order() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new();

        let table = OpTable::new(
            vec![Var::new("x")],
            vec![
                vec![Some(Value::Integer(3))],
                vec![Some(Value::Integer(1))],
                vec![Some(Value::Integer(2))],
            ],
        );

        let op = Op::Order(OpOrder::new(
            Op::Table(table),
            vec![OrderKey::asc(ExprTerm::Var(Var::new("x")))],
        ));

        let result = engine.execute(op, &ctx).unwrap();
        let bindings: Vec<_> = result.collect().unwrap();

        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].get(&Var::new("x")), Some(&Value::Integer(1)));
        assert_eq!(bindings[1].get(&Var::new("x")), Some(&Value::Integer(2)));
        assert_eq!(bindings[2].get(&Var::new("x")), Some(&Value::Integer(3)));
    }

    #[test]
    fn test_engine_with_stats() {
        let engine = InMemoryEngine::new();
        let ctx = QueryContext::new().with_stats();

        let table = OpTable::unit();
        let result = engine.execute(Op::Table(table), &ctx).unwrap();

        assert!(result.stats.is_some());
    }

    #[test]
    fn test_engine_capabilities() {
        let engine = InMemoryEngine::new();
        let caps = engine.capabilities();

        assert!(caps.graph_patterns);
        assert!(caps.subqueries);
        assert!(!caps.transactions);
    }
}
