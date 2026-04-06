//! Query Optimizer
//!
//! Provides filter ranking, cost-based optimization, and subquery decorrelation.

pub mod decorrelate;
pub mod filter_rank;
pub mod stats;

pub use decorrelate::{
    CorrelationOp, CorrelationPredicate, DecorrelationBlocker, DecorrelationStrategy, Decorrelator,
    RewriteJoinType, SubqueryAnalysis, SubqueryKind, SubqueryRewrite,
};
pub use filter_rank::{FilterRanker, RankedFilter, RankingConfig};
pub use stats::{ColumnStats, StatsCollector, TableStats};
