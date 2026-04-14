//! Query Engine for RedDB
//!
//! Provides query execution, filtering, sorting, and similarity search
//! capabilities for the RedDB storage engine.
//!
//! # Components
//!
//! - **filter**: Filter predicates (Eq, Lt, Gt, Between, Like, etc.)
//! - **sort**: Sorting and ordering operations
//! - **executor**: Query plan execution
//! - **similarity**: Vector similarity search integration
//! - **modes**: Multi-mode query parsing (SQL, Gremlin, Cypher, SPARQL, Natural Language)
//!
//! # Multi-Mode Parsing
//!
//! The query engine supports multiple query languages with automatic mode detection:
//!
//! ```ignore
//! use reddb::storage::query::modes::{parse_multi, detect_mode, QueryMode};
//!
//! // Gremlin
//! let gremlin = parse_multi("g.V().hasLabel('host').out('connects')").unwrap();
//!
//! // SPARQL
//! let sparql = parse_multi("SELECT ?host WHERE { ?host :hasIP ?ip }").unwrap();
//!
//! // Natural language
//! let natural = parse_multi("find all hosts with ssh open").unwrap();
//! ```
//!
//! # Example
//!
//! ```ignore
//! use reddb::storage::query::{Query, Filter, OrderBy, Direction};
//!
//! let query = Query::select("users")
//!     .filter(Filter::eq("status", "active"))
//!     .filter(Filter::gt("age", 18))
//!     .order_by("created_at", Direction::Desc)
//!     .limit(10);
//!
//! let results = executor.execute(&query)?;
//! ```

pub mod analyze_cmd;
pub mod analyzer;
pub mod ast;
pub mod binary;
pub mod engine;
pub mod executor;
pub mod executors;
pub mod expr_typing;
pub mod filter;
pub mod filter_compiled;
pub mod lexer;
pub mod modes;
pub mod optimizer;
pub mod parser;
pub mod planner;
pub mod rag;
pub mod security;
pub mod similarity;
pub mod sort;
pub mod sql_lowering;
pub mod step;
#[cfg(test)]
pub(crate) mod test_support;
pub mod unified;
pub(crate) mod value_compare;

// Re-export common types
pub use crate::storage::schema::{SqlTypeName, TypeModifier};
pub use analyzer::{
    analyze_create_table, resolve_declared_data_type, resolve_sql_type_name, AnalysisError,
    AnalyzedColumnDef, AnalyzedCreateTableQuery,
};
pub use ast::{
    AlterOperation, AlterTableQuery, CompareOp, CreateColumnDef, CreateTableQuery,
    CreateTimeSeriesQuery, CteDefinition, CteQueryBuilder, DeleteQuery, DropTableQuery,
    DropTimeSeriesQuery, EdgeDirection, EdgePattern, FieldRef, Filter as AstFilter, GraphCommand,
    GraphPattern, GraphQuery, InsertEntityType, InsertQuery, JoinCondition, JoinQuery, JoinType,
    NodePattern, NodeSelector, OrderByClause, PathQuery, Projection, QueryExpr, QueryWithCte,
    SearchCommand, SelectItem, TableQuery, UpdateQuery, WithClause,
};
pub use engine::{
    Binding, BindingBuilder, BindingIterator, Op, OpBGP, OpDisjunction, OpDistinct, OpExtend,
    OpFilter, OpGroup, OpJoin, OpLeftJoin, OpMinus, OpNull, OpOrder, OpProject, OpReduced,
    OpSequence, OpSlice, OpTable, OpTransform, OpTriple, OpUnion, OpVisitor, Pattern, QueryEngine,
    QueryEngineFactory, QueryEngineRegistry, QueryIter, QueryIterBase, QueryIterFilter,
    QueryIterJoin, QueryIterProject, QueryIterSlice, QueryIterSort, QueryIterUnion, TransformCopy,
    TransformPushFilter, Triple, Var,
};
pub use executor::{QueryExecutor, QueryPlan, QueryResult};
pub use executors::{
    CteContext, CteExecutor, CteStats, ExecuteResult, GremlinExecutor, MultiModeExecutor,
    NaturalExecutor, SparqlExecutor,
};
pub use filter::{Filter, FilterOp, Predicate};
pub use lexer::{Lexer, LexerError, Position, Spanned, Token};
pub use optimizer::{
    ColumnStats, FilterRanker, RankedFilter, RankingConfig, StatsCollector, TableStats,
};
pub use parser::{parse, ParseError, Parser};
pub use planner::{
    CacheStats, CachedPlan, CardinalityEstimate, CostEstimator, OptimizationPass, PlanCache,
    PlanCost, QueryOptimizer, QueryPlan as PlannerQueryPlan, QueryPlanner, QueryRewriter,
    RewriteContext, RewriteRule,
};
pub use rag::{
    ChunkSource, ContextChunk, EntityType, MultiSourceRetriever, QueryAnalysis, QueryIntent,
    RagConfig, RagEngine, RetrievalContext, RetrievalStrategy, SimilarEntity,
};
pub use security::{
    // Result types
    AttackPath,
    // Query types
    AttackPathQuery,
    BlastRadiusQuery,
    BlastRadiusResult,
    CredentialChain,
    LateralMovementQuery,
    LateralMovementResult,
    PrivEscPath,
    PrivEscQuery,
    ReachableHost,
    // Query engine
    SecurityQueries,
    SimilarCVE,
};
pub use similarity::{SimilarityQuery, SimilarityResult};
pub use sort::{Direction, NullsOrder, OrderBy, QueryLimits, SortKey};
pub use step::{
    AggregateStep, BarrierStep, BasicTraversal, BranchStep, ChooseStep, CollectingBarrierStep,
    DedupStep, Direction as TraversalDirection, EdgeSourceStep, EdgeStep, ExecutionMode,
    FilterStep, FlatMapStep, FoldStep, GroupStep, HasStep, IdStep, LimitStep, LoopState, MapStep,
    OptionalStep, OrderStep, Path, PathStep, Predicate as StepPredicate, ProjectStep, PropertyStep,
    RangeStep, ReducingBarrierStep, RepeatStep, SelectStep, SideEffectStep, SourceStep, Step,
    StepPosition, StepResult, StoreStep, Traversal, TraversalParent, Traverser, TraverserGenerator,
    TraverserRequirement, TraverserValue, UnionStep, ValueMapStep, VertexSourceStep, VertexStep,
    WhereStep,
};
pub use unified::{
    ExecutionError, GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedExecutor,
    UnifiedRecord, UnifiedResult,
};

/// Returns true when the table source should be interpreted as the universal entity
/// space (table, document, graph, vector mixed mode).
pub fn is_universal_entity_source(table: &str) -> bool {
    table.eq_ignore_ascii_case("any")
        || table.eq_ignore_ascii_case("_any")
        || table.eq_ignore_ascii_case("all")
        || table.eq_ignore_ascii_case("entity")
        || table.eq_ignore_ascii_case("universal")
        || table.eq_ignore_ascii_case("mixed")
        || table == "*"
}
