//! Query privilege gate extracted from `impl_core` (issue #1622, PRD
//! #1619): [`RedDBRuntime::check_query_privilege`] and the per-domain
//! privilege gates it dispatches to. Behaviour-preserving move; the free
//! IAM/policy-column helpers these consume live in [`super::policy_columns`].
use super::super::execution_context::{current_auth_identity, current_tenant};
use super::super::*;
use super::policy_columns::*;

/// Collect every table a statement reads *below* its top level: FROM
/// subqueries and derived tables, join children, `IN (SELECT …)` and scalar
/// subqueries in projections and predicates, the structured half of a
/// hybrid search, and vector sources. The top-level table is the caller's
/// business; synthetic subquery wrappers (`__subq_N`) and the source-free
/// scalar table (`any`) name no real table and are skipped.
fn collect_nested_read_tables(expr: &reddb_rql::ast::QueryExpr, out: &mut Vec<String>) {
    use reddb_rql::ast::{QueryExpr, TableSource, VectorSource};

    fn push_table(name: &str, out: &mut Vec<String>) {
        if name.is_empty() || name == "any" || name.starts_with("__subq_") {
            return;
        }
        if !out.iter().any(|seen| seen == name) {
            out.push(name.to_string());
        }
    }

    // A nested statement counts in full: its own table plus whatever it
    // nests. Only the outermost statement's table is excluded.
    fn walk_nested(expr: &QueryExpr, out: &mut Vec<String>) {
        match expr {
            QueryExpr::Table(t) if t.source.is_none() => push_table(&t.table, out),
            QueryExpr::Insert(i) => push_table(&i.table, out),
            QueryExpr::Update(u) => push_table(&u.table, out),
            QueryExpr::Delete(d) => push_table(&d.table, out),
            _ => {}
        }
        collect_nested_read_tables(expr, out);
    }

    match expr {
        QueryExpr::Table(t) => {
            match &t.source {
                Some(TableSource::Subquery(inner)) => walk_nested(inner, out),
                Some(TableSource::InlineGraphFunction { nodes, edges, .. }) => {
                    walk_nested(nodes, out);
                    walk_nested(edges, out);
                }
                Some(TableSource::Name(name)) => push_table(name, out),
                Some(TableSource::Function { .. }) | None => {}
            }
            for item in &t.select_items {
                if let reddb_rql::ast::SelectItem::Expr { expr, .. } = item {
                    collect_expr_subqueries(expr, out);
                }
            }
            if let Some(e) = &t.where_expr {
                collect_expr_subqueries(e, out);
            }
            if let Some(e) = &t.having_expr {
                collect_expr_subqueries(e, out);
            }
            for e in &t.group_by_exprs {
                collect_expr_subqueries(e, out);
            }
        }
        QueryExpr::Join(j) => {
            walk_nested(&j.left, out);
            walk_nested(&j.right, out);
        }
        QueryExpr::Hybrid(h) => {
            walk_nested(&h.structured, out);
            if let VectorSource::Subquery(inner) = &h.vector.query_vector {
                walk_nested(inner, out);
            }
        }
        QueryExpr::Vector(v) => {
            if let VectorSource::Subquery(inner) = &v.query_vector {
                walk_nested(inner, out);
            }
        }
        QueryExpr::Update(u) => {
            if let Some(e) = &u.where_expr {
                collect_expr_subqueries(e, out);
            }
        }
        QueryExpr::Delete(d) => {
            if let Some(e) = &d.where_expr {
                collect_expr_subqueries(e, out);
            }
        }
        QueryExpr::Explain(e) => collect_nested_read_tables(&e.inner, out),
        _ => {}
    }
}

fn collect_expr_subqueries(expr: &reddb_rql::ast::Expr, out: &mut Vec<String>) {
    use reddb_rql::ast::Expr;
    match expr {
        Expr::Subquery { query, .. } => {
            let inner: &reddb_rql::ast::QueryExpr = &query.query;
            if let reddb_rql::ast::QueryExpr::Table(t) = inner {
                if t.source.is_none()
                    && t.table != "any"
                    && !t.table.starts_with("__subq_")
                    && !out.iter().any(|seen| seen == &t.table)
                {
                    out.push(t.table.clone());
                }
            }
            collect_nested_read_tables(inner, out);
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            collect_expr_subqueries(lhs, out);
            collect_expr_subqueries(rhs, out);
        }
        Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => {
            collect_expr_subqueries(operand, out);
        }
        Expr::Cast { inner, .. } => collect_expr_subqueries(inner, out),
        Expr::FunctionCall { args, .. } | Expr::WindowFunctionCall { args, .. } => {
            for arg in args {
                collect_expr_subqueries(arg, out);
            }
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (when, then) in branches {
                collect_expr_subqueries(when, out);
                collect_expr_subqueries(then, out);
            }
            if let Some(e) = else_ {
                collect_expr_subqueries(e, out);
            }
        }
        Expr::InList { target, values, .. } => {
            collect_expr_subqueries(target, out);
            for v in values {
                collect_expr_subqueries(v, out);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            collect_expr_subqueries(target, out);
            collect_expr_subqueries(low, out);
            collect_expr_subqueries(high, out);
        }
        Expr::Literal { .. } | Expr::Column { .. } | Expr::Parameter { .. } => {}
    }
}

/// Role-tier gate for statements that have no finer-grained resource:
/// `role` must be at least `needed`.
fn require_role_tier(
    username: &str,
    role: crate::auth::Role,
    needed: crate::auth::Role,
    statement: &str,
) -> Result<(), String> {
    if role >= needed {
        Ok(())
    } else {
        Err(format!(
            "principal=`{username}` role=`{role:?}` cannot issue {statement} (requires {needed:?})"
        ))
    }
}

/// Human label for the statement families the tier gate names in its
/// denial message.
fn statement_family(expr: &reddb_rql::ast::QueryExpr) -> &'static str {
    use reddb_rql::ast::QueryExpr;
    match expr {
        QueryExpr::Scrub { .. } => "SCRUB",
        QueryExpr::MaintenanceCommand { .. } => "a maintenance command",
        QueryExpr::EventsBackfill { .. } => "EVENTS BACKFILL",
        QueryExpr::ForkStore { .. } => "FORK STORE",
        QueryExpr::PromoteFork { .. } => "PROMOTE FORK",
        QueryExpr::DropFork { .. } => "DROP FORK",
        QueryExpr::ShowSecrets { .. } => "SHOW SECRETS",
        QueryExpr::CreateVcsRef { .. } => "CREATE REF",
        QueryExpr::DropVcsRef { .. } => "DROP REF",
        _ => "this statement",
    }
}

