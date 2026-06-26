//! Subquery Decorrelation Optimizer
//!
//! Transforms correlated subqueries into efficient join-based queries.
//!
//! # Motivation
//!
//! Correlated subqueries are evaluated per-row of the outer query (O(n²)).
//! Decorrelation transforms them into joins which can be executed more efficiently (O(n log n)).
//!
//! # Example Transformation
//!
//! **Before (correlated):**
//! ```sql
//! SELECT * FROM orders o
//! WHERE total > (SELECT AVG(total) FROM orders WHERE customer_id = o.customer_id)
//! ```
//!
//! **After (decorrelated):**
//! ```sql
//! SELECT o.* FROM orders o
//! JOIN (SELECT customer_id, AVG(total) as avg_total FROM orders GROUP BY customer_id) sub
//!   ON o.customer_id = sub.customer_id
//! WHERE o.total > sub.avg_total
//! ```
//!
//! # Supported Patterns
//!
//! - Scalar correlated subqueries with equality correlation predicates
//! - IN/EXISTS correlated subqueries
//! - Aggregation subqueries (GROUP BY the correlation columns)

/// Represents a correlation predicate between outer and inner queries
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationPredicate {
    /// Column from outer query
    pub outer_col: String,
    /// Column from inner query
    pub inner_col: String,
    /// Comparison operator (typically Eq for decorrelation)
    pub op: CorrelationOp,
}

/// Correlation comparison operator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrelationOp {
    /// Equality correlation (most common, fully decorrelatable)
    Eq,
    /// Less than (semi-decorrelatable)
    Lt,
    /// Greater than (semi-decorrelatable)
    Gt,
}

/// Analysis result for a subquery
#[derive(Debug, Clone)]
pub struct SubqueryAnalysis {
    /// Whether the subquery is correlated
    pub is_correlated: bool,
    /// Correlation predicates (if correlated)
    pub correlation_predicates: Vec<CorrelationPredicate>,
    /// Whether decorrelation is possible
    pub can_decorrelate: bool,
    /// Reason if decorrelation is not possible
    pub decorrelation_blocker: Option<DecorrelationBlocker>,
    /// Suggested decorrelation strategy
    pub strategy: Option<DecorrelationStrategy>,
}

/// Reasons why decorrelation might not be possible
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecorrelationBlocker {
    /// Non-equality correlation predicates that can't be converted to joins
    NonEqualityCorrelation,
    /// Correlation in LIMIT/OFFSET (can't be pushed down)
    CorrelationInLimit,
    /// Multiple correlation levels (nested correlated subqueries)
    NestedCorrelation,
    /// Correlation in HAVING clause (complex transformation needed)
    CorrelationInHaving,
    /// Lateral join semantics required but not supported
    RequiresLateralJoin,
}

/// Strategy for decorrelating a subquery
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecorrelationStrategy {
    /// Convert to INNER JOIN with GROUP BY on correlation columns
    /// Used for scalar subqueries with aggregation
    JoinWithGroupBy {
        /// Columns to group by (the correlation columns)
        group_by_cols: Vec<String>,
        /// Join condition (equality on correlation columns)
        join_condition: Vec<(String, String)>,
    },

    /// Convert to LEFT JOIN (for nullable results)
    LeftJoinWithGroupBy {
        group_by_cols: Vec<String>,
        join_condition: Vec<(String, String)>,
    },

    /// Convert IN subquery to SEMI JOIN
    SemiJoin {
        join_condition: Vec<(String, String)>,
    },

    /// Convert NOT IN/NOT EXISTS to ANTI JOIN
    AntiJoin {
        join_condition: Vec<(String, String)>,
    },

    /// Apply DISTINCT to inner query and join
    /// Used when inner returns duplicates but only existence matters
    DistinctJoin {
        join_condition: Vec<(String, String)>,
    },
}

/// Subquery decorrelation optimizer
pub struct Decorrelator {
    /// Counter for generating unique aliases
    alias_counter: usize,
}

impl Decorrelator {
    /// Create a new decorrelator
    pub fn new() -> Self {
        Self { alias_counter: 0 }
    }

    /// Generate a unique alias for derived tables
    fn next_alias(&mut self) -> String {
        self.alias_counter += 1;
        format!("__derived_{}", self.alias_counter)
    }

