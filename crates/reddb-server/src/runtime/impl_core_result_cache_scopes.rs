use super::graph_tvf::is_graph_tvf_name;
use super::*;

fn cache_scope_insert(scopes: &mut HashSet<String>, name: &str) {
    if name.is_empty() || name.starts_with("__subq_") || is_universal_query_source(name) {
        return;
    }
    scopes.insert(name.to_string());
}

fn collect_table_source_scopes(scopes: &mut HashSet<String>, query: &TableQuery) {
    match query.source.as_ref() {
        Some(crate::storage::query::ast::TableSource::Name(name)) => {
            cache_scope_insert(scopes, name)
        }
        Some(crate::storage::query::ast::TableSource::Subquery(subquery)) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        // Graph-collection TVFs (e.g. `louvain(g)`) read the graph store
        // read-only. The result is now cached (issue #802) and scoped to the
        // graph collection named in the first argument, so any mutation on
        // that collection (`INSERT INTO g NODE/EDGE …`) invalidates the
        // entry via `invalidate_result_cache_for_table`. Non-graph or
        // zero-arg functions contribute no scope.
        Some(crate::storage::query::ast::TableSource::Function { name, args, .. }) => {
            if is_graph_tvf_name(name) {
                if let Some(graph) = args.first() {
                    cache_scope_insert(scopes, graph);
                }
            }
        }
        // The inline-graph form reads ordinary tables/docs through its
        // `nodes`/`edges` subqueries, so its result cache must be scoped to
        // those source collections — mutating any of them invalidates the
        // cached result (issue #799).
        Some(crate::storage::query::ast::TableSource::InlineGraphFunction {
            nodes, edges, ..
        }) => {
            collect_query_expr_result_cache_scopes(scopes, nodes);
            collect_query_expr_result_cache_scopes(scopes, edges);
        }
        None => cache_scope_insert(scopes, &query.table),
    }
}

fn collect_vector_source_scopes(
    scopes: &mut HashSet<String>,
    source: &crate::storage::query::ast::VectorSource,
) {
    match source {
        crate::storage::query::ast::VectorSource::Reference { collection, .. } => {
            cache_scope_insert(scopes, collection);
        }
        crate::storage::query::ast::VectorSource::Subquery(subquery) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        crate::storage::query::ast::VectorSource::Literal(_)
        | crate::storage::query::ast::VectorSource::Text(_) => {}
    }
}

fn collect_path_selector_scopes(
    scopes: &mut HashSet<String>,
    selector: &crate::storage::query::ast::NodeSelector,
) {
    if let crate::storage::query::ast::NodeSelector::ByRow { table, .. } = selector {
        cache_scope_insert(scopes, table);
    }
}

