//! Graph inline-TVF & analytics materialization.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 4/10, issue #1625).
//! Houses the graph table-valued-function family that PRD #1619 keeps out of
//! the central dispatch file:
//!
//! - **Inline-TVF free fns** — `is_graph_tvf_name`, `analytics_view_algorithm`,
//!   argument parsing/validation, node/edge inlining, and the small
//!   `Value`-to-node-id / weight coercions.
//! - **Analytics dispatch methods** — `execute_inline_graph_function`,
//!   `materialize_whole_graph_abstract`, `try_resolve_analytics_view`,
//!   `dispatch_graph_algorithm`, the per-algorithm TVF executors, and
//!   `materialize_graph_with_rls` (which routes every entity through the RLS
//!   gate — see `super::rls_injection`).
//!
//! Names, signatures and visibility are preserved so the central dispatch and
//! sibling-file callers need no edits; a few formerly-private free fns are
//! bumped to `pub(crate)` and re-exported from `impl_core` for the call sites
//! that still live there.
use super::authz::policy_columns::parse_positive_iterations;
use super::execution_context::{
    capture_current_snapshot, current_auth_identity, entity_visible_with_context,
};
use super::rls_injection::{edge_passes_rls, node_passes_rls};
use super::*;

/// The graph-analytics table-valued functions recognized in FROM position.
/// Both the graph-collection form and the inline `nodes => / edges =>` form
/// (issue #799) accept these names.
pub(crate) fn is_graph_tvf_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("components")
        || name.eq_ignore_ascii_case("louvain")
        || name.eq_ignore_ascii_case("degree_centrality")
        || name.eq_ignore_ascii_case("shortest_path")
        || name.eq_ignore_ascii_case("betweenness")
        || name.eq_ignore_ascii_case("eigenvector")
        || name.eq_ignore_ascii_case("pagerank")
}

/// Map a declared `WITH ANALYTICS` view to the concrete graph algorithm name
/// and named-argument list that [`RedDBRuntime::dispatch_graph_algorithm`]
/// consumes (issue #800). The `using` option selects the algorithm inside the
/// output family; unsupported algorithms and the options that do not apply to
/// the chosen algorithm are rejected so a view never silently ignores a
/// declared parameter.
fn analytics_view_algorithm(
    graph: &str,
    view: &crate::catalog::AnalyticsViewDescriptor,
) -> RedDBResult<(String, Vec<(String, f64)>)> {
    use crate::catalog::AnalyticsOutput;

    let mut named_args: Vec<(String, f64)> = Vec::new();
    let algorithm = match view.output {
        AnalyticsOutput::Communities => {
            let algo = view.algorithm.as_deref().unwrap_or("louvain");
            if !algo.eq_ignore_ascii_case("louvain") {
                return Err(RedDBError::Query(format!(
                    "analytics output 'communities' on graph '{graph}' has unsupported algorithm '{algo}' (expected louvain)"
                )));
            }
            if let Some(resolution) = view.resolution {
                named_args.push(("resolution".to_string(), resolution));
            }
            "louvain".to_string()
        }
        AnalyticsOutput::Components => {
            if let Some(algo) = view.algorithm.as_deref() {
                if !algo.eq_ignore_ascii_case("components")
                    && !algo.eq_ignore_ascii_case("connected_components")
                {
                    return Err(RedDBError::Query(format!(
                        "analytics output 'components' on graph '{graph}' has unsupported algorithm '{algo}' (expected connected_components)"
                    )));
                }
            }
            "components".to_string()
        }
        AnalyticsOutput::Centrality => {
            let algo = view
                .algorithm
                .as_deref()
                .unwrap_or("pagerank")
                .to_ascii_lowercase();
            match algo.as_str() {
                "pagerank" => {
                    if let Some(max_iterations) = view.max_iterations {
                        named_args.push(("max_iterations".to_string(), max_iterations as f64));
                    }
                }
                "eigenvector" => {
                    if let Some(max_iterations) = view.max_iterations {
                        named_args.push(("max_iterations".to_string(), max_iterations as f64));
                    }
                    if let Some(tolerance) = view.tolerance {
                        named_args.push(("tolerance".to_string(), tolerance));
                    }
                }
                "betweenness" => {}
                other => {
                    return Err(RedDBError::Query(format!(
                        "analytics output 'centrality' on graph '{graph}' has unsupported algorithm '{other}' (expected pagerank, betweenness, or eigenvector)"
                    )));
                }
            }
            algo
        }
    };
    Ok((algorithm, named_args))
}