    /// Analyze a correlated subquery for decorrelation potential
    pub fn analyze(
        &self,
        outer_refs: &[String],
        inner_cols: &[String],
        correlation_predicates: &[(String, String)], // (outer, inner) equality pairs
        subquery_type: SubqueryKind,
        has_aggregation: bool,
        has_limit: bool,
    ) -> SubqueryAnalysis {
        // Non-correlated subqueries don't need decorrelation
        if outer_refs.is_empty() {
            return SubqueryAnalysis {
                is_correlated: false,
                correlation_predicates: Vec::new(),
                can_decorrelate: false,
                decorrelation_blocker: None,
                strategy: None,
            };
        }

        // Build correlation predicates
        let predicates: Vec<CorrelationPredicate> = correlation_predicates
            .iter()
            .map(|(outer, inner)| CorrelationPredicate {
                outer_col: outer.clone(),
                inner_col: inner.clone(),
                op: CorrelationOp::Eq,
            })
            .collect();

        // Check for blockers
        if has_limit {
            return SubqueryAnalysis {
                is_correlated: true,
                correlation_predicates: predicates,
                can_decorrelate: false,
                decorrelation_blocker: Some(DecorrelationBlocker::CorrelationInLimit),
                strategy: None,
            };
        }

        // Determine strategy based on subquery type
        let strategy = match subquery_type {
            SubqueryKind::Scalar if has_aggregation => {
                // Scalar aggregation can be decorrelated with GROUP BY
                let group_by_cols: Vec<String> =
                    predicates.iter().map(|p| p.inner_col.clone()).collect();
                let join_condition: Vec<(String, String)> = predicates
                    .iter()
                    .map(|p| (p.outer_col.clone(), p.inner_col.clone()))
                    .collect();

                Some(DecorrelationStrategy::JoinWithGroupBy {
                    group_by_cols,
                    join_condition,
                })
            }
            SubqueryKind::Scalar => {
                // Non-aggregation scalar - use LEFT JOIN
                let group_by_cols: Vec<String> =
                    predicates.iter().map(|p| p.inner_col.clone()).collect();
                let join_condition: Vec<(String, String)> = predicates
                    .iter()
                    .map(|p| (p.outer_col.clone(), p.inner_col.clone()))
                    .collect();

                Some(DecorrelationStrategy::LeftJoinWithGroupBy {
                    group_by_cols,
                    join_condition,
                })
            }
            SubqueryKind::Exists | SubqueryKind::In => {
                // EXISTS/IN becomes SEMI JOIN
                let join_condition: Vec<(String, String)> = predicates
                    .iter()
                    .map(|p| (p.outer_col.clone(), p.inner_col.clone()))
                    .collect();

                Some(DecorrelationStrategy::SemiJoin { join_condition })
            }
            SubqueryKind::NotExists | SubqueryKind::NotIn => {
                // NOT EXISTS/NOT IN becomes ANTI JOIN
                let join_condition: Vec<(String, String)> = predicates
                    .iter()
                    .map(|p| (p.outer_col.clone(), p.inner_col.clone()))
                    .collect();

                Some(DecorrelationStrategy::AntiJoin { join_condition })
            }
            SubqueryKind::Any | SubqueryKind::All => {
                // ANY/ALL with equality can be semi/anti join
                // For other comparisons, more complex transformation needed
                None
            }
        };

        SubqueryAnalysis {
            is_correlated: true,
            correlation_predicates: predicates,
            can_decorrelate: strategy.is_some(),
            decorrelation_blocker: if strategy.is_none() {
                Some(DecorrelationBlocker::RequiresLateralJoin)
            } else {
                None
            },
            strategy,
        }
    }

