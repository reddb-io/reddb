//! Query privilege gate extracted from `impl_core` (issue #1622, PRD
//! #1619): [`RedDBRuntime::check_query_privilege`] and the per-domain
//! privilege gates it dispatches to. Behaviour-preserving move; the free
//! IAM/policy-column helpers these consume live in [`super::policy_columns`].
use super::super::execution_context::{current_auth_identity, current_tenant};
use super::super::*;
use super::policy_columns::*;

impl RedDBRuntime {
    /// Project a `QueryExpr` to the (action, resource) pair the
    /// privilege engine cares about. Returns `Ok(())` for statements
    /// that don't touch user data (transaction control, SHOW, SET, etc.).
    pub(crate) fn check_query_privilege(
        &self,
        expr: &crate::storage::query::ast::QueryExpr,
    ) -> Result<(), String> {
        use crate::auth::privileges::{Action, AuthzContext, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::QueryExpr;

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
                use crate::storage::query::ast::QueueCommand;
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
                use crate::storage::query::ast::GraphCommand;
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
                use crate::storage::query::ast::SearchCommand;
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
            QueryExpr::ShowPolicies { .. } | QueryExpr::ShowEffectivePermissions { .. } => {
                return Ok(());
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
            // Everything else (SET, SHOW, transaction control, graph
            // commands, queue/tree commands, MaintenanceCommand …)
            // is allowed for any authenticated principal.
            _ => return Ok(()),
        };

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
        table: &crate::storage::query::ast::TableQuery,
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
        query: &crate::storage::query::ast::GraphQuery,
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

        if resource_kind == "policy"
            && matches!(
                action,
                "policy:put" | "policy:drop" | "policy:attach" | "policy:detach"
            )
            && self
                .inner
                .config_registry
                .get_active(resource_name)
                .map(|entry| entry.managed)
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