/// Reject any named arguments for a TVF that accepts none.
fn reject_named_args(name: &str, named_args: &[(String, f64)]) -> RedDBResult<()> {
    if let Some((key, _)) = named_args.first() {
        return Err(RedDBError::Query(format!(
            "table function '{name}' has no named argument '{key}'"
        )));
    }
    Ok(())
}

/// Resolve louvain's optional `resolution` named arg (γ, default 1.0). Any
/// other named key, or a non-finite / non-positive resolution, is rejected.
fn louvain_resolution(named_args: &[(String, f64)]) -> RedDBResult<f64> {
    let mut resolution = 1.0_f64;
    for (key, value) in named_args {
        if key.eq_ignore_ascii_case("resolution") {
            if !value.is_finite() || *value <= 0.0 {
                return Err(RedDBError::Query(format!(
                    "table function 'louvain' resolution must be > 0, got {value}"
                )));
            }
            resolution = *value;
        } else {
            return Err(RedDBError::Query(format!(
                "table function 'louvain' has no named argument '{key}' (expected 'resolution')"
            )));
        }
    }
    Ok(resolution)
}

/// Undirected degree centrality over abstract inputs: each edge contributes
/// 1 to both of its endpoints. Returns `(node_id, degree)` deterministically
/// in ascending node-id order, so identical input always yields identical
/// rows.
pub(crate) fn abstract_degree_centrality(
    nodes: &[String],
    edges: &[(
        String,
        String,
        crate::storage::engine::graph_algorithms::Weight,
    )],
) -> Vec<(String, usize)> {
    let mut degree: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for n in nodes {
        degree.entry(n.clone()).or_insert(0);
    }
    for (a, b, _w) in edges {
        *degree.entry(a.clone()).or_insert(0) += 1;
        *degree.entry(b.clone()).or_insert(0) += 1;
    }
    degree.into_iter().collect()
}