    /// Estimate cost improvement from decorrelation
    /// Returns the ratio of (correlated cost) / (decorrelated cost)
    pub fn estimate_speedup(
        &self,
        outer_cardinality: usize,
        inner_cardinality: usize,
        strategy: &DecorrelationStrategy,
    ) -> f64 {
        // Correlated: O(outer * inner) - subquery runs once per outer row
        let correlated_cost = (outer_cardinality * inner_cardinality) as f64;

        // Decorrelated: O(outer + inner + join) - depends on strategy
        let decorrelated_cost = match strategy {
            DecorrelationStrategy::JoinWithGroupBy { group_by_cols, .. } => {
                // GROUP BY inner + hash join
                let group_by_cost = inner_cardinality as f64 * (group_by_cols.len() as f64).log2();
                let join_cost = (outer_cardinality + inner_cardinality) as f64;
                group_by_cost + join_cost
            }
            DecorrelationStrategy::LeftJoinWithGroupBy { .. } => {
                // Similar to inner join but may produce more rows
                (outer_cardinality + inner_cardinality) as f64 * 1.5
            }
            DecorrelationStrategy::SemiJoin { .. } | DecorrelationStrategy::AntiJoin { .. } => {
                // Hash-based semi/anti join
                (outer_cardinality + inner_cardinality) as f64
            }
            DecorrelationStrategy::DistinctJoin { .. } => {
                // Distinct + join
                let distinct_cost = inner_cardinality as f64 * 1.2;
                let join_cost = (outer_cardinality + inner_cardinality) as f64;
                distinct_cost + join_cost
            }
        };

        // Avoid division by zero
        if decorrelated_cost < 1.0 {
            return correlated_cost;
        }

        correlated_cost / decorrelated_cost
    }

    /// Check if decorrelation is worthwhile based on cardinality
    pub fn should_decorrelate(
        &self,
        outer_cardinality: usize,
        inner_cardinality: usize,
        strategy: &DecorrelationStrategy,
    ) -> bool {
        // Always decorrelate if speedup > 1.5x
        let speedup = self.estimate_speedup(outer_cardinality, inner_cardinality, strategy);
        speedup > 1.5
    }
}

impl Default for Decorrelator {
    fn default() -> Self {
        Self::new()
    }
}

/// Kind of subquery for decorrelation analysis
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubqueryKind {
    /// Scalar subquery (returns single value)
    Scalar,
    /// EXISTS subquery
    Exists,
    /// NOT EXISTS subquery
    NotExists,
    /// IN subquery
    In,
    /// NOT IN subquery
    NotIn,
    /// ANY comparison
    Any,
    /// ALL comparison
    All,
}

// ============================================================================
// Rewrite Rules
// ============================================================================

/// Represents a rewrite of a correlated subquery to a join
#[derive(Debug, Clone)]
pub struct SubqueryRewrite {
    /// Alias for the derived table
    pub derived_alias: String,
    /// Join type to use
    pub join_type: RewriteJoinType,
    /// Columns to select from inner query (for the derived table)
    pub inner_select: Vec<String>,
    /// GROUP BY columns for the derived table (if aggregation)
    pub group_by: Vec<String>,
    /// Join condition (outer_col = derived_alias.inner_col pairs)
    pub join_on: Vec<(String, String)>,
    /// Column in derived table that replaces the subquery result
    pub result_col: Option<String>,
}

/// Join type for rewritten subquery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteJoinType {
    Inner,
    Left,
    Semi,
    Anti,
}