impl RedDBRuntime {
    /// Project a `QueryExpr` to the (action, resource) pair the
    /// privilege engine cares about. Returns `Ok(())` for statements
    /// that don't touch user data (transaction control, SHOW, SET, etc.).
    pub(crate) fn check_query_privilege(
        &self,
        expr: &reddb_rql::ast::QueryExpr,
    ) -> Result<(), String> {
        use crate::auth::privileges::{Action, AuthzContext, Resource};
        use crate::auth::UserId;
        use reddb_rql::ast::{
            ConfigCommand, KvCommand, ProbabilisticCommand, QueryExpr, TreeCommand,
        };

        // No auth store wired (embedded mode / fresh DB / tests) → bypass.
        // The bootstrap path itself goes through `execute_query` so this
        // is the only sensible default; once auth is wired, the gate
        // becomes active.
        let auth_store = match self.inner.auth_store.read().clone() {
            Some(s) => s,
            None => return Ok(()),
        };

        // Resolve principal + role from the thread-local identity.
        // Anonymous (no identity) is allowed to read the bootstrap path
        // only when auth_store says so; we treat missing identity as
        // platform-admin-equivalent here so embedded test harnesses
        // continue to work without setting an identity.
        let (username, role) = match current_auth_identity() {
            Some(p) => p,
            None => return Ok(()),
        };
        let tenant = current_tenant();

        let ctx = AuthzContext {
            principal: &username,
            effective_role: role,
            tenant: tenant.as_deref(),
        };
        let principal_id = UserId::from_parts(tenant.as_deref(), &username);

        // Map QueryExpr → (Action, Resource).
        let (action, resource) = match expr {
            QueryExpr::Table(t) => (Action::Select, Resource::table_from_name(&t.table)),
            QueryExpr::RankOf(_) | QueryExpr::ApproxRankOf(_) | QueryExpr::RankRange(_) => {
                (Action::Select, Resource::Database)
            }
            QueryExpr::QueueSelect(q) => {
                return self.check_queue_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "queue:peek",
                    &q.queue,
                );
            }
            QueryExpr::QueueCommand(cmd) => {
                use reddb_rql::ast::QueueCommand;
                let (queue, action_verb) = match cmd {
                    QueueCommand::Push { queue, .. } => (queue.as_str(), "queue:enqueue"),
                    QueueCommand::Pop { queue, .. }
                    | QueueCommand::GroupRead { queue, .. }
                    | QueueCommand::Claim { queue, .. } => (queue.as_str(), "queue:read"),
                    QueueCommand::Peek { queue, .. }
                    | QueueCommand::Len { queue }
                    | QueueCommand::Pending { queue, .. } => (queue.as_str(), "queue:peek"),
                    QueueCommand::Ack { queue, .. } => (queue.as_str(), "queue:ack"),
                    QueueCommand::Nack {
                        queue, delay_ms, ..
                    } => {
                        // Per-failure retry overrides re-shape retry
                        // behaviour for everyone draining the queue and
                        // gate on the dedicated `queue:retry` verb so
                        // operators can grant base NACK without granting
                        // the override capability.
                        let verb = if delay_ms.is_some() {
                            "queue:retry"
                        } else {
                            "queue:nack"
                        };
                        (queue.as_str(), verb)
                    }
                    QueueCommand::Purge { queue } => (queue.as_str(), "queue:purge"),
                    // `GroupCreate` is part of the consumer-setup
                    // surface — read-side, never destructive.
                    QueueCommand::GroupCreate { queue, .. } => (queue.as_str(), "queue:read"),
                    QueueCommand::Move { source, .. } => (source.as_str(), "queue:dlq:move"),
                };
                return self.check_queue_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action_verb,
                    queue,
                );
            }
            QueryExpr::Graph(g) => {
                // MATCH … RETURN is the explorer's pattern-traversal
                // surface — gate on `graph:traverse` (#757).
                self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "graph:traverse",
                )?;
                if auth_store.iam_authorization_enabled() {
                    self.check_graph_property_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        g,
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Path(_) => {
                // PATH FROM … TO … is a path-traversal query — gates
                // on `graph:traverse` like neighborhood/shortest-path
                // (#757).
                return self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "graph:traverse",
                );
            }
            QueryExpr::GraphCommand(cmd) => {
                use reddb_rql::ast::GraphCommand;
                let action_verb = match cmd {
                    // Metadata / property reads.
                    GraphCommand::Properties { .. } => "graph:read",
                    // Traversal / pattern-walk surface.
                    GraphCommand::Neighborhood { .. }
                    | GraphCommand::Traverse { .. }
                    | GraphCommand::ShortestPath { .. } => "graph:traverse",
                    // Analytics algorithms — expensive enough that Red
                    // UI needs to gate the runner independently of
                    // ordinary traversal.
                    GraphCommand::Centrality { .. }
                    | GraphCommand::Community { .. }
                    | GraphCommand::Components { .. }
                    | GraphCommand::Cycles { .. }
                    | GraphCommand::Clustering
                    | GraphCommand::TopologicalSort => "graph:algorithm:run",
                };
                return self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action_verb,
                );
            }
            QueryExpr::Vector(v) => {
                if auth_store.iam_authorization_enabled() {
                    self.check_vector_op_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        "vector:search",
                        &v.collection,
                    )?;
                    self.check_table_like_column_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        &v.collection,
                        &["content".to_string()],
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::SearchCommand(cmd) => {
                use reddb_rql::ast::SearchCommand;
                if auth_store.iam_authorization_enabled() {
                    // `SEARCH SIMILAR [..] COLLECTION <c>` and `SEARCH
                    // HYBRID ... COLLECTION <c>` are the same UI
                    // affordances as `VECTOR SEARCH` / hybrid joins —
                    // Red UI must see the same `vector:search` envelope
                    // so a single toolbar grant is sufficient.
                    let collection = match cmd {
                        SearchCommand::Similar { collection, .. }
                        | SearchCommand::Hybrid { collection, .. } => Some(collection.as_str()),
                        _ => None,
                    };
                    if let Some(c) = collection {
                        self.check_vector_op_privilege(
                            &auth_store,
                            &principal_id,
                            role,
                            tenant.as_deref(),
                            "vector:search",
                            c,
                        )?;
                        return Ok(());
                    }
                }
                return Ok(());
            }
            QueryExpr::Hybrid(h) => {
                // The structured half is an arbitrary statement — a table
                // read, a join, another hybrid — and needs its own gate in
                // every enforcement mode; the vector half is checked below.
                self.check_query_privilege(&h.structured)?;
                if auth_store.iam_authorization_enabled() {
                    // The vector half of a hybrid search is gated under
                    // the same `vector:search` verb as a standalone
                    // VECTOR SEARCH — Red UI's hybrid-search toolbar
                    // must surface the same UI-safe denial envelope
                    // when the principal lacks the grant. The
                    // structured half is dispatched to its own gate via
                    // the inner query during execution.
                    self.check_vector_op_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        "vector:search",
                        &h.vector.collection,
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Insert(i) => (Action::Insert, Resource::table_from_name(&i.table)),
            QueryExpr::Update(u) => (Action::Update, Resource::table_from_name(&u.table)),
            QueryExpr::Delete(d) => (Action::Delete, Resource::table_from_name(&d.table)),
            // Joins inherit the read privilege from any constituent
            // table — for now we emit a single Select on the database
            // (admins bypass; non-admins need a Database/Schema grant).
            QueryExpr::Join(_) => (Action::Select, Resource::Database),
            // GRANT / REVOKE / USER DDL are authority statements;
            // require Admin (the helper methods enforce).
            QueryExpr::Grant(_)
            | QueryExpr::Revoke(_)
            | QueryExpr::AlterUser(_)
            | QueryExpr::CreateUser(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                        username, role
                    ))
                };
            }
            QueryExpr::CreateIamPolicy { id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:put",
                    "policy",
                    id,
                );
            }
            QueryExpr::DropIamPolicy { id } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:drop",
                    "policy",
                    id,
                );
            }
            QueryExpr::AttachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:attach",
                    "policy",
                    policy_id,
                );
            }
            QueryExpr::DetachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:detach",
                    "policy",
                    policy_id,
                );
            }
            // A principal may always inspect its own effective set; anything
            // wider (every policy, or another principal's set — including one
            // in another tenant, since the target tenant comes from the
            // statement text) is policy management.
            QueryExpr::ShowPolicies { filter } => {
                // Admins inspect policies as part of managing them; the
                // explicit-allow requirement below is for everyone else.
                if role == crate::auth::Role::Admin {
                    return Ok(());
                }
                let self_only = matches!(
                    filter,
                    Some(reddb_rql::ast::PolicyPrincipalRef::User(u))
                        if u.username == username && u.tenant.as_deref() == tenant.as_deref()
                );
                if self_only {
                    return Ok(());
                }
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::ShowEffectivePermissions { user, .. } => {
                // Admins inspect policies as part of managing them; the
                // explicit-allow requirement below is for everyone else.
                if role == crate::auth::Role::Admin {
                    return Ok(());
                }
                if user.username == username && user.tenant.as_deref() == tenant.as_deref() {
                    return Ok(());
                }
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::SimulatePolicy { .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::LintPolicy { .. } => {
                // Linting is a read-only inspection — gate it like
                // simulate (policy management role).
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::MigratePolicyMode { dry_run, .. } => {
                // DRY RUN is a pre-flight inspection (policy:simulate).
                // The actual mode flip is a privileged mutation under
                // the policy:put action (it persists a new enforcement
                // mode to the vault KV through `set_enforcement_mode`).
                let action = if *dry_run {
                    "policy:simulate"
                } else {
                    "policy:put"
                };
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action,
                    "policy",
                    "*",
                );
            }
            // DROP and TRUNCATE — Write-role gate + per-collection IAM policy
            // when IAM mode is active. Other DDL stays role-only for now.
            QueryExpr::DropTable(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropGraph(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropVector(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropDocument(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropKv(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropCollection(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::Truncate(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "truncate",
                    &q.name,
                );
            }
            // Remaining DDL (#753) — hybrid policy-aware gate. Specific
            // create/alter/drop verbs gate operations with a clear
            // per-collection target so Red UI can author fine-grained
            // policies (`create on collection:users`). Namespace-level
            // and grouped DDL fall back to broader `schema:admin` /
            // `schema:write` verbs against a `schema:<name>` resource.
            // All branches share the [`check_ddl_object_privilege`]
            // helper so allows / denies produce the same structured
            // "principal=… action=… resource=<kind>:<name> denied by
            // IAM policy" reason the Red UI security read contracts
            // (#740) already render.
            QueryExpr::CreateTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateCollection(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateVector(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateIndex(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropIndex(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateSchema(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::DropSchema(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::CreateSequence(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropSequence(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::RefreshMaterializedView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreatePolicy(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropPolicy(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateServer(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::DropServer(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::CreateForeignTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropForeignTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateTimeSeries(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateMetric(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterMetric(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateSlo(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropTimeSeries(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateTree(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.collection,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropTree(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.collection,
                    crate::auth::Role::Write,
                );
            }
            // Migration DDL — CREATE MIGRATION is grouped DDL on the
            // schema namespace; uses the `schema:write` fallback verb
            // (no obvious per-collection target).
            QueryExpr::CreateMigration(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            // APPLY / ROLLBACK change data and schema — require Admin.
            QueryExpr::ApplyMigration(_) | QueryExpr::RollbackMigration(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue APPLY/ROLLBACK MIGRATION",
                        username, role
                    ))
                };
            }
            // EXPLAIN MIGRATION is read-only — any authenticated principal.
            QueryExpr::ExplainMigration(_) => return Ok(()),
            // EXPLAIN plans the inner statement and reports its schema and
            // cardinality, so it needs exactly what that statement needs.
            QueryExpr::Explain(explain) => return self.check_query_privilege(&explain.inner),
            // EXPLAIN ALTER simulates a schema change on the named table and
            // reports the resulting layout — a read of that table.
            QueryExpr::ExplainAlter(explain) => (
                Action::Select,
                Resource::table_from_name(&explain.target.name),
            ),
            // ASK retrieves over the store before calling the model.
            QueryExpr::Ask { .. } => (Action::Select, Resource::Database),
            // Operator-grade store operations: whole-store forks, scrub,
            // maintenance, event backfill, and the secret catalogue.
            QueryExpr::Scrub { .. }
            | QueryExpr::MaintenanceCommand { .. }
            | QueryExpr::EventsBackfill { .. }
            | QueryExpr::ForkStore { .. }
            | QueryExpr::PromoteFork { .. }
            | QueryExpr::DropFork { .. }
            | QueryExpr::ShowSecrets { .. } => {
                return require_role_tier(
                    &username,
                    role,
                    crate::auth::Role::Admin,
                    statement_family(expr),
                );
            }
            // Mutations of VCS refs, trees, probabilistic structures and the
            // KV / config namespaces need the Write tier; their reads are
            // open to any authenticated principal, and the executor applies
            // the per-key IAM checks on top.
            QueryExpr::CreateVcsRef { .. } | QueryExpr::DropVcsRef { .. } => {
                return require_role_tier(
                    &username,
                    role,
                    crate::auth::Role::Write,
                    statement_family(expr),
                );
            }
            QueryExpr::TreeCommand(cmd) => {
                return if matches!(cmd, TreeCommand::Validate { .. }) {
                    Ok(())
                } else {
                    require_role_tier(&username, role, crate::auth::Role::Write, "tree command")
                };
            }
            QueryExpr::ProbabilisticCommand(cmd) => {
                let read_only = matches!(
                    cmd,
                    ProbabilisticCommand::HllCount { .. }
                        | ProbabilisticCommand::HllInfo { .. }
                        | ProbabilisticCommand::SketchCount { .. }
                        | ProbabilisticCommand::SketchInfo { .. }
                        | ProbabilisticCommand::FilterCheck { .. }
                        | ProbabilisticCommand::FilterCount { .. }
                        | ProbabilisticCommand::FilterInfo { .. }
                );
                return if read_only {
                    Ok(())
                } else {
                    require_role_tier(
                        &username,
                        role,
                        crate::auth::Role::Write,
                        "probabilistic structure command",
                    )
                };
            }
            QueryExpr::KvCommand(cmd) => {
                let mutation = matches!(
                    cmd,
                    KvCommand::Put { .. }
                        | KvCommand::InvalidateTags { .. }
                        | KvCommand::Unseal { .. }
                        | KvCommand::Rotate { .. }
                );
                return if mutation {
                    require_role_tier(&username, role, crate::auth::Role::Write, "KV command")
                } else {
                    Ok(())
                };
            }
            QueryExpr::ConfigCommand(cmd) => {
                let mutation = matches!(
                    cmd,
                    ConfigCommand::Put { .. } | ConfigCommand::Rotate { .. }
                );
                return if mutation {
                    require_role_tier(&username, role, crate::auth::Role::Write, "config command")
                } else {
                    Ok(())
                };
            }
            // Read-only status and session statements.
            QueryExpr::EventsBackfillStatus { .. }
            | QueryExpr::ShowTenant
            | QueryExpr::TransactionControl { .. } => return Ok(()),
            // Gated at dispatch by a dedicated check, because the gate needs
            // the executor's view of the key or path the statement names:
            // `check_config_write_privilege` / `check_config_read_privilege`,
            // `check_secret_write_privilege`, `check_kv_write_privilege`,
            // `check_set_tenant_privilege`, `check_copy_from_privilege`,
            // `check_vcs_command_privilege`.
            QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::SetSecret { .. }
            | QueryExpr::DeleteSecret { .. }
            | QueryExpr::SetKv { .. }
            | QueryExpr::DeleteKv { .. }
            | QueryExpr::SetTenant { .. }
            | QueryExpr::CopyFrom { .. }
            | QueryExpr::VcsCommand { .. } => return Ok(()),
            // No wildcard: a statement kind added later fails to compile
            // here until it is classified, instead of running ungated.
        };

        // The outer statement names one table; the tables it reads through
        // a FROM-subquery, a derived table, a join child, `IN (SELECT …)` or
        // a vector source were never checked, so a principal with no grant
        // on `salaries` could still read it as `SELECT * FROM (SELECT * FROM
        // salaries) s`. Every nested read needs Select on its own table.
        let mut nested_tables = Vec::new();
        collect_nested_read_tables(expr, &mut nested_tables);
        for table in nested_tables {
            let nested_resource = Resource::table_from_name(&table);
            if auth_store.iam_authorization_enabled() {
                let iam_action = legacy_action_to_iam(Action::Select);
                let iam_resource = legacy_resource_to_iam(&nested_resource, tenant.as_deref());
                let iam_ctx = runtime_iam_context(role, tenant.as_deref());
                if !auth_store.check_policy_authz_with_role(
                    &principal_id,
                    iam_action,
                    &iam_resource,
                    &iam_ctx,
                    role,
                ) {
                    return Err(format!(
                        "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                        username, iam_action, iam_resource.kind, iam_resource.name
                    ));
                }
            } else {
                auth_store
                    .check_grant(&ctx, Action::Select, &nested_resource)
                    .map_err(|e| e.to_string())?;
            }
        }

        // A derived-table wrapper (`SELECT … FROM (SELECT …) s`) names only
        // the synthetic `__subq_*` table; every read it performs was just
        // checked above, and there is no grant to hold on the wrapper.
        if matches!(expr, QueryExpr::Table(t) if t.table.starts_with("__subq_")) {
            return Ok(());
        }

        if auth_store.iam_authorization_enabled() {
            let iam_action = legacy_action_to_iam(action);
            let iam_resource = legacy_resource_to_iam(&resource, tenant.as_deref());
            let iam_ctx = runtime_iam_context(role, tenant.as_deref());
            if !auth_store.check_policy_authz_with_role(
                &principal_id,
                iam_action,
                &iam_resource,
                &iam_ctx,
                role,
            ) {
                return Err(format!(
                    "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                    username, iam_action, iam_resource.kind, iam_resource.name
                ));
            }

            if let QueryExpr::Table(table) = expr {
                self.check_table_column_projection_privilege(
                    &auth_store,
                    &principal_id,
                    &iam_ctx,
                    table,
                )?;
            }

            if let QueryExpr::Update(update) = expr {
                let columns = update_set_target_columns(update);
                if !columns.is_empty() {
                    let request = column_access_request_for_table_update(&update.table, columns);
                    let outcome =
                        auth_store.check_column_projection_authz(&principal_id, &request, &iam_ctx);
                    if let Some(denied) = outcome.first_denied_column() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM column policy",
                            username, iam_action, denied.resource.kind, denied.resource.name
                        ));
                    }
                    if !outcome.allowed() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                            username,
                            iam_action,
                            outcome.table_resource.kind,
                            outcome.table_resource.name
                        ));
                    }
                }

                if let Some(columns) = update_returning_columns_for_policy(self, update) {
                    let request = column_access_request_for_table_select(&update.table, columns);
                    let outcome =
                        auth_store.check_column_projection_authz(&principal_id, &request, &iam_ctx);
                    if let Some(denied) = outcome.first_denied_column() {
                        return Err(format!(
                            "principal=`{}` action=`select` resource=`{}:{}` denied by IAM column policy",
                            username, denied.resource.kind, denied.resource.name
                        ));
                    }
                    if !outcome.allowed() {
                        return Err(format!(
                            "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                            username, outcome.table_resource.kind, outcome.table_resource.name
                        ));
                    }
                }
            }

            Ok(())
        } else {
            auth_store
                .check_grant(&ctx, action, &resource)
                .map_err(|e| e.to_string())
        }
    }

    pub(crate) fn check_table_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        ctx: &crate::auth::policies::EvalContext,
        table: &reddb_rql::ast::TableQuery,
    ) -> Result<(), String> {
        use crate::auth::{ColumnAccessRequest, ColumnDecisionEffect};

        let columns = requested_table_columns_for_policy(table);
        if columns.is_empty() {
            return Ok(());
        }

        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let outcome = auth_store.check_column_projection_authz(principal, &request, ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if !matches!(
            outcome.table_decision,
            crate::auth::policies::Decision::Allow { .. }
                | crate::auth::policies::Decision::AdminBypass
        ) {
            return Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, outcome.table_resource.kind, outcome.table_resource.name
            ));
        }

        let denied = outcome
            .first_denied_column()
            .filter(|decision| decision.effective == ColumnDecisionEffect::Denied);
        match denied {
            Some(decision) => Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, decision.resource.kind, decision.resource.name
            )),
            None => Ok(()),
        }
    }

    pub(crate) fn check_graph_property_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        query: &reddb_rql::ast::GraphQuery,
    ) -> Result<(), String> {
        let columns = explicit_graph_projection_properties(query);
        if columns.is_empty() {
            return Ok(());
        }
        self.check_table_like_column_projection_privilege(
            auth_store, principal, role, tenant, "graph", &columns,
        )
    }

    pub(crate) fn check_table_like_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        table: &str,
        columns: &[String],
    ) -> Result<(), String> {
        let iam_ctx = runtime_iam_context(role, tenant);
        let request =
            crate::auth::ColumnAccessRequest::select(table.to_string(), columns.iter().cloned());
        let outcome = auth_store.check_column_projection_authz(principal, &request, &iam_ctx);
        if outcome.allowed() {
            return Ok(());
        }
        let denied = outcome
            .first_denied_column()
            .map(|d| d.resource.name.clone())
            .unwrap_or_else(|| format!("{table}.<unknown>"));
        Err(format!(
            "principal=`{}` action=`select` resource=`column:{}` denied by IAM policy",
            principal, denied
        ))
    }

    pub(crate) fn check_policy_management_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        resource_kind: &str,
        resource_name: &str,
    ) -> Result<(), String> {
        let ctx = runtime_iam_context(role, tenant);

        if !auth_store.iam_authorization_enabled() {
            return if role == crate::auth::Role::Admin {
                Ok(())
            } else {
                Err(format!(
                    "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                    principal, role
                ))
            };
        }

        // Only a *policy-typed* managed entry hands the decision to the
        // execution-side `ManagedPolicyGate`. Matching on id alone let a
        // policy named after any managed entry of another type (e.g. the
        // cloud preset's `red.config.cloud` config namespace) skip this
        // check entirely, while the managed gate ignored the non-policy
        // entry — an unauthenticated route to an allow-all policy.
        if resource_kind == "policy"
            && matches!(
                action,
                "policy:put" | "policy:drop" | "policy:attach" | "policy:detach"
            )
            && self
                .inner
                .config_registry
                .get_active(resource_name)
                .map(|entry| {
                    entry.managed
                        && entry.resource_type == crate::auth::managed_policy::RESOURCE_TYPE_POLICY
                })
                .unwrap_or(false)
        {
            return Ok(());
        }

        let mut resource = crate::auth::policies::ResourceRef::new(
            resource_kind.to_string(),
            resource_name.to_string(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                principal, action, resource.kind, resource.name
            ))
        }
    }

    pub(crate) fn check_managed_config_write_for_set_config(&self, key: &str) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        let (username, role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let tenant = current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let ctx = runtime_iam_context(role, tenant.as_deref());
        let gate = crate::auth::managed_config::ManagedConfigGate::new(
            self.inner.config_registry.as_ref(),
        );
        match gate.check_write(&auth_store, &principal, &ctx, key) {
            crate::auth::managed_config::ManagedConfigDecision::PassThrough { .. }
            | crate::auth::managed_config::ManagedConfigDecision::Allow { .. } => Ok(()),
            crate::auth::managed_config::ManagedConfigDecision::Deny { reason, .. } => {
                Err(RedDBError::Query(format!(
                    "permission denied: managed config mutation blocked for `{key}`: {reason}"
                )))
            }
        }
    }

    pub(crate) fn check_secret_write_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        key: &str,
    ) -> RedDBResult<()> {
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let tenant = current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("secret".to_string(), key.to_string());
        if let Some(tenant) = &tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = runtime_iam_context(role, tenant.as_deref());
        if auth_store.check_policy_authz_with_role(
            &principal,
            "secret:write",
            &resource,
            &ctx,
            role,
        ) {
            return Ok(());
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{}` action=`secret:write` resource=`secret:{}` denied by IAM policy",
            principal, key
        )))
    }

    /// Gate for `SET CONFIG key = value`. Under IAM the principal needs
    /// `config:write` on `config:<key>`; under legacy RBAC any write-role
    /// principal may set ordinary keys, but the AI provider namespace
    /// (`red.config.ai.*`, which can redirect provider calls and select which
    /// vault secret is sent as a bearer) is admin-only. Embedded callers with
    /// no identity pass, as everywhere else in this gate.
    pub(crate) fn check_config_write_privilege(&self, key: &str) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let tenant = current_tenant();
        if auth_store.iam_authorization_enabled() {
            let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
            let mut resource =
                crate::auth::policies::ResourceRef::new("config".to_string(), key.to_string());
            if let Some(tenant) = &tenant {
                resource = resource.with_tenant(tenant.clone());
            }
            let ctx = runtime_iam_context(role, tenant.as_deref());
            if auth_store.check_policy_authz_with_role(
                &principal,
                "config:write",
                &resource,
                &ctx,
                role,
            ) {
                return Ok(());
            }
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` action=`config:write` resource=`config:{}` denied by IAM policy",
                principal, key
            )));
        }
        // Namespaces that steer where data goes or who may read it: the AI
        // egress (which vault secret is sent to which host), backup and WAL
        // archive destinations, secret auto-decryption, and the IAM / ACL
        // stores themselves. A write-role principal could otherwise repoint
        // the backup head or switch secret decryption on.
        let lowered = key.to_ascii_lowercase();
        let admin_only = [
            "red.config.ai.",
            "red.config.backup.",
            "red.config.wal.",
            "red.config.secret.",
            "red.config.iam.",
            "red.config.acl.",
            "red.config.replication.",
        ]
        .iter()
        .any(|prefix| lowered.starts_with(prefix));
        let allowed = if admin_only {
            role.can_admin()
        } else {
            role.can_write()
        };
        if allowed {
            Ok(())
        } else {
            Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` role=`{:?}` cannot SET CONFIG `{}`",
                username, role, key
            )))
        }
    }

    /// Gate for `SHOW CONFIG`. Under IAM the principal needs `config:read`
    /// on the config namespace (`config:*`); legacy RBAC lets any
    /// authenticated principal read.
    pub(crate) fn check_config_read_privilege(&self) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let tenant = current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("config".to_string(), "*".to_string());
        if let Some(tenant) = &tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = runtime_iam_context(role, tenant.as_deref());
        if auth_store.check_policy_authz_with_role(&principal, "config:read", &resource, &ctx, role)
        {
            return Ok(());
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{}` action=`config:read` resource=`config:*` denied by IAM policy",
            principal
        )))
    }

    /// Gate for the VCS commands that are dispatched before the statement
    /// frame is built (`CHECKPOINT`, `CHECKOUT`, `RESET`, `MERGE`, …).
    /// `RESET` rewinds the whole store and is admin-only; every other VCS
    /// command mutates history and needs at least the write role.
    pub(crate) fn check_vcs_command_privilege(
        &self,
        command: &super::super::vcs_command::RuntimeVcsCommand,
    ) -> RedDBResult<()> {
        use super::super::vcs_command::RuntimeVcsCommand;
        if self.inner.auth_store.read().is_none() {
            return Ok(());
        }
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let (verb, allowed) = match command {
            RuntimeVcsCommand::Reset { .. } => ("RESET", role.can_admin()),
            RuntimeVcsCommand::Checkpoint { .. } => ("CHECKPOINT", role.can_write()),
            RuntimeVcsCommand::Checkout { .. } => ("CHECKOUT", role.can_write()),
            RuntimeVcsCommand::Merge { .. } => ("MERGE", role.can_write()),
            RuntimeVcsCommand::CherryPick { .. } => ("CHERRY PICK", role.can_write()),
            RuntimeVcsCommand::Revert { .. } => ("REVERT", role.can_write()),
            RuntimeVcsCommand::ResolveConflict { .. } => ("RESOLVE CONFLICT", role.can_write()),
        };
        if allowed {
            Ok(())
        } else {
            Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` role=`{:?}` cannot issue {}",
                username, role, verb
            )))
        }
    }

    /// Gate for `COPY <table> FROM '<path>'`: the statement reads a file
    /// from the *server's* filesystem, so — like PostgreSQL's
    /// `pg_read_server_files` — it is admin-only whenever an identity is
    /// installed.
    pub(crate) fn check_copy_from_privilege(&self, table: &str) -> RedDBResult<()> {
        if self.inner.auth_store.read().is_none() {
            return Ok(());
        }
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        if role.can_admin() {
            Ok(())
        } else {
            Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` role=`{:?}` cannot COPY `{}` FROM a server file (admin only)",
                username, role, table
            )))
        }
    }

    /// Gate for `SET TENANT` / `RESET TENANT`: a tenant-scoped principal
    /// (its transport pinned a tenant) may only re-select its own tenant;
    /// switching to another tenant or clearing the scope would let it read
    /// through the victim tenant's RLS predicates. Platform-scoped
    /// principals (no tenant) and embedded callers keep the old behaviour.
    pub(crate) fn check_set_tenant_privilege(&self, target: Option<&str>) -> RedDBResult<()> {
        if self.inner.auth_store.read().is_none() {
            return Ok(());
        }
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let Some(own_tenant) = current_tenant() else {
            return Ok(());
        };
        if target == Some(own_tenant.as_str()) {
            return Ok(());
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{}` role=`{:?}` is scoped to tenant `{}` and cannot switch tenant",
            username, role, own_tenant
        )))
    }

    /// IAM gate for `SET KV` / `DELETE KV` writes (#1602). Mirrors
    /// [`Self::check_secret_write_privilege`]: embedded/anonymous callers
    /// (no thread-local identity) pass, and `LegacyRbac` lets admins
    /// through by default. Under `PolicyOnly` a principal needs an explicit
    /// `kv:write` grant on `kv:<key>`.
    pub(crate) fn check_kv_write_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        key: &str,
    ) -> RedDBResult<()> {
        let Some((username, role)) = current_auth_identity() else {
            return Ok(());
        };
        let tenant = current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("kv".to_string(), key.to_string());
        if let Some(tenant) = &tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = runtime_iam_context(role, tenant.as_deref());
        if auth_store.check_policy_authz_with_role(&principal, "kv:write", &resource, &ctx, role) {
            return Ok(());
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{}` action=`kv:write` resource=`kv:{}` denied by IAM policy",
            principal, key
        )))
    }

    /// IAM privilege check for a granular queue operation (issue #755 /
    /// PRD #735).
    ///
    /// Each queue operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] (`queue:enqueue`, `queue:read`,
    /// `queue:peek`, `queue:ack`, `queue:nack`, `queue:retry`,
    /// `queue:dlq:move`, `queue:purge`, `queue:presence:read`). The
    /// resource is `queue:<name>` scoped to the current tenant. In
    /// legacy mode (no IAM authorization configured) the check is a
    /// no-op — the role gates in `execute_queue_command` still apply
    /// and the legacy `select` / `write` grant table continues to
    /// govern queue access. In IAM-enabled mode a missing granular
    /// grant yields a structured, UI-safe error of the form
    /// `principal=… action=queue:… resource=queue:… denied by IAM
    /// policy` so Red UI can surface the failing toolbar action.
    pub(crate) fn check_queue_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        queue: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("queue".to_string(), queue.to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`queue:{}` denied by IAM policy",
                principal, action, queue
            ))
        }
    }

    /// IAM privilege check for a graph operation (issue #757 / PRD
    /// #735).
    ///
    /// Each graph operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] — `graph:read` for
    /// metadata/property lookups, `graph:traverse` for MATCH / PATH /
    /// NEIGHBORHOOD / TRAVERSE / SHORTEST_PATH, and
    /// `graph:algorithm:run` for analytics algorithms (centrality,
    /// community, components, cycles, clustering, topological sort).
    /// The resource is `graph:*` scoped to the current tenant — the
    /// runtime today operates on a singleton graph store so the name
    /// has no concrete identifier; policies grant the explorer
    /// surface by writing `graph:*` as the resource pattern.
    ///
    /// In legacy mode (no IAM authorization configured) the check is
    /// a no-op so the existing role-based defaults continue to
    /// govern. In IAM-enabled mode a missing grant produces the
    /// UI-safe envelope `principal=… action=graph:… resource=graph:*
    /// denied by IAM policy` Red UI keys on.
    pub(crate) fn check_graph_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("graph".to_string(), "*".to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`graph:*` denied by IAM policy",
                principal, action
            ))
        }
    }

    /// IAM privilege check for a granular vector operation (issue #756
    /// / PRD #735).
    ///
    /// Each vector operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] (`vector:read`, `vector:search`,
    /// `vector:artifact:read`, `vector:artifact:rebuild`,
    /// `vector:admin`). The resource is `vector:<collection>` scoped to
    /// the current tenant. In legacy mode (no IAM authorization
    /// configured) the check is a no-op — the role gates and existing
    /// `select` / column-projection grants continue to govern access.
    /// In IAM-enabled mode a missing granular grant yields a
    /// structured, UI-safe error of the form `principal=…
    /// action=vector:… resource=vector:… denied by IAM policy` so Red
    /// UI can surface the failing toolbar action.
    pub(crate) fn check_vector_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        collection: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("vector".to_string(), collection.to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`vector:{}` denied by IAM policy",
                principal, action, collection
            ))
        }
    }

    /// IAM privilege check for DROP / TRUNCATE on a named collection.
    ///
    /// Delegates to [`check_ddl_object_privilege`] with `resource_kind =
    /// "collection"`. Kept as a thin wrapper so the existing DROP/TRUNCATE
    /// callsites stay readable.
    pub(crate) fn check_ddl_collection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        username: &str,
        action: &str,
        collection: &str,
    ) -> Result<(), String> {
        self.check_ddl_object_privilege(
            auth_store,
            principal,
            role,
            tenant,
            username,
            action,
            "collection",
            collection,
            crate::auth::Role::Write,
        )
    }

    /// Generalised IAM privilege check for DDL on a named object.
    ///
    /// `action` is the stable verb advertised through the action catalog
    /// (`create`, `alter`, `drop`, `truncate`, `schema:write`,
    /// `schema:admin`). `resource_kind` / `resource_name` form the policy
    /// resource (`collection:<name>`, `schema:<name>`). `min_role` is the
    /// legacy gate when IAM is not yet enabled.
    ///
    /// Behaviour:
    /// * Role below `min_role` → structured "principal=… role=… cannot
    ///   issue DDL" denial, audit recorded.
    /// * IAM disabled → audit-record success and allow (legacy path).
    /// * IAM enabled → call `check_policy_authz_with_role`. Explicit Deny
    ///   and DefaultDeny in PolicyOnly mode both produce a UI-safe
    ///   "principal=… action=… resource=<kind>:<name> denied by IAM
    ///   policy" string. Explicit Allow and the LegacyRbac fallback
    ///   allow the action.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_ddl_object_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        username: &str,
        action: &str,
        resource_kind: &str,
        resource_name: &str,
        min_role: crate::auth::Role,
    ) -> Result<(), String> {
        if role < min_role {
            let msg = format!(
                "principal=`{}` role=`{:?}` cannot issue DDL action=`{}` resource=`{}:{}`",
                username, role, action, resource_kind, resource_name
            );
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "denied",
                crate::json::Value::Null,
            );
            return Err(msg);
        }

        if !auth_store.iam_authorization_enabled() {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "ok",
                crate::json::Value::Null,
            );
            return Ok(());
        }

        let mut resource = crate::auth::policies::ResourceRef::new(
            resource_kind.to_string(),
            resource_name.to_string(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "ok",
                crate::json::Value::Null,
            );
            Ok(())
        } else {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "denied",
                crate::json::Value::Null,
            );
            Err(format!(
                "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                username, action, resource_kind, resource_name
            ))
        }
    }
}