fn collect_query_expr_result_cache_scopes(scopes: &mut HashSet<String>, expr: &QueryExpr) {
    match expr {
        QueryExpr::Table(query) => collect_table_source_scopes(scopes, query),
        QueryExpr::Join(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.left);
            collect_query_expr_result_cache_scopes(scopes, &query.right);
        }
        QueryExpr::Path(query) => {
            collect_path_selector_scopes(scopes, &query.from);
            collect_path_selector_scopes(scopes, &query.to);
        }
        QueryExpr::Vector(query) => {
            cache_scope_insert(scopes, &query.collection);
            collect_vector_source_scopes(scopes, &query.query_vector);
        }
        QueryExpr::Hybrid(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.structured);
            cache_scope_insert(scopes, &query.vector.collection);
            collect_vector_source_scopes(scopes, &query.vector.query_vector);
        }
        QueryExpr::Insert(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Update(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Delete(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropGraph(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropDocument(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropKv(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::Truncate(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateVcsRef(_) | QueryExpr::DropVcsRef(_) => {
            cache_scope_insert(scopes, crate::application::vcs_collections::REFS)
        }
        QueryExpr::CreateIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::DropIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateMetric(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::AlterMetric(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::CreateSlo(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::DropTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::QueueSelect(query) => cache_scope_insert(scopes, &query.queue),
        QueryExpr::QueueCommand(query) => match query {
            QueueCommand::Push { queue, .. }
            | QueueCommand::Pop { queue, .. }
            | QueueCommand::Peek { queue, .. }
            | QueueCommand::Len { queue }
            | QueueCommand::Purge { queue }
            | QueueCommand::GroupCreate { queue, .. }
            | QueueCommand::GroupRead { queue, .. }
            | QueueCommand::Pending { queue, .. }
            | QueueCommand::Claim { queue, .. }
            | QueueCommand::Ack { queue, .. }
            | QueueCommand::Nack { queue, .. } => cache_scope_insert(scopes, queue),
            QueueCommand::Move {
                source,
                destination,
                ..
            } => {
                cache_scope_insert(scopes, source);
                cache_scope_insert(scopes, destination);
            }
        },
        QueryExpr::EventsBackfill(query) => {
            cache_scope_insert(scopes, &query.collection);
            cache_scope_insert(scopes, &query.target_queue);
        }
        QueryExpr::CreateTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::DropTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::TreeCommand(query) => match query {
            TreeCommand::Insert { collection, .. }
            | TreeCommand::Move { collection, .. }
            | TreeCommand::Delete { collection, .. }
            | TreeCommand::Validate { collection, .. }
            | TreeCommand::Rebalance { collection, .. } => cache_scope_insert(scopes, collection),
        },
        QueryExpr::SearchCommand(query) => match query {
            SearchCommand::Similar { collection, .. }
            | SearchCommand::Hybrid { collection, .. }
            | SearchCommand::SpatialRadius { collection, .. }
            | SearchCommand::SpatialBbox { collection, .. }
            | SearchCommand::SpatialNearest { collection, .. } => {
                cache_scope_insert(scopes, collection);
            }
            SearchCommand::Text { collection, .. }
            | SearchCommand::Multimodal { collection, .. }
            | SearchCommand::Index { collection, .. }
            | SearchCommand::Context { collection, .. } => {
                if let Some(collection) = collection.as_deref() {
                    cache_scope_insert(scopes, collection);
                }
            }
        },
        QueryExpr::Ask(query) => {
            if let Some(collection) = query.collection.as_deref() {
                cache_scope_insert(scopes, collection);
            }
        }
        QueryExpr::ExplainAlter(query) => cache_scope_insert(scopes, &query.target.name),
        QueryExpr::MaintenanceCommand(cmd) => match cmd {
            crate::storage::query::ast::MaintenanceCommand::Vacuum { target, .. }
            | crate::storage::query::ast::MaintenanceCommand::Analyze { target } => {
                if let Some(t) = target {
                    cache_scope_insert(scopes, t);
                }
            }
        },
        QueryExpr::CopyFrom(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateView(cmd) => {
            cache_scope_insert(scopes, &cmd.name);
            // Invalidating the view should also invalidate its dependencies.
            collect_query_expr_result_cache_scopes(scopes, &cmd.query);
        }
        QueryExpr::DropView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::RefreshMaterializedView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::CreatePolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::DropPolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateServer(_) | QueryExpr::DropServer(_) => {}
        QueryExpr::CreateForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::DropForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::Graph(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::ProbabilisticCommand(_)
        | QueryExpr::SetConfig { .. }
        | QueryExpr::ShowConfig { .. }
        | QueryExpr::SetSecret { .. }
        | QueryExpr::DeleteSecret { .. }
        | QueryExpr::ShowSecrets { .. }
        | QueryExpr::SetKv { .. }
        | QueryExpr::DeleteKv { .. }
        | QueryExpr::SetTenant(_)
        | QueryExpr::ShowTenant
        | QueryExpr::TransactionControl(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::Grant(_)
        | QueryExpr::Revoke(_)
        | QueryExpr::AlterUser(_)
        | QueryExpr::CreateUser(_)
        | QueryExpr::CreateIamPolicy { .. }
        | QueryExpr::DropIamPolicy { .. }
        | QueryExpr::AttachPolicy { .. }
        | QueryExpr::DetachPolicy { .. }
        | QueryExpr::ShowPolicies { .. }
        | QueryExpr::ShowEffectivePermissions { .. }
        | QueryExpr::RankOf(_)
        | QueryExpr::ApproxRankOf(_)
        | QueryExpr::RankRange(_)
        | QueryExpr::SimulatePolicy { .. }
        | QueryExpr::LintPolicy { .. }
        | QueryExpr::MigratePolicyMode { .. }
        | QueryExpr::CreateMigration(_)
        | QueryExpr::ApplyMigration(_)
        | QueryExpr::RollbackMigration(_)
        | QueryExpr::ExplainMigration(_)
        | QueryExpr::EventsBackfillStatus { .. } => {}
        QueryExpr::KvCommand(cmd) => {
            use crate::storage::query::ast::KvCommand;
            match cmd {
                KvCommand::Put { collection, .. }
                | KvCommand::InvalidateTags { collection, .. }
                | KvCommand::Get { collection, .. }
                | KvCommand::Unseal { collection, .. }
                | KvCommand::Rotate { collection, .. }
                | KvCommand::History { collection, .. }
                | KvCommand::List { collection, .. }
                | KvCommand::Purge { collection, .. }
                | KvCommand::Watch { collection, .. }
                | KvCommand::Delete { collection, .. }
                | KvCommand::Incr { collection, .. }
                | KvCommand::Cas { collection, .. } => cache_scope_insert(scopes, collection),
            }
        }
        QueryExpr::ConfigCommand(cmd) => {
            use crate::storage::query::ast::ConfigCommand;
            match cmd {
                ConfigCommand::Put { collection, .. }
                | ConfigCommand::Get { collection, .. }
                | ConfigCommand::Resolve { collection, .. }
                | ConfigCommand::Rotate { collection, .. }
                | ConfigCommand::Delete { collection, .. }
                | ConfigCommand::History { collection, .. }
                | ConfigCommand::List { collection, .. }
                | ConfigCommand::Watch { collection, .. }
                | ConfigCommand::InvalidVolatileOperation { collection, .. } => {
                    cache_scope_insert(scopes, collection)
                }
            }
        }
        _ => {}
    }
}

/// Collect every concrete table reference inside a `QueryExpr`.
///
/// Used by view bookkeeping (dependency tracking for materialised
/// invalidation) and any other rewriter that needs to know the base
/// tables a query pulls from. Does not descend into projections/filters;
/// only the `FROM` side.
pub(crate) fn collect_table_refs(expr: &QueryExpr) -> Vec<String> {
    let mut scopes: HashSet<String> = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes.into_iter().collect()
}

pub(super) fn query_expr_result_cache_scopes(expr: &QueryExpr) -> HashSet<String> {
    let mut scopes = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes
}