impl Decorrelator {
    /// Generate a rewrite plan for a decorrelatable subquery
    pub fn plan_rewrite(
        &mut self,
        analysis: &SubqueryAnalysis,
        aggregation_col: Option<&str>,
    ) -> Option<SubqueryRewrite> {
        let strategy = analysis.strategy.as_ref()?;

        let alias = self.next_alias();

        match strategy {
            DecorrelationStrategy::JoinWithGroupBy {
                group_by_cols,
                join_condition,
            } => {
                let mut inner_select = group_by_cols.clone();
                let result_col = aggregation_col.map(|c| {
                    let col_name = format!("__agg_{}", c);
                    inner_select.push(col_name.clone());
                    col_name
                });

                Some(SubqueryRewrite {
                    derived_alias: alias.clone(),
                    join_type: RewriteJoinType::Inner,
                    inner_select,
                    group_by: group_by_cols.clone(),
                    join_on: join_condition
                        .iter()
                        .map(|(o, i)| (o.clone(), format!("{}.{}", alias, i)))
                        .collect(),
                    result_col,
                })
            }
            DecorrelationStrategy::LeftJoinWithGroupBy {
                group_by_cols,
                join_condition,
            } => {
                let mut inner_select = group_by_cols.clone();
                let result_col = aggregation_col.map(|c| {
                    let col_name = format!("__agg_{}", c);
                    inner_select.push(col_name.clone());
                    col_name
                });

                Some(SubqueryRewrite {
                    derived_alias: alias.clone(),
                    join_type: RewriteJoinType::Left,
                    inner_select,
                    group_by: group_by_cols.clone(),
                    join_on: join_condition
                        .iter()
                        .map(|(o, i)| (o.clone(), format!("{}.{}", alias, i)))
                        .collect(),
                    result_col,
                })
            }
            DecorrelationStrategy::SemiJoin { join_condition } => Some(SubqueryRewrite {
                derived_alias: alias.clone(),
                join_type: RewriteJoinType::Semi,
                inner_select: join_condition.iter().map(|(_, i)| i.clone()).collect(),
                group_by: Vec::new(),
                join_on: join_condition
                    .iter()
                    .map(|(o, i)| (o.clone(), format!("{}.{}", alias, i)))
                    .collect(),
                result_col: None,
            }),
            DecorrelationStrategy::AntiJoin { join_condition } => Some(SubqueryRewrite {
                derived_alias: alias.clone(),
                join_type: RewriteJoinType::Anti,
                inner_select: join_condition.iter().map(|(_, i)| i.clone()).collect(),
                group_by: Vec::new(),
                join_on: join_condition
                    .iter()
                    .map(|(o, i)| (o.clone(), format!("{}.{}", alias, i)))
                    .collect(),
                result_col: None,
            }),
            DecorrelationStrategy::DistinctJoin { join_condition } => {
                Some(SubqueryRewrite {
                    derived_alias: alias.clone(),
                    join_type: RewriteJoinType::Semi,
                    inner_select: join_condition.iter().map(|(_, i)| i.clone()).collect(),
                    group_by: join_condition.iter().map(|(_, i)| i.clone()).collect(), // DISTINCT via GROUP BY
                    join_on: join_condition
                        .iter()
                        .map(|(o, i)| (o.clone(), format!("{}.{}", alias, i)))
                        .collect(),
                    result_col: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_correlated() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &[], // no outer refs
            &["id".to_string(), "value".to_string()],
            &[],
            SubqueryKind::Scalar,
            true,
            false,
        );

        assert!(!analysis.is_correlated);
        assert!(!analysis.can_decorrelate);
    }

    #[test]
    fn test_scalar_aggregation_decorrelation() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.customer_id".to_string()],
            &["customer_id".to_string(), "total".to_string()],
            &[("o.customer_id".to_string(), "customer_id".to_string())],
            SubqueryKind::Scalar,
            true,  // has aggregation
            false, // no limit
        );

        assert!(analysis.is_correlated);
        assert!(analysis.can_decorrelate);
        assert!(matches!(
            analysis.strategy,
            Some(DecorrelationStrategy::JoinWithGroupBy { .. })
        ));
    }

    #[test]
    fn test_exists_decorrelation() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.id".to_string()],
            &["order_id".to_string()],
            &[("o.id".to_string(), "order_id".to_string())],
            SubqueryKind::Exists,
            false,
            false,
        );