/// Ordered column names for a materialized subquery result: the projection
/// columns when present, else the first record's field order.
fn ordered_result_columns(result: &crate::storage::query::unified::UnifiedResult) -> Vec<String> {
    if !result.columns.is_empty() {
        return result.columns.clone();
    }
    result
        .records
        .first()
        .map(|record| {
            record
                .column_names()
                .iter()
                .map(|column| column.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Canonical node-id string for a cell value, so the node universe (from the
/// `nodes` subquery) and the edge endpoints (from the `edges` subquery)
/// compare equal regardless of integer-vs-text typing. `Null` is not a node.
fn value_to_node_id(value: &crate::storage::schema::Value) -> Option<String> {
    use crate::storage::schema::Value;
    match value {
        Value::Null => None,
        Value::Text(s) => Some(s.to_string()),
        Value::Integer(n) => Some(n.to_string()),
        Value::UnsignedInteger(n) => Some(n.to_string()),
        Value::NodeRef(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Numeric edge weight from a cell value (the optional third `edges` column).
fn value_to_weight(value: &crate::storage::schema::Value) -> Option<f32> {
    use crate::storage::schema::Value;
    match value {
        Value::Float(f) => Some(*f as f32),
        Value::Integer(n) => Some(*n as f32),
        Value::UnsignedInteger(n) => Some(*n as f32),
        _ => None,
    }
}

/// Build the node universe from a materialized `nodes` subquery result: the
/// first projected column of each row is the node id (issue #799). Zero rows
/// is a valid empty node set; a row set with no columns is a shape error.
fn inline_node_ids(
    name: &str,
    result: &crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<Vec<String>> {
    if result.records.is_empty() {
        return Ok(Vec::new());
    }
    let columns = ordered_result_columns(result);
    let Some(first_col) = columns.first() else {
        return Err(RedDBError::Query(format!(
            "table function '{name}' inline form: `nodes` subquery must project at least one column (the node id)"
        )));
    };
    let mut ids = Vec::with_capacity(result.records.len());
    for record in &result.records {
        if let Some(id) = record.get(first_col).and_then(value_to_node_id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Build the edge list from a materialized `edges` subquery result: the first
/// two projected columns are `(source, target)` and an optional third column
/// is the numeric weight (defaulting to 1.0). Fewer than two columns is a
/// shape error (issue #799).
fn inline_edges(
    name: &str,
    result: &crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<
    Vec<(
        String,
        String,
        crate::storage::engine::graph_algorithms::Weight,
    )>,
> {
    if result.records.is_empty() {
        return Ok(Vec::new());
    }
    let columns = ordered_result_columns(result);
    if columns.len() < 2 {
        return Err(RedDBError::Query(format!(
            "table function '{name}' inline form: `edges` subquery must project at least two columns (source, target), got {}",
            columns.len()
        )));
    }
    let src_col = &columns[0];
    let dst_col = &columns[1];
    let weight_col = columns.get(2);
    let mut edges = Vec::with_capacity(result.records.len());
    for record in &result.records {
        let (Some(src), Some(dst)) = (
            record.get(src_col).and_then(value_to_node_id),
            record.get(dst_col).and_then(value_to_node_id),
        ) else {
            // A null/absent endpoint is not a valid edge; skip it.
            continue;
        };
        let weight = match weight_col {
            Some(col) => match record.get(col) {
                None | Some(crate::storage::schema::Value::Null) => 1.0,
                Some(value) => value_to_weight(value).ok_or_else(|| {
                    RedDBError::Query(format!(
                        "table function '{name}' inline form: `edges` weight column must be numeric"
                    ))
                })?,
            },
            None => 1.0,
        };
        edges.push((src, dst, weight));
    }
    Ok(edges)
}

impl RedDBRuntime {
    /// Dispatch an inline-graph table-valued function call in FROM position
    /// (e.g. `SELECT * FROM components(nodes => (…), edges => (…))`, issue
    /// #799).
    ///
    /// Materializes the two subqueries through the normal read path (so RLS,
    /// column authz, and MVCC visibility all apply), constructs the abstract
    /// graph — the first column of `nodes` is the node id; the first two-or-
    /// three columns of `edges` are `(source, target [, weight])` — then runs
    /// the same algorithm path used by the graph-collection form. Read-only.
    pub(crate) fn execute_inline_graph_function(
        &self,
        name: &str,
        nodes_query: &QueryExpr,
        edges_query: &QueryExpr,
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        if !is_graph_tvf_name(name) {
            return Err(RedDBError::Query(format!("unknown table function: {name}")));
        }

        let node_result = self.execute_query_expr(nodes_query.clone())?.result;
        let nodes = inline_node_ids(name, &node_result)?;

        let edge_result = self.execute_query_expr(edges_query.clone())?.result;
        let edges = inline_edges(name, &edge_result)?;

        self.dispatch_graph_algorithm(name, nodes, edges, named_args)
    }

    /// Materialize the whole active graph read-only into the abstract
    /// `(nodes, edges)` inputs the pure graph algorithms consume.
    pub(crate) fn materialize_whole_graph_abstract(
        &self,
    ) -> RedDBResult<(
        Vec<String>,
        Vec<(
            String,
            String,
            crate::storage::engine::graph_algorithms::Weight,
        )>,
    )> {
        use crate::storage::engine::graph_algorithms;

        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();
        Ok((nodes, edges))
    }

    /// Resolve a `<graph>.<output>` analytics virtual view (issue #800).
    ///
    /// Returns `Ok(None)` when `table` is not an analytics view — either the
    /// name is not dotted, a real collection of that exact name exists (a real
    /// collection always wins; no shadowing), the suffix is not a recognised
    /// analytics output, or the parent is not a graph. Returns `Ok(Some(_))`
    /// with the freshly computed result when it does resolve, and an error when
    /// the parent graph exists but the output is not enabled, a declared
    /// algorithm is unsupported, or the parent collection's policy denies the
    /// read.
    ///
    /// The view is recomputed on every call (no result-cache write) so it
    /// always reflects the current graph data, satisfying the on-demand
    /// recompute contract for this slice.
    pub(crate) fn try_resolve_analytics_view(
        &self,
        table: &TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<crate::storage::query::unified::UnifiedResult>> {
        let full = table.table.as_str();
        let Some(dot) = full.rfind('.') else {
            return Ok(None);
        };
        // A real collection literally named `g.communities` always wins.
        if self.inner.db.store().get_collection(full).is_some() {
            return Ok(None);
        }
        let graph_name = &full[..dot];
        let output_name = &full[dot + 1..];
        let Some(output) = crate::catalog::AnalyticsOutput::from_str(output_name) else {
            return Ok(None);
        };

        let contracts = self.inner.db.collection_contracts();
        let Some(contract) = contracts.iter().find(|c| c.name == graph_name) else {
            return Ok(None);
        };
        if contract.declared_model != crate::catalog::CollectionModel::Graph {
            return Ok(None);
        }
        let Some(view) = contract
            .analytics_config
            .iter()
            .find(|view| view.output == output)
        else {
            // The parent graph exists but this output was not declared — a
            // clear error beats the misleading "collection not found".
            return Err(RedDBError::Query(format!(
                "analytics output '{output_name}' is not enabled on graph '{graph_name}'; declare it with WITH ANALYTICS (...)"
            )));
        };

        // Policy inheritance (AC5): route through the parent graph collection's
        // read authorization. A policy or RLS rule that denies the parent
        // denies its analytics views transitively.
        let parent_query = TableQuery::new(graph_name);
        if self
            .authorize_relational_table_select(parent_query, frame)?
            .is_none()
        {
            return Err(RedDBError::Query(format!(
                "permission denied: policy on graph '{graph_name}' denies analytics view '{output_name}'"
            )));
        }

        let (algorithm, named_args) = analytics_view_algorithm(graph_name, view)?;
        let (nodes, edges) = self.materialize_whole_graph_abstract()?;
        let result = self.dispatch_graph_algorithm(&algorithm, nodes, edges, &named_args)?;
        Ok(Some(result))
    }

    /// Shared algorithm dispatch over abstract `(nodes, edges)` inputs.
    ///
    /// Both the graph-collection form and the inline-graph form route here so
    /// named-argument validation and the projected row shape stay identical
    /// across the two signatures (issue #799). Projects each algorithm's
    /// native output shape.
    pub(crate) fn dispatch_graph_algorithm(
        &self,
        name: &str,
        nodes: Vec<String>,
        edges: Vec<(
            String,
            String,
            crate::storage::engine::graph_algorithms::Weight,
        )>,
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        if name.eq_ignore_ascii_case("components") {
            reject_named_args(name, named_args)?;
            let assignment = graph_algorithms::connected_components(&nodes, &edges);
            let mut result =
                UnifiedResult::with_columns(vec!["node_id".into(), "island_id".into()]);
            for (node_id, island_id) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("island_id", Value::Integer(island_id as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("louvain") {
            // The only supported named argument is `resolution` (γ). It
            // defaults to 1.0 (classic modularity) and must be a finite,
            // strictly positive number — a non-positive (or NaN/inf)
            // resolution has no sensible meaning.
            let resolution = louvain_resolution(named_args)?;
            let assignment = graph_algorithms::louvain(&nodes, &edges, resolution);
            let mut result =
                UnifiedResult::with_columns(vec!["node_id".into(), "community_id".into()]);
            for (node_id, community_id) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("community_id", Value::Integer(community_id as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("degree_centrality") {
            reject_named_args(name, named_args)?;
            let assignment = abstract_degree_centrality(&nodes, &edges);
            let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "degree".into()]);
            for (node_id, degree) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("degree", Value::Integer(degree as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("shortest_path") {
            // Scalar named arguments: `src` and `dst` are required node ids,
            // `max_hops` is an optional non-negative edge-count cap. Node ids
            // in the graph store are integer entity ids rendered as strings, so
            // each id arg must be a non-negative whole number; reject anything
            // else (fractional, negative, NaN/inf) with a clear message.
            let mut src: Option<String> = None;
            let mut dst: Option<String> = None;
            let mut max_hops: Option<usize> = None;
            let as_node_id = |key: &str, value: f64| -> RedDBResult<String> {
                if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
                    return Err(RedDBError::Query(format!(
                        "table function 'shortest_path' argument '{key}' must be a non-negative integer node id, got {value}"
                    )));
                }
                Ok((value as i64).to_string())
            };
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("src") {
                    src = Some(as_node_id("src", *value)?);
                } else if key.eq_ignore_ascii_case("dst") {
                    dst = Some(as_node_id("dst", *value)?);
                } else if key.eq_ignore_ascii_case("max_hops") {
                    if !value.is_finite() || *value < 0.0 || value.fract() != 0.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'shortest_path' max_hops must be a non-negative integer, got {value}"
                        )));
                    }
                    max_hops = Some(*value as usize);
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'shortest_path' has no named argument '{key}' (expected 'src', 'dst', 'max_hops')"
                    )));
                }
            }
            let src = src.ok_or_else(|| {
                RedDBError::Query(
                    "table function 'shortest_path' requires named argument 'src'".to_string(),
                )
            })?;
            let dst = dst.ok_or_else(|| {
                RedDBError::Query(
                    "table function 'shortest_path' requires named argument 'dst'".to_string(),
                )
            })?;

            // Columns are always present; an unreachable pair (within the
            // optional `max_hops` budget) simply yields zero rows — never an
            // error. `hop` is the 0-based index from the source;
            // `cumulative_weight` is the running path weight (0 at the source,
            // the total at the destination). Edges are treated as undirected,
            // consistent with `components` / `louvain`.
            let mut result = UnifiedResult::with_columns(vec![
                "hop".into(),
                "node_id".into(),
                "cumulative_weight".into(),
            ]);
            if let Some(path) =
                graph_algorithms::shortest_path(&nodes, &edges, &src, &dst, max_hops)
            {
                for (hop, (node_id, cumulative_weight)) in path.into_iter().enumerate() {
                    let mut record = UnifiedRecord::new();
                    record.set("hop", Value::Integer(hop as i64));
                    record.set("node_id", Value::text(node_id));
                    record.set("cumulative_weight", Value::Float(cumulative_weight));
                    result.push(record);
                }
            }
            return Ok(result);
        }
        // ── Centrality family (issue #797): each returns rows `(node_id,
        // score)` over the abstract `(nodes, edges)` graph. Like the other
        // graph TVFs the graph is treated as undirected and scores are
        // deterministic; the inline-graph form shares this dispatch. ──
        if name.eq_ignore_ascii_case("betweenness") {
            reject_named_args(name, named_args)?;
            return Ok(Self::centrality_result(graph_algorithms::betweenness(
                &nodes, &edges,
            )));
        }
        if name.eq_ignore_ascii_case("eigenvector") {
            // Optional `max_iterations` (positive integer, default 100) and
            // `tolerance` (finite, strictly positive, default 1e-6).
            let mut max_iterations = 100_usize;
            let mut tolerance = 1e-6_f64;
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("max_iterations") {
                    max_iterations = parse_positive_iterations("eigenvector", value)?;
                } else if key.eq_ignore_ascii_case("tolerance") {
                    if !value.is_finite() || *value <= 0.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'eigenvector' tolerance must be > 0, got {value}"
                        )));
                    }
                    tolerance = *value;
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'eigenvector' has no named argument '{key}' (expected 'max_iterations' or 'tolerance')"
                    )));
                }
            }
            return Ok(Self::centrality_result(graph_algorithms::eigenvector(
                &nodes,
                &edges,
                max_iterations,
                tolerance,
            )));
        }
        if name.eq_ignore_ascii_case("pagerank") {
            // Optional `damping` (in (0, 1), default 0.85) and `max_iterations`
            // (positive integer, default 100).
            let mut damping = 0.85_f64;
            let mut max_iterations = 100_usize;
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("damping") {
                    if !value.is_finite() || *value <= 0.0 || *value >= 1.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'pagerank' damping must be in (0, 1), got {value}"
                        )));
                    }
                    damping = *value;
                } else if key.eq_ignore_ascii_case("max_iterations") {
                    max_iterations = parse_positive_iterations("pagerank", value)?;
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'pagerank' has no named argument '{key}' (expected 'damping' or 'max_iterations')"
                    )));
                }
            }
            return Ok(Self::centrality_result(graph_algorithms::pagerank(
                &nodes,
                &edges,
                damping,
                max_iterations,
            )));
        }
        Err(RedDBError::Query(format!("unknown table function: {name}")))
    }

    /// `components(<graph_collection>)` — returns rows `(node_id, island_id)`.
    ///
    /// Materializes the active graph (nodes + weighted edges) read-only and
    /// runs the pure `graph_algorithms::connected_components`. Edges are
    /// treated as undirected; island ids are deterministic (ascending order of
    /// each component's smallest node).
    fn execute_components_tvf(
        &self,
        _collection: &str,
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        // Read-only materialization of the full active graph. The named
        // collection identifies the active graph scope; passing `None` for the
        // projection uses the full graph store (the same result
        // `active_graph_projection` yields when no projection is registered).
        // Materialization never mutates any store.
        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;

        // Materialize abstract inputs for the pure algorithm.
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();

        let assignment = graph_algorithms::connected_components(&nodes, &edges);

        // Project into a UnifiedResult with columns ["node_id", "island_id"].
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "island_id".into()]);
        for (node_id, island_id) in assignment {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("island_id", Value::Integer(island_id as i64));
            result.push(record);
        }
        Ok(result)
    }

    /// `louvain(<graph> [, resolution => <f64>])` — returns rows
    /// `(node_id, community_id)` (issue #796).
    ///
    /// Materializes the active graph (nodes + weighted edges) read-only and
    /// runs the pure, deterministic `graph_algorithms::louvain`. Edges are
    /// treated as undirected; community ids are assigned in ascending order of
    /// each community's smallest node, so identical input + resolution always
    /// yields identical rows. Like `components`, the v0 form runs over the
    /// whole graph store regardless of the collection argument value.
    fn execute_louvain_tvf(
        &self,
        _collection: &str,
        resolution: f64,
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;

        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();

        let assignment = graph_algorithms::louvain(&nodes, &edges, resolution);

        // Project into a UnifiedResult with columns ["node_id", "community_id"].
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "community_id".into()]);
        for (node_id, community_id) in assignment {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("community_id", Value::Integer(community_id as i64));
            result.push(record);
        }
        Ok(result)
    }

    /// Project `(node_id, score)` centrality rows into a `UnifiedResult` with
    /// columns `["node_id", "score"]`; scores are `Value::Float`.
    fn centrality_result(
        rows: Vec<(String, f64)>,
    ) -> crate::storage::query::unified::UnifiedResult {
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "score".into()]);
        for (node_id, score) in rows {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("score", Value::Float(score));
            result.push(record);
        }
        result
    }

    /// Materialise the entire graph store while applying MVCC visibility
    /// AND per-collection RLS to each candidate node and edge. Mirrors
    /// `materialize_graph` but routes every entity through the same
    /// gate the SELECT path uses, with the correct `PolicyTargetKind`
    /// per entity kind (`Nodes` for graph nodes, `Edges` for graph
    /// edges). Returns the filtered `GraphStore` plus the
    /// `node_id → properties` map the executor needs for `RETURN n.*`
    /// projections.
    pub(crate) fn materialize_graph_with_rls(
        &self,
    ) -> RedDBResult<(
        crate::storage::engine::GraphStore,
        std::collections::HashMap<
            String,
            std::collections::HashMap<String, crate::storage::schema::Value>,
        >,
        crate::storage::query::unified::EdgeProperties,
    )> {
        use crate::storage::engine::GraphStore;
        use crate::storage::query::ast::{PolicyAction, PolicyTargetKind};
        use crate::storage::unified::entity::{EntityData, EntityKind};
        use std::collections::{HashMap, HashSet};

        let store = self.inner.db.store();
        let snap_ctx = capture_current_snapshot();
        let role = current_auth_identity().map(|(_, r)| r.as_str().to_string());

        let graph = GraphStore::new();
        let mut node_properties: HashMap<String, HashMap<String, crate::storage::schema::Value>> =
            HashMap::new();
        let mut edge_properties: crate::storage::query::unified::EdgeProperties = HashMap::new();
        let mut allowed_nodes: HashSet<String> = HashSet::new();

        // Per-collection cached compiled filters — Nodes-kind for
        // first pass, Edges-kind for the second. None entries mean
        // "RLS enabled, zero matching policy → deny all of this kind".
        let mut node_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();
        let mut edge_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();

        let collections = store.list_collections();

        // First pass — gather nodes.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphNode(ref node) = entity.kind else {
                    continue;
                };
                if !node_passes_rls(self, collection, role.as_deref(), &mut node_rls, &entity) {
                    continue;
                }
                let id_str = entity.id.raw().to_string();
                graph
                    .add_node_with_label(
                        &id_str,
                        &node.label,
                        &super::graph_node_label(&node.node_type),
                    )
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                allowed_nodes.insert(id_str.clone());
                if let EntityData::Node(node_data) = &entity.data {
                    node_properties.insert(id_str, node_data.properties.clone());
                }
            }
        }

        // Second pass — gather edges. An edge appears only when both
        // endpoint nodes survived the RLS pass AND the edge itself
        // passes its own RLS gate.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphEdge(ref edge) = entity.kind else {
                    continue;
                };
                if !allowed_nodes.contains(&edge.from_node)
                    || !allowed_nodes.contains(&edge.to_node)
                {
                    continue;
                }
                if !edge_passes_rls(self, collection, role.as_deref(), &mut edge_rls, &entity) {
                    continue;
                }
                let weight = match &entity.data {
                    EntityData::Edge(e) => e.weight,
                    _ => edge.weight as f32 / 1000.0,
                };
                let edge_label = super::graph_edge_label(&edge.label);
                graph
                    .add_edge_with_label(&edge.from_node, &edge.to_node, &edge_label, weight)
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                if let EntityData::Edge(edge_data) = &entity.data {
                    edge_properties.insert(
                        (edge.from_node.clone(), edge_label, edge.to_node.clone()),
                        edge_data.properties.clone(),
                    );
                }
            }
        }

        // Suppress unused-PolicyAction/PolicyTargetKind warnings — both
        // are used inside the helper closures via the per-kind helpers
        // declared at the bottom of this file.
        let _ = (PolicyAction::Select, PolicyTargetKind::Nodes);

        Ok((graph, node_properties, edge_properties))
    }
}
