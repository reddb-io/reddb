//! ClickHouse-style projections — query-specific pre-aggregated
//! indexes the planner can transparently pick over the base table.
//!
//! A [`ProjectionSpec`] declares:
//! * which columns are group keys,
//! * which aggregates are stored,
//! * which filter (optional) restricts the source rows.
//!
//! The planner picks a projection when the query's shape is a
//! **subset** of the projection's shape: every group key mentioned
//! in the query's `GROUP BY` must exist in the projection, every
//! requested aggregate must be present, and the query's filter must
//! be compatible (either the same or tighter via an additional
//! predicate).
//!
//! This module models only the metadata + matcher. The actual
//! storage maintenance (CDC hook, incremental refresh) lives in a
//! follow-on sprint once SQL DDL wires `ALTER TABLE ... ADD
//! PROJECTION`. Keeping the matcher standalone lets tests exercise
//! the routing rules on synthetic specs.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionAggregate {
    Count,
    Sum(usize),
    Min(usize),
    Max(usize),
}

#[derive(Debug, Clone)]
pub struct ProjectionSpec {
    pub name: String,
    /// Table the projection is attached to.
    pub table: String,
    /// Columns (by index in the parent table schema) used as group
    /// keys.
    pub group_keys: Vec<usize>,
    /// Aggregates materialised in this projection.
    pub aggregates: Vec<ProjectionAggregate>,
    /// Optional pre-applied filter — e.g.
    /// `WHERE env = 'production'`. Represented as a canonical string
    /// because the matcher only needs equality on the serialised
    /// form; a richer AST comparison can plug in later.
    pub filter_signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectionQuery {
    pub table: String,
    pub group_keys: Vec<usize>,
    pub aggregates: Vec<ProjectionAggregate>,
    pub filter_signature: Option<String>,
}

/// Result of matching a query against a set of projections. Callers
/// use the lowest-cost hit; ties broken by declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionMatch {
    pub projection_name: String,
    /// Relative cost hint. Smaller = better. Currently encodes
    /// "extra group keys the query doesn't need" as cost — so the
    /// narrowest-fit projection wins.
    pub cost: u32,
}

pub fn pick_projection(
    query: &ProjectionQuery,
    candidates: &[ProjectionSpec],
) -> Option<ProjectionMatch> {
    let mut best: Option<ProjectionMatch> = None;
    for spec in candidates {
        if spec.table != query.table {
            continue;
        }
        if !is_filter_compatible(&spec.filter_signature, &query.filter_signature) {
            continue;
        }
        if !group_keys_cover(&spec.group_keys, &query.group_keys) {
            continue;
        }
        if !aggregates_cover(&spec.aggregates, &query.aggregates) {
            continue;
        }
        let extra_keys = spec
            .group_keys
            .iter()
            .filter(|k| !query.group_keys.contains(k))
            .count() as u32;
        let extra_aggs = spec
            .aggregates
            .iter()
            .filter(|a| !query.aggregates.contains(a))
            .count() as u32;
        let cost = extra_keys * 10 + extra_aggs;
        let candidate = ProjectionMatch {
            projection_name: spec.name.clone(),
            cost,
        };
        match &best {
            Some(existing) if existing.cost <= cost => {}
            _ => best = Some(candidate),
        }
    }
    best
}

fn is_filter_compatible(spec: &Option<String>, query: &Option<String>) -> bool {
    match (spec, query) {
        (None, _) => true,            // projection covers all rows
        (Some(_), None) => false,     // projection is narrower than query
        (Some(s), Some(q)) => s == q, // only identical filters allowed for now
    }
}

fn group_keys_cover(spec_keys: &[usize], query_keys: &[usize]) -> bool {
    let set: HashSet<usize> = spec_keys.iter().copied().collect();
    query_keys.iter().all(|k| set.contains(k))
}

fn aggregates_cover(spec: &[ProjectionAggregate], query: &[ProjectionAggregate]) -> bool {
    let set: HashSet<ProjectionAggregate> = spec.iter().copied().collect();
    query.iter().all(|a| set.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_spec() -> ProjectionSpec {
        ProjectionSpec {
            name: "daily_by_user".into(),
            table: "events".into(),
            group_keys: vec![0, 1], // user_id, day
            aggregates: vec![ProjectionAggregate::Count, ProjectionAggregate::Sum(2)],
            filter_signature: None,
        }
    }

    fn narrower_spec() -> ProjectionSpec {
        ProjectionSpec {
            name: "daily_total".into(),
            table: "events".into(),
            group_keys: vec![1], // day only
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        }
    }

    fn filtered_spec() -> ProjectionSpec {
        ProjectionSpec {
            name: "prod_daily".into(),
            table: "events".into(),
            group_keys: vec![1],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: Some("env = 'production'".into()),
        }
    }

    #[test]
    fn picks_matching_projection_when_query_is_a_subset() {
        let query = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![0],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        let pick = pick_projection(&query, &[base_spec()]).unwrap();
        assert_eq!(pick.projection_name, "daily_by_user");
    }

    #[test]
    fn prefers_narrower_projection_when_both_match() {
        let query = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![1],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        let pick = pick_projection(&query, &[base_spec(), narrower_spec()]).unwrap();
        // narrower has fewer extra group keys ⇒ lower cost ⇒ wins.
        assert_eq!(pick.projection_name, "daily_total");
    }

    #[test]
    fn rejects_projection_when_query_requests_unknown_aggregate() {
        let query = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![1],
            aggregates: vec![ProjectionAggregate::Max(3)],
            filter_signature: None,
        };
        assert!(pick_projection(&query, &[base_spec()]).is_none());
    }

    #[test]
    fn rejects_projection_when_query_key_not_in_projection() {
        let query = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![5], // never grouped by this in any spec
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        assert!(pick_projection(&query, &[base_spec(), narrower_spec()]).is_none());
    }

    #[test]
    fn filtered_projection_matches_only_when_filters_match() {
        let query_without_filter = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![1],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        assert!(pick_projection(&query_without_filter, &[filtered_spec()]).is_none());

        let query_with_filter = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![1],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: Some("env = 'production'".into()),
        };
        let pick = pick_projection(&query_with_filter, &[filtered_spec()]).unwrap();
        assert_eq!(pick.projection_name, "prod_daily");
    }

    #[test]
    fn different_table_never_matches() {
        let query = ProjectionQuery {
            table: "other_table".into(),
            group_keys: vec![0],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        assert!(pick_projection(&query, &[base_spec()]).is_none());
    }

    #[test]
    fn empty_candidate_list_returns_none() {
        let query = ProjectionQuery {
            table: "events".into(),
            group_keys: vec![],
            aggregates: vec![ProjectionAggregate::Count],
            filter_signature: None,
        };
        assert!(pick_projection(&query, &[]).is_none());
    }
}