        assert!(analysis.is_correlated);
        assert!(analysis.can_decorrelate);
        assert!(matches!(
            analysis.strategy,
            Some(DecorrelationStrategy::SemiJoin { .. })
        ));
    }

    #[test]
    fn test_limit_blocks_decorrelation() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.id".to_string()],
            &["order_id".to_string()],
            &[("o.id".to_string(), "order_id".to_string())],
            SubqueryKind::Scalar,
            false,
            true, // has limit - blocks decorrelation
        );

        assert!(analysis.is_correlated);
        assert!(!analysis.can_decorrelate);
        assert_eq!(
            analysis.decorrelation_blocker,
            Some(DecorrelationBlocker::CorrelationInLimit)
        );
    }

    #[test]
    fn test_speedup_estimation() {
        let decorrelator = Decorrelator::new();

        // With 1000 outer rows and 1000 inner rows:
        // Correlated: 1000 * 1000 = 1,000,000 operations
        // Decorrelated (join): ~2000 + join cost
        let speedup = decorrelator.estimate_speedup(
            1000,
            1000,
            &DecorrelationStrategy::SemiJoin {
                join_condition: vec![("a".to_string(), "b".to_string())],
            },
        );

        // Should be significant speedup
        assert!(speedup > 100.0);
    }

    #[test]
    fn test_rewrite_plan() {
        let mut decorrelator = Decorrelator::new();

        let analysis = decorrelator.analyze(
            &["o.customer_id".to_string()],
            &["customer_id".to_string(), "total".to_string()],
            &[("o.customer_id".to_string(), "customer_id".to_string())],
            SubqueryKind::Scalar,
            true,
            false,
        );

        let rewrite = decorrelator.plan_rewrite(&analysis, Some("avg_total"));
        assert!(rewrite.is_some());

        let rewrite = rewrite.unwrap();
        assert_eq!(rewrite.join_type, RewriteJoinType::Inner);
        assert!(rewrite.group_by.contains(&"customer_id".to_string()));
        assert!(rewrite.result_col.is_some());
    }

    #[test]
    fn test_scalar_non_aggregation_uses_left_join() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.customer_id".to_string()],
            &["customer_id".to_string()],
            &[("o.customer_id".to_string(), "customer_id".to_string())],
            SubqueryKind::Scalar,
            false, // no aggregation
            false,
        );

        assert!(analysis.is_correlated);
        assert!(analysis.can_decorrelate);
        assert!(matches!(
            analysis.strategy,
            Some(DecorrelationStrategy::LeftJoinWithGroupBy { .. })
        ));
    }

    #[test]
    fn test_not_exists_and_not_in_use_anti_join() {
        let decorrelator = Decorrelator::new();
        for kind in [SubqueryKind::NotExists, SubqueryKind::NotIn] {
            let analysis = decorrelator.analyze(
                &["o.id".to_string()],
                &["order_id".to_string()],
                &[("o.id".to_string(), "order_id".to_string())],
                kind,
                false,
                false,
            );

            assert!(analysis.can_decorrelate);
            assert!(matches!(
                analysis.strategy,
                Some(DecorrelationStrategy::AntiJoin { .. })
            ));
        }
    }

    #[test]
    fn test_in_subquery_uses_semi_join() {
        let decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.id".to_string()],
            &["order_id".to_string()],
            &[("o.id".to_string(), "order_id".to_string())],
            SubqueryKind::In,
            false,
            false,
        );

        assert!(matches!(
            analysis.strategy,
            Some(DecorrelationStrategy::SemiJoin { .. })
        ));
    }

    #[test]
    fn test_any_all_block_decorrelation_with_lateral_join() {
        let decorrelator = Decorrelator::new();
        for kind in [SubqueryKind::Any, SubqueryKind::All] {
            let analysis = decorrelator.analyze(
                &["o.id".to_string()],
                &["order_id".to_string()],
                &[("o.id".to_string(), "order_id".to_string())],
                kind,
                false,
                false,
            );

            assert!(analysis.is_correlated);
            assert!(!analysis.can_decorrelate);
            assert!(analysis.strategy.is_none());
            assert_eq!(
                analysis.decorrelation_blocker,
                Some(DecorrelationBlocker::RequiresLateralJoin)
            );
        }
    }

    #[test]
    fn test_speedup_covers_every_strategy() {
        let decorrelator = Decorrelator::new();
        let join_condition = vec![("a".to_string(), "b".to_string())];

        // JoinWithGroupBy + LeftJoinWithGroupBy + DistinctJoin + AntiJoin
        let strategies = [
            DecorrelationStrategy::JoinWithGroupBy {
                group_by_cols: vec!["b".to_string()],
                join_condition: join_condition.clone(),
            },
            DecorrelationStrategy::LeftJoinWithGroupBy {
                group_by_cols: vec!["b".to_string()],
                join_condition: join_condition.clone(),
            },
            DecorrelationStrategy::AntiJoin {
                join_condition: join_condition.clone(),
            },
            DecorrelationStrategy::DistinctJoin {
                join_condition: join_condition.clone(),
            },
        ];

        for strategy in &strategies {
            let speedup = decorrelator.estimate_speedup(1000, 1000, strategy);
            assert!(speedup > 1.0, "expected speedup for {strategy:?}");
        }
    }

    #[test]
    fn test_speedup_guards_against_zero_cost() {
        let decorrelator = Decorrelator::new();
        // outer=0, inner=0 → decorrelated_cost < 1.0 → returns correlated_cost (0.0)
        let speedup = decorrelator.estimate_speedup(
            0,
            0,
            &DecorrelationStrategy::SemiJoin {
                join_condition: vec![("a".to_string(), "b".to_string())],
            },
        );
        assert_eq!(speedup, 0.0);
    }

    #[test]
    fn test_should_decorrelate_threshold() {
        let decorrelator = Decorrelator::new();
        let strategy = DecorrelationStrategy::SemiJoin {
            join_condition: vec![("a".to_string(), "b".to_string())],
        };

        // Large cardinalities → big speedup → worthwhile.
        assert!(decorrelator.should_decorrelate(1000, 1000, &strategy));
        // Tiny cardinalities → speedup below the 1.5x bar → not worthwhile.
        assert!(!decorrelator.should_decorrelate(1, 1, &strategy));
    }

    #[test]
    fn test_plan_rewrite_left_join() {
        let mut decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &["o.customer_id".to_string()],
            &["customer_id".to_string()],
            &[("o.customer_id".to_string(), "customer_id".to_string())],
            SubqueryKind::Scalar,
            false,
            false,
        );

        let rewrite = decorrelator
            .plan_rewrite(&analysis, Some("last_total"))
            .unwrap();
        assert_eq!(rewrite.join_type, RewriteJoinType::Left);
        assert!(rewrite.result_col.is_some());
        assert!(rewrite.group_by.contains(&"customer_id".to_string()));
    }

    #[test]
    fn test_plan_rewrite_semi_and_anti_join() {
        let mut decorrelator = Decorrelator::new();

        let semi = decorrelator.analyze(
            &["o.id".to_string()],
            &["order_id".to_string()],
            &[("o.id".to_string(), "order_id".to_string())],
            SubqueryKind::Exists,
            false,
            false,
        );
        let semi_rewrite = decorrelator.plan_rewrite(&semi, None).unwrap();
        assert_eq!(semi_rewrite.join_type, RewriteJoinType::Semi);
        assert!(semi_rewrite.result_col.is_none());
        assert!(semi_rewrite.group_by.is_empty());

        let anti = decorrelator.analyze(
            &["o.id".to_string()],
            &["order_id".to_string()],
            &[("o.id".to_string(), "order_id".to_string())],
            SubqueryKind::NotExists,
            false,
            false,
        );
        let anti_rewrite = decorrelator.plan_rewrite(&anti, None).unwrap();
        assert_eq!(anti_rewrite.join_type, RewriteJoinType::Anti);
    }

    #[test]
    fn test_plan_rewrite_distinct_join() {
        let mut decorrelator = Decorrelator::new();
        // DistinctJoin is not produced by `analyze`, so build the analysis by hand
        // to exercise the `plan_rewrite` DistinctJoin arm.
        let analysis = SubqueryAnalysis {
            is_correlated: true,
            correlation_predicates: Vec::new(),
            can_decorrelate: true,
            decorrelation_blocker: None,
            strategy: Some(DecorrelationStrategy::DistinctJoin {
                join_condition: vec![("o.id".to_string(), "order_id".to_string())],
            }),
        };

        let rewrite = decorrelator.plan_rewrite(&analysis, None).unwrap();
        assert_eq!(rewrite.join_type, RewriteJoinType::Semi);
        // DISTINCT is expressed via GROUP BY on the inner join column.
        assert_eq!(rewrite.group_by, vec!["order_id".to_string()]);
        assert!(rewrite.join_on[0].1.ends_with(".order_id"));
    }

    #[test]
    fn test_plan_rewrite_returns_none_without_strategy() {
        let mut decorrelator = Decorrelator::new();
        let analysis = decorrelator.analyze(
            &[],
            &["id".to_string()],
            &[],
            SubqueryKind::Scalar,
            true,
            false,
        );
        assert!(decorrelator.plan_rewrite(&analysis, None).is_none());
    }

    #[test]
    fn test_default_impl() {
        let decorrelator = Decorrelator::default();
        let analysis = decorrelator.analyze(&[], &[], &[], SubqueryKind::Scalar, false, false);
        assert!(!analysis.is_correlated);
    }
}
