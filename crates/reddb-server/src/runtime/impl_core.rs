use super::*;
use crate::auth::column_policy_gate::ColumnAccessRequest;
use crate::auth::UserId;
use crate::replication::cdc::ChangeRecord;
use crate::storage::query::ast::TableSource;
// Authorization surface moved to `super::authz` (issue #1622). Re-export the
// free IAM/policy-column helpers so this module's remaining dispatch code
// (and the `impl_core::decision_to_strings` path used by
// `server::handlers_iam_policy`) keeps calling them unqualified.
pub(crate) use super::authz::policy_columns::*;

// Graph inline-TVF/analytics moved to `super::graph_tvf` (issue #1625) and the
// RLS injection family to `super::rls_injection`. Re-export the free fns still
// referenced from this module (and via `crate::runtime::impl_core::…` paths by
// sibling files) so those call sites need no edits.
pub(crate) use super::graph_tvf::{abstract_degree_centrality, is_graph_tvf_name};
pub(crate) use super::rls_injection::{
    apply_foreign_table_filters, inject_rls_filters, inject_rls_into_join, rls_is_enabled,
    rls_policy_filter, rls_policy_filter_for_kind,
};
// VCS command parse/execute moved to `super::vcs_command` (issue #1626). The
// execute methods are inherent `pub(crate)` methods resolved via `self`; the
// free parse helpers still called from the central dispatch (and by
// `collections_referenced` here) are re-exported so those call sites need no
// edits.
pub(crate) use super::vcs_command::{
    parse_runtime_vcs_command, strip_explain_prefix, walk_collections,
};

// Ranking / metrics / SLO / analytics-source DDL moved to
// `super::impl_ranking` (issue #1627). Execute methods are inherent
// `pub(crate)` methods resolved via `self`; no call-site edits needed.

// Config / secret / KV resolution moved to `super::impl_config_secret` and the
// query-audit + control-event families to `super::impl_audit_control`
// (issue #1628). Methods are inherent `pub(crate)` methods resolved via `self`;
// the one free fn still called via a `crate::runtime::impl_core::…` path
// (`control_event_outcome_for_error`, from `impl_backup`) is re-exported so that
// call site needs no edit. The dispatch here also still calls the three
// audit-planning free fns, so they are pulled back into scope unqualified.
pub(crate) use super::impl_audit_control::{
    control_event_outcome_for_error, query_audit_plan, query_control_event_specs,
};
// Config/secret free helpers still called unqualified from the dispatch here.
pub(crate) use super::impl_config_secret::{
    insert_config_json_path, secret_sql_value_to_string, seed_storage_deploy_config,
    show_config_json_result, show_secrets_allows_key,
};
pub(super) use super::impl_core_outer_scope::{
    first_column_values, query_references_outer_scope, relation_scopes_for_query,
};
pub(crate) use super::impl_core_result_cache_scopes::collect_table_refs;
pub(super) use super::impl_core_result_cache_scopes::query_expr_result_cache_scopes;

// Bootstrap / constructor / handle accessors moved to `super::impl_lifecycle`,
// telemetry / gates / limits / shutdown to `super::impl_telemetry_accessors`,
// and catalog / stats / maintenance to `super::impl_catalog_accessors`
// (issue #1629). Methods are inherent `pub(crate)`/`pub` methods resolved via
// `self`; the two moved free fns still called unqualified from the dispatch
// here (`view_records_to_entities`) and the runtime-index rehydration paths
// (`table_row_index_fields`) are re-exported so those call sites need no edit.
pub(crate) use super::impl_lifecycle::{table_row_index_fields, view_records_to_entities};

pub use super::execution_context::{
    capture_current_snapshot, clear_current_auth_identity, clear_current_connection_id,
    clear_current_snapshot, clear_current_tenant, current_auth_identity_for_audit,
    current_connection_id, current_tenant, entity_visible_under_current_snapshot,
    entity_visible_with_context, set_current_auth_identity, set_current_connection_id,
    set_current_snapshot, set_current_tenant, snapshot_bundle, with_snapshot_bundle,
    SnapshotBundle, SnapshotContext,
};
pub(crate) use super::execution_context::{
    config_read_permitted, current_auth_identity, current_config_value, current_kv_value,
    current_role_projected, current_scope_override, current_secret_value,
    current_snapshot_requires_index_fallback, current_user_projected, has_scope_override_active,
    kv_read_permitted, parse_set_local_tenant, update_current_config_value,
    update_current_kv_value, update_current_secret_value, xids_visible_under_current_snapshot,
    ConfigSnapshotGuard, CurrentSnapshotGuard, KvStoreGuard, ScopeOverrideGuard, SecretStoreGuard,
    TxLocalTenantGuard,
};

/// Cheap prefix check for a leading `WITH` keyword. Used to gate the
/// CTE-aware parse in `execute_query` without paying for a full
/// lexer pass on every statement. Treats `WITHIN` as not-a-CTE so
/// `WITHIN TENANT '...' SELECT ...` doesn't mis-route.
pub(super) fn has_with_prefix(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("WITH")
}

/// Same as `peek_top_level_as_of` but also returns the table name
/// targeted by the AS OF clause (when the FROM clause names a
/// concrete table). `None` for the table slot means scalar SELECT
/// or a subquery source — callers treat those as "no enforcement".
pub(super) fn peek_top_level_as_of_with_table(
    sql: &str,
) -> Option<(crate::application::vcs::AsOfSpec, Option<String>)> {
    if !sql
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"as of"))
    {
        return None;
    }
    let parsed = crate::storage::query::parser::parse(sql).ok()?;
    let crate::storage::query::ast::QueryExpr::Table(table) = parsed.query else {
        return None;
    };
    let clause = table.as_of?;
    let table_name = if table.table.is_empty() || table.table == "any" {
        None
    } else {
        Some(table.table.clone())
    };
    let spec = match clause {
        crate::storage::query::ast::AsOfClause::Commit(h) => {
            crate::application::vcs::AsOfSpec::Commit(h)
        }
        crate::storage::query::ast::AsOfClause::Branch(b) => {
            crate::application::vcs::AsOfSpec::Branch(b)
        }
        crate::storage::query::ast::AsOfClause::Tag(t) => crate::application::vcs::AsOfSpec::Tag(t),
        crate::storage::query::ast::AsOfClause::TimestampMs(ts) => {
            crate::application::vcs::AsOfSpec::TimestampMs(ts)
        }
        crate::storage::query::ast::AsOfClause::Snapshot(x) => {
            crate::application::vcs::AsOfSpec::Snapshot(x)
        }
    };
    Some((spec, table_name))
}

pub(super) fn query_has_volatile_builtin(sql: &str) -> bool {
    // Lowercase the bytes up to the first null/newline into a small
    // stack buffer for cheap contains() checks. Most SQL fits in the
    // buffer; longer queries fall back to owned lowercase.
    const VOLATILE_TOKENS: &[&str] = &[
        "pg_advisory_lock",
        "pg_try_advisory_lock",
        "pg_advisory_unlock",
        "random()",
        // `$config.<path>` / `$secret.<path>` resolve mutable runtime config /
        // vault state at execution time (#1370). A cached result would serve a
        // stale value after a later `SET CONFIG` / `SET SECRET`, so treat any
        // query referencing them as volatile (never result-cache it).
        "$config",
        "$secret",
        "$kv",
        // NOW() / CURRENT_TIMESTAMP / CURRENT_DATE intentionally
        // omitted for now — they ARE volatile but today's tests rely
        // on caching them. Revisit once a tighter volatility story
        // lands.
    ];
    let lowered = sql.to_ascii_lowercase();
    VOLATILE_TOKENS.iter().any(|t| lowered.contains(t))
}

pub(super) fn query_is_ask_statement(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("ASK")
}

/// Pick the `(global_mode, collection_mode)` pair for an expression,
/// or `None` for variants that opt out of intent-locking entirely
/// (admin statements like `SHOW CONFIG`, transaction control, tenant
/// toggles).
///
/// Phase-1 contract:
/// - Reads  — `(IX-compatible) (Global, IS) → (Collection, IS)`
/// - Writes — `(IX-compatible) (Global, IX) → (Collection, IX)`
/// - DDL    — `(strong)        (Global, IX) → (Collection, X)`
pub(super) fn intent_lock_modes_for(
    expr: &QueryExpr,
) -> Option<(
    crate::runtime::lock_manager::LockMode,
    crate::runtime::lock_manager::LockMode,
)> {
    use crate::runtime::lock_manager::LockMode::{Exclusive, IntentExclusive, IntentShared};

    match expr {
        // Reads — IS / IS.
        QueryExpr::Table(_)
        | QueryExpr::Join(_)
        | QueryExpr::Vector(_)
        | QueryExpr::Hybrid(_)
        | QueryExpr::Graph(_)
        | QueryExpr::Path(_)
        | QueryExpr::Ask(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::RankOf(_)
        | QueryExpr::ApproxRankOf(_)
        | QueryExpr::RankRange(_)
        | QueryExpr::QueueSelect(_) => Some((IntentShared, IntentShared)),

        // Writes — IX / IX. Non-tabular mutations (vector insert,
        // graph node insert, queue push, timeseries point insert)
        // don't carry their own dispatch arm here; they ride through
        // the Insert variant or a command variant covered by the
        // read-side arm above. P1.T4 expands only the TableQuery-ish
        // writes; non-tabular kinds inherit when their DML variants
        // land in later phases.
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::QueueCommand(QueueCommand::Move { .. }) => {
            Some((IntentExclusive, IntentExclusive))
        }
        QueryExpr::QueueCommand(_) => Some((IntentShared, IntentShared)),

        // DDL — IX / X. A DDL against collection `c` blocks all
        // other writers + readers on `c` but leaves other collections
        // running (because Global stays IX, not X).
        QueryExpr::CreateTable(_)
        | QueryExpr::CreateCollection(_)
        | QueryExpr::CreateVector(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::DropGraph(_)
        | QueryExpr::DropVector(_)
        | QueryExpr::DropDocument(_)
        | QueryExpr::DropKv(_)
        | QueryExpr::DropCollection(_)
        | QueryExpr::Truncate(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::CreateMetric(_)
        | QueryExpr::AlterMetric(_)
        | QueryExpr::CreateSlo(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::AlterQueue(_)
        | QueryExpr::DropQueue(_)
        | QueryExpr::CreateTree(_)
        | QueryExpr::DropTree(_)
        | QueryExpr::CreatePolicy(_)
        | QueryExpr::DropPolicy(_)
        | QueryExpr::CreateView(_)
        | QueryExpr::DropView(_)
        | QueryExpr::RefreshMaterializedView(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::CreateServer(_)
        | QueryExpr::DropServer(_)
        | QueryExpr::CreateForeignTable(_)
        | QueryExpr::DropForeignTable(_) => Some((IntentExclusive, Exclusive)),

        // Admin / control — skip intent locks. `SET TENANT`,
        // `BEGIN / COMMIT / ROLLBACK`, `SET CONFIG`, `SHOW CONFIG`,
        // `VACUUM`, etc. don't touch collection data the same way
        // and the existing transaction layer already serialises the
        // pieces that matter.
        _ => None,
    }
}

/// Best-effort collection inventory for an expression. Used to pick
/// `Collection(...)` resources for the intent-lock guard. Overshoots
/// are fine (take an extra IS, benign); undershoots leak writes past
/// DDL X locks, so err on the side of listing more names.
pub(super) fn collections_referenced(expr: &QueryExpr) -> Vec<String> {
    let mut out = Vec::new();
    walk_collections(expr, &mut out);
    out.sort();
    out.dedup();
    out
}

impl RedDBRuntime {
    /// Execute a query under a typed scope override without embedding
    /// the tenant / user / role values into the SQL string. Use this
    /// from transport middleware (HTTP / gRPC / worker loops) where the
    /// scope is resolved from auth claims and the SQL is a parameterised
    /// template — avoids the string-concat injection risk of building
    /// `WITHIN TENANT '<id>' …` manually, and is drop-in compatible with
    /// prepared statements that didn't know about tenancy.
    ///
    /// Precedence matches the `WITHIN` clause: the passed `scope`
    /// overrides `SET LOCAL TENANT`, which overrides `SET TENANT`.
    /// The override is pushed on the thread-local scope stack for the
    /// duration of the call and popped on return — pool-shared
    /// connections cannot leak it across requests.
    pub fn execute_query_with_scope(
        &self,
        query: &str,
        scope: crate::runtime::within_clause::ScopeOverride,
    ) -> RedDBResult<RuntimeQueryResult> {
        if scope.is_empty() {
            return self.execute_query(query);
        }
        let _scope_guard = ScopeOverrideGuard::install(scope);
        self.execute_query(query)
    }

    /// Issue #205 — single lifecycle exit for slow-query logging.
    ///
    /// `execute_query_inner` does the real work; this wrapper times it
    /// and, if elapsed exceeds the configured threshold, hands the
    /// triple `(QueryKind, elapsed_ms, sql_redacted, scope)` to the
    /// SlowQueryLogger. The threshold + sample_pct were captured at
    /// SlowQueryLogger construction (runtime startup), so the per-call
    /// cost on below-threshold paths is one relaxed atomic load.
    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        let started = std::time::Instant::now();
        self.inner.node_load_telemetry.query_start();
        let result = self.execute_query_inner(query);
        self.finish_query_lifecycle(query, started, result)
    }

    /// Execute a SQL statement with already-decoded positional bind
    /// parameters. Transports should call this instead of parsing +
    /// binding on their side and then reaching for `execute_query_expr`:
    /// this entry keeps parameterized statements inside the same
    /// statement lifecycle as textual SQL (snapshot guard, config/secret
    /// guards, coarse auth, intent locks, slow-query logging, integrity
    /// tombstone filtering, and causal bookmarks).
    pub fn execute_query_with_params(
        &self,
        query: &str,
        params: &[Value],
    ) -> RedDBResult<RuntimeQueryResult> {
        if params.is_empty() {
            return self.execute_query(query);
        }
        let started = std::time::Instant::now();
        self.inner.node_load_telemetry.query_start();
        let result = self.execute_query_with_params_inner(query, params);
        self.finish_query_lifecycle(query, started, result)
    }

    fn finish_query_lifecycle(
        &self,
        query: &str,
        started: std::time::Instant,
        mut result: RedDBResult<RuntimeQueryResult>,
    ) -> RedDBResult<RuntimeQueryResult> {
        // Issue #765 / S6 — filter integrity-tombstoned rows out of SELECT
        // results before they reach any consumer. Fast no-op (one relaxed
        // atomic load) unless an input-stream digest mismatch has tombstoned
        // a RID range on this store.
        if let Ok(ref mut query_result) = result {
            if query_result.statement_type == "select" {
                self.filter_integrity_tombstoned(&mut query_result.result);
            }
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Build EffectiveScope from the same thread-locals frame-build
        // consults — keeps the slow-log row consistent with the audit /
        // RLS view of "this statement". `ai_scope()` is the canonical
        // builder.
        let scope = self.ai_scope();
        let kind = match result
            .as_ref()
            .map(|r| r.statement_type)
            .unwrap_or("select")
        {
            "select" => crate::telemetry::slow_query_logger::QueryKind::Select,
            "insert" => crate::telemetry::slow_query_logger::QueryKind::Insert,
            "update" => crate::telemetry::slow_query_logger::QueryKind::Update,
            "delete" => crate::telemetry::slow_query_logger::QueryKind::Delete,
            _ => crate::telemetry::slow_query_logger::QueryKind::Internal,
        };
        // SQL redaction: pass the raw query through. The slow-query
        // logger writes structured JSON so embedded literals stay
        // escape-safe at the JSON boundary (proven by
        // `adversarial_sql_is_escape_safe` in slow_query_logger.rs).
        // PII redaction (e.g. literal masking) is a follow-up.
        self.inner
            .slow_query_logger
            .record(kind, elapsed_ms, query.to_string(), &scope);

        // Issue #1241 — record latency into the bounded per-`kind`
        // histogram substrate (always, not only above the slow-query
        // threshold). `started.elapsed()` is re-read here for sub-ms
        // resolution; the cost is one `Instant::now` plus a handful of
        // relaxed atomic adds (see `query_latency_telemetry` docs).
        self.inner
            .query_latency_telemetry
            .observe(kind, started.elapsed().as_secs_f64());

        // Issue #1245 — decrement the active-query gauge. One relaxed
        // atomic sub; the matching increment happened at execute_query /
        // execute_query_with_params entry.
        self.inner.node_load_telemetry.query_finish();

        if let Ok(ref mut query_result) = result {
            if matches!(query_result.statement_type, "insert" | "update" | "delete") {
                let bookmark = crate::replication::CausalBookmark::new(
                    self.current_replication_term(),
                    self.cdc_current_lsn(),
                );
                query_result.bookmark = Some(bookmark.encode());
            }
        }

        result
    }

    fn execute_query_with_params_inner(
        &self,
        query: &str,
        params: &[Value],
    ) -> RedDBResult<RuntimeQueryResult> {
        let parsed = parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        let bound = crate::storage::query::user_params::bind(&parsed, params).map_err(|err| {
            RedDBError::Validation {
                message: err.to_string(),
                validation: crate::json!({
                    "code": "INVALID_PARAMS",
                    "surface": "query.params",
                }),
            }
        })?;
        self.execute_bound_query_expr_in_frame(query, bound)
    }

    fn execute_bound_query_expr_in_frame(
        &self,
        query: &str,
        expr: QueryExpr,
    ) -> RedDBResult<RuntimeQueryResult> {
        let rewritten_query = super::red_schema::rewrite_virtual_names(query);
        let execution_query = rewritten_query.as_deref().unwrap_or(query);
        let frame = super::statement_frame::StatementExecutionFrame::build(self, execution_query)?;
        // Box the guards so the ~640-byte StatementFrameGuards struct lives on
        // the heap rather than the call stack — important for recursive paths
        // (view refresh, nested queries) where the stack can be as small as 2 MB.
        let _frame_guards = Box::new(frame.install(self));
        let _log_span = crate::telemetry::span::query_span(query).entered();

        let expr = self.rewrite_view_refs(expr);
        let mode = detect_mode(execution_query);
        let control_event_specs = query_control_event_specs(&expr);
        let _lock_guard = match frame.prepare_dispatch(self, &expr) {
            Ok(guard) => guard,
            Err(err) => {
                let outcome = control_event_outcome_for_error(&err);
                for spec in &control_event_specs {
                    self.emit_control_event(
                        spec.kind,
                        outcome,
                        spec.action,
                        spec.resource.clone(),
                        Some(err.to_string()),
                        spec.fields.clone(),
                    )?;
                }
                return Err(err);
            }
        };

        let mut result = self.dispatch_expr(expr, query, mode)?;
        if result.statement_type == "select" {
            self.apply_secret_decryption(&mut result);
        }
        Ok(result)
    }

    pub fn causal_session(&self) -> crate::runtime::CausalSession {
        crate::runtime::CausalSession {
            runtime: self.clone(),
            bookmark: None,
            wait_timeout: std::time::Duration::from_secs(5),
        }
    }

    pub fn wait_for_bookmark(
        &self,
        bookmark: &crate::replication::CausalBookmark,
        timeout: std::time::Duration,
    ) -> RedDBResult<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let applied_lsn = self.local_contiguous_applied_lsn();
            if applied_lsn >= bookmark.commit_lsn() {
                return Ok(());
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(RedDBError::InvalidOperation(format!(
                    "timed out waiting for causal bookmark lsn {}; applied={}",
                    bookmark.commit_lsn(),
                    applied_lsn
                )));
            }
            let remaining = deadline.saturating_duration_since(now);
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
        }
    }

    fn local_contiguous_applied_lsn(&self) -> u64 {
        match self.inner.db.options().replication.role {
            crate::replication::ReplicationRole::Replica { .. } => {
                self.config_u64("red.replication.last_applied_lsn", 0)
            }
            _ => self.cdc_current_lsn(),
        }
    }

    #[inline(never)]
    pub(super) fn execute_query_inner(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        // ── ULTRA-TURBO: autocommit `SELECT * FROM t WHERE _entity_id = N` ──
        //
        // Moved above every boot-cost the normal path pays (WITHIN
        // strip, SET LOCAL parse, tx_local_tenants read, snapshot
        // guard, tracing span, tx_contexts read) because the bench's
        // `select_point` scenario was observed at 28× vs PostgreSQL —
        // the dominant cost wasn't the entity fetch but the ceremony
        // before it. Only fires when there's no ambient transaction
        // context or WITHIN override, so the snapshot install we skip
        // truly is a no-op for this query.
        if !has_scope_override_active()
            && !query.trim_start().starts_with("WITHIN")
            && !query.trim_start().starts_with("within")
            && !self.inner.query_audit.has_rules()
            && !self
                .inner
                .tx_contexts
                .read()
                .contains_key(&current_connection_id())
        {
            if let Some(result) = self.try_fast_entity_lookup(query) {
                return result;
            }
        }

        // `WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>` —
        // strip the prefix, push a stack-scoped override, recurse on
        // the inner statement, pop on return. Stack lives in a
        // thread-local but is balanced by the RAII guard, so a
        // pool-shared connection cannot leak the override across
        // requests and an early `?` return still pops cleanly.
        match crate::runtime::within_clause::try_strip_within_prefix(query) {
            Ok(Some((scope, inner))) => {
                let _scope_guard = ScopeOverrideGuard::install(scope);
                // Re-enter the inner path, NOT `execute_query`, so the
                // slow-query lifecycle hook records exactly one row per
                // top-level statement (the WITHIN-stripped form would
                // double-record).
                return self.execute_query_inner(inner);
            }
            Ok(None) => {}
            Err(msg) => return Err(RedDBError::Query(msg)),
        }

        // `EXPLAIN ANALYZE <dml>` — paid truth tier. Executes the
        // mutating statement inside a transaction that is always
        // rolled back, then reports the actual affected row count.
        if let Some(inner) = strip_explain_analyze_prefix(query) {
            return self.explain_analyze_as_rows(query, inner);
        }

        // `EXPLAIN <stmt>` — introspection. Runs the planner on the
        // inner statement (WITHOUT executing it) and returns the
        // CanonicalLogicalNode tree as rows so the caller can see the
        // operator shape and estimated cost. `EXPLAIN ALTER FOR ...`
        // is a distinct schema-diff command and continues down the
        // regular SQL path.
        if let Some(inner) = strip_explain_prefix(query) {
            return self.explain_as_rows(query, inner);
        }

        // `SET LOCAL TENANT '<id>'` — write the per-transaction tenant
        // override and return. Outside a transaction the statement is
        // an error (matches PG semantics: SET LOCAL only takes effect
        // within an active transaction).
        if let Some(value) = parse_set_local_tenant(query)? {
            let conn_id = current_connection_id();
            if !self.inner.tx_contexts.read().contains_key(&conn_id) {
                return Err(RedDBError::Query(
                    "SET LOCAL TENANT requires an active transaction".to_string(),
                ));
            }
            self.inner
                .tx_local_tenants
                .write()
                .insert(conn_id, value.clone());
            return Ok(RuntimeQueryResult::ok_message(
                query.to_string(),
                &match &value {
                    Some(id) => format!("local tenant set: {id}"),
                    None => "local tenant cleared".to_string(),
                },
                "set_local_tenant",
            ));
        }

        if super::red_schema::is_system_schema_write(query) {
            return Err(RedDBError::Query(
                super::red_schema::READ_ONLY_ERROR.to_string(),
            ));
        }

        if let Some(command) = parse_runtime_vcs_command(query) {
            return self.execute_vcs_command(query, detect_mode(query), command?);
        }

        if let Some(create_source) = super::analytics_source_catalog::parse_create_statement(query)?
        {
            return self.execute_create_analytics_source(query, create_source);
        }

        // Issue #790 — `READ METRIC <path>` is intentionally rejected at
        // v0. The descriptor itself is readable through
        // `red.analytics.metrics`; the *output* read returns a
        // structured error so callers can tell "execution engine not yet
        // built" apart from "metric does not exist".
        if let Some(path) = super::metric_descriptor_catalog::parse_read_metric_statement(query) {
            return Err(super::metric_descriptor_catalog::read_output_unsupported(
                &path,
            ));
        }

        // Issue #918 / ADR 0035 — leaderboard rank capability catalog
        // declarations are still recognised before the general parser.
        // Rank reads themselves are parser AST nodes, including Redis-flavor
        // Z* sugar that desugars to the same canonical rank shapes.
        if let Some(parsed) = super::ranking_descriptor_catalog::parse_create_ranking(query) {
            return self.execute_create_ranking(query, parsed?);
        }
        if super::ranking_descriptor_catalog::parse_show_rankings(query) {
            return self.execute_show_rankings(query);
        }

        let rewritten_query = super::red_schema::rewrite_virtual_names(query);
        let execution_query = rewritten_query.as_deref().unwrap_or(query);

        let frame = super::statement_frame::StatementExecutionFrame::build(self, execution_query)?;
        let _frame_guards = Box::new(frame.install(self));

        // Phase 6 logging: enter a span stamped with conn_id / tenant
        // / query_len. Every downstream tracing::info!/warn!/error!
        // inherits these fields — no need to thread them manually
        // through storage/scan layers. Entered AFTER the WITHIN /
        // SET LOCAL TENANT resolution above so the span reflects the
        // effective scope for this statement.
        let _log_span = crate::telemetry::span::query_span(query).entered();

        // ── CTE prelude (#41) — `WITH x AS (...) SELECT ... FROM x` ──
        if let Some(rewritten) = frame.prepare_cte(execution_query)? {
            return self.execute_query_expr(rewritten);
        }

        // ── TURBO: bypass SQL parse for SELECT * FROM x WHERE _entity_id = N ──
        if !self.inner.query_audit.has_rules() {
            if let Some(result) = self.try_fast_entity_lookup(execution_query) {
                return result;
            }
        }

        // ── Result cache: return cached result if still fresh (30s TTL) ──
        if !self.inner.query_audit.has_rules() {
            if let Some(result) = frame.read_result_cache(self) {
                return Ok(result);
            }
        }

        let prepared = frame.prepare_statement(self, execution_query)?;
        let mode = prepared.mode;
        let expr = prepared.expr;

        let statement = query_expr_name(&expr);
        let result_cache_scopes = query_expr_result_cache_scopes(&expr);
        let control_event_specs = query_control_event_specs(&expr);
        let query_audit_plan = query_audit_plan(&expr);

        let _lock_guard = match frame.prepare_dispatch(self, &expr) {
            Ok(guard) => guard,
            Err(err) => {
                let outcome = control_event_outcome_for_error(&err);
                for spec in &control_event_specs {
                    self.emit_control_event(
                        spec.kind,
                        outcome,
                        spec.action,
                        spec.resource.clone(),
                        Some(err.to_string()),
                        spec.fields.clone(),
                    )?;
                }
                return Err(err);
            }
        };
        let frame_iface: &dyn super::statement_frame::ReadFrame = &frame;
        let query_audit_started = std::time::Instant::now();

        let query_result = match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Apply MVCC visibility + RLS gate while materialising the
                // graph: every node entity is screened against the source
                // collection's policy chain (basic and `Nodes`-targeted)
                // and dropped when the caller's tenant / role doesn't
                // admit it. Edges are pruned automatically because the
                // graph builder skips edges whose endpoints aren't in
                // `allowed_nodes`.
                let (graph, node_properties, edge_properties) =
                    self.materialize_graph_with_rls()?;
                let result =
                    crate::storage::query::unified::UnifiedExecutor::execute_on_with_graph_properties(
                        &graph,
                        &expr,
                        node_properties,
                        edge_properties,
                    )
                        .map_err(|err| RedDBError::Query(err.to_string()))?;

                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "materialized-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            QueryExpr::Table(table) => {
                let table = self.resolve_table_expr_subqueries(
                    table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )?;
                // Table-valued functions (e.g. components(g)) dispatch to a
                // read-only executor before any catalog/virtual-table routing
                // (issue #795).
                if let Some(TableSource::Function {
                    name,
                    args,
                    named_args,
                }) = table.source.clone()
                {
                    // The graph-collection form is cacheable (issue #802): the
                    // result-cache read at the top of this function keys on the
                    // query string, and `result_cache_scopes` carries the graph
                    // collection (see `collect_table_source_scopes`) so a write
                    // to it invalidates the entry. Deterministic algorithm
                    // output is worth caching at any row count, so the write
                    // bypasses the generic ≤5-row payload heuristic.
                    let tvf_result = RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf",
                        result: self.execute_table_function(&name, &args, &named_args)?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    };
                    frame.write_result_cache(self, &tvf_result, result_cache_scopes.clone());
                    return Ok(tvf_result);
                }
                // Inline-graph TVF (issue #799): the graph is supplied by two
                // subqueries instead of a collection reference. Unlike the
                // graph-collection form, the result IS cacheable — its cache
                // key is the query string (the result-cache read at the top of
                // `execute_query_inner` keys on it) and `result_cache_scopes`
                // already carries the `nodes`/`edges` source collections, so a
                // write to any of them invalidates the entry.
                if let Some(TableSource::InlineGraphFunction {
                    name,
                    nodes,
                    edges,
                    named_args,
                }) = table.source.clone()
                {
                    let inline_result = RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf-inline",
                        result: self.execute_inline_graph_function(
                            &name,
                            &nodes,
                            &edges,
                            &named_args,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    };
                    frame.write_result_cache(self, &inline_result, result_cache_scopes);
                    return Ok(inline_result);
                }
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &frame as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }

                // `<graph>.<output>` analytics virtual view (issue #800).
                // Recomputed on demand — intentionally not result-cached, so it
                // always reflects the current graph data.
                if let Some(view_result) = self.try_resolve_analytics_view(
                    &table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )? {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-analytics-view",
                        result: view_result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }

                if let Some(result) = self.execute_probabilistic_select(&table)? {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-probabilistic",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }

                // Foreign-table intercept (Phase 3.2.2 PG parity).
                //
                // When the referenced table matches a `CREATE FOREIGN TABLE`
                // registration, short-circuit into the FDW scan. Phase 3.2
                // wrappers don't yet support pushdown, so filters/projections
                // apply post-scan via `apply_foreign_table_filters` — good
                // enough for correctness; perf work lands in 3.2.3.
                if self.inner.foreign_tables.is_foreign_table(&table.table) {
                    let records = self
                        .inner
                        .foreign_tables
                        .scan(&table.table)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?;
                    let result = apply_foreign_table_filters(records, &table);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-fdw",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }

                // Row-Level Security enforcement (Phase 2.5.2 PG parity).
                //
                // When RLS is enabled on this table, fetch every policy
                // that applies to the current (role, SELECT) pair and
                // fold them into the query's WHERE clause: policies
                // OR-combine (any of them admitting the row is enough),
                // then AND into the caller's existing filter.
                //
                // Anonymous callers (no thread-local identity) pass
                // `role = None`; policies with a specific `TO role`
                // clause skip, but `TO PUBLIC` policies still apply.
                //
                // When `inject_rls_filters` returns `None` the table has
                // RLS enabled but no policy admits the caller's role —
                // short-circuit with an empty result set instead of
                // synthesising a contradiction filter.
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    let empty = crate::storage::query::unified::UnifiedResult::empty();
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-table-rls",
                        result: empty,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-table",
                    // #885: lend the frame-owned row-buffer arena to the
                    // streaming path so chunk buffers are reused across
                    // this statement's chunk-fetches instead of allocated
                    // fresh per chunk. This is the table-query dispatch
                    // that runs under a `StatementExecutionFrame`; the
                    // frameless prepared/subquery paths keep `None`.
                    result: execute_runtime_table_query_in(
                        &self.inner.db,
                        &table_with_rls,
                        Some(&self.inner.index_store),
                        Some(frame.row_arena()),
                    )?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            QueryExpr::Join(join) => {
                // Fold per-table RLS filters into each `QueryExpr::Table`
                // leaf of the join tree before executing. Without this
                // the join executor scans both tables raw and ignores
                // policies — a `WITHIN TENANT 'x'` against a join of
                // two tenant-scoped tables would leak cross-tenant rows.
                // When any leaf has RLS enabled and zero matching policy,
                // short-circuit to an empty join result instead of
                // emitting a contradiction filter.
                let join_with_rls = match self.authorize_relational_join_select(
                    join,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )? {
                    Some(j) => j,
                    None => {
                        return Ok(RuntimeQueryResult {
                            query: query.to_string(),
                            mode,
                            statement,
                            engine: "runtime-join-rls",
                            result: crate::storage::query::unified::UnifiedResult::empty(),
                            affected_rows: 0,
                            statement_type: "select",
                            bookmark: None,
                            notice: None,
                        });
                    }
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
                notice: None,
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
                notice: None,
            }),
            QueryExpr::RankOf(ref rank) => self.execute_rank_of(query, rank),
            QueryExpr::ApproxRankOf(ref rank) => self.execute_approx_rank_of(query, rank),
            QueryExpr::RankRange(ref range) => self.execute_rank_range(query, range),
            // DML execution
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => self
                .with_deferred_store_wal_for_dml(self.insert_may_emit_events(insert), || {
                    self.execute_insert(query, insert)
                }),
            QueryExpr::Update(ref update) => self
                .with_deferred_store_wal_for_dml(self.update_may_emit_events(update), || {
                    self.execute_update(query, update)
                }),
            QueryExpr::Delete(ref delete) => self
                .with_deferred_store_wal_for_dml(self.delete_may_emit_events(delete), || {
                    self.execute_delete(query, delete)
                }),
            // DDL execution
            QueryExpr::CreateTable(ref create) => self.execute_create_table(query, create),
            QueryExpr::CreateCollection(ref create) => {
                self.execute_create_collection(query, create)
            }
            QueryExpr::CreateVector(ref create) => self.execute_create_vector(query, create),
            QueryExpr::DropTable(ref drop_tbl) => self.execute_drop_table(query, drop_tbl),
            QueryExpr::DropGraph(ref drop_graph) => self.execute_drop_graph(query, drop_graph),
            QueryExpr::DropVector(ref drop_vector) => self.execute_drop_vector(query, drop_vector),
            QueryExpr::DropDocument(ref drop_document) => {
                self.execute_drop_document(query, drop_document)
            }
            QueryExpr::DropKv(ref drop_kv) => self.execute_drop_kv(query, drop_kv),
            QueryExpr::DropCollection(ref drop_collection) => {
                self.execute_drop_collection(query, drop_collection)
            }
            QueryExpr::Truncate(ref truncate) => self.execute_truncate(query, truncate),
            QueryExpr::AlterTable(ref alter) => self.execute_alter_table(query, alter),
            QueryExpr::CreateVcsRef(ref create) => self.execute_create_vcs_ref(query, create),
            QueryExpr::DropVcsRef(ref drop_ref) => self.execute_drop_vcs_ref(query, drop_ref),
            QueryExpr::ForkStore(ref fork) => self.execute_fork_store(query, fork),
            QueryExpr::PromoteFork(ref promote_fork) => {
                self.execute_promote_fork(query, promote_fork)
            }
            QueryExpr::DropFork(ref drop_fork) => self.execute_drop_fork(query, drop_fork),
            QueryExpr::ExplainAlter(ref explain) => self.execute_explain_alter(query, explain),
            // Graph analytics commands
            QueryExpr::GraphCommand(ref cmd) => self.execute_graph_command(query, cmd),
            // Search commands
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query, cmd),
            // ASK: RAG query with LLM synthesis
            QueryExpr::Ask(ref ask) => self.execute_ask(query, ask),
            QueryExpr::CreateIndex(ref create_idx) => self.execute_create_index(query, create_idx),
            QueryExpr::DropIndex(ref drop_idx) => self.execute_drop_index(query, drop_idx),
            QueryExpr::ProbabilisticCommand(ref cmd) => {
                self.execute_probabilistic_command(query, cmd)
            }
            // Time-series DDL
            QueryExpr::CreateTimeSeries(ref ts) => self.execute_create_timeseries(query, ts),
            QueryExpr::CreateMetric(ref metric) => self.execute_create_metric(query, metric),
            QueryExpr::AlterMetric(ref alter) => self.execute_alter_metric(query, alter),
            QueryExpr::CreateSlo(ref slo) => self.execute_create_slo(query, slo),
            QueryExpr::DropTimeSeries(ref ts) => self.execute_drop_timeseries(query, ts),
            // Queue DDL and commands
            QueryExpr::CreateQueue(ref q) => self.execute_create_queue(query, q),
            QueryExpr::AlterQueue(ref q) => self.execute_alter_queue(query, q),
            QueryExpr::DropQueue(ref q) => self.execute_drop_queue(query, q),
            QueryExpr::QueueSelect(ref q) => self.execute_queue_select(query, q),
            QueryExpr::QueueCommand(ref cmd) => self
                .with_deferred_store_wal_if_transaction(|| self.execute_queue_command(query, cmd)),
            QueryExpr::EventsBackfill(ref backfill) => {
                self.execute_events_backfill(query, backfill)
            }
            QueryExpr::EventsBackfillStatus { ref collection } => Err(RedDBError::Query(format!(
                "EVENTS BACKFILL STATUS for '{collection}' is not implemented in this slice"
            ))),
            QueryExpr::KvCommand(ref cmd) => self.execute_kv_command(query, cmd),
            QueryExpr::ConfigCommand(ref cmd) => self.execute_config_command(query, cmd),
            QueryExpr::CreateTree(ref tree) => self.execute_create_tree(query, tree),
            QueryExpr::DropTree(ref tree) => self.execute_drop_tree(query, tree),
            QueryExpr::TreeCommand(ref cmd) => self.execute_tree_command(query, cmd),
            // SET CONFIG key = value
            QueryExpr::SetConfig { ref key, ref value } => {
                if key.starts_with("red.secret.") {
                    return Err(RedDBError::Query(
                        "red.secret.* is reserved for vault secrets; use SET SECRET".to_string(),
                    ));
                }
                if key.starts_with("red.secrets.") {
                    return Err(RedDBError::Query(
                        "red.secrets.* is reserved for vault secrets; use SET SECRET".to_string(),
                    ));
                }
                // ADR-0068 §5 clean break: reject the removed AI config keys
                // (old `default.*` provider/model and per-alias base_url shape)
                // with a didactic error naming the replacement key.
                crate::ai::validate_ai_config_key_on_write(key)?;
                match self.check_managed_config_write_for_set_config(key) {
                    Err(err) => Err(err),
                    Ok(()) => {
                        let store = self.inner.db.store();
                        let json_val = match value {
                            Value::Text(s) => crate::serde_json::Value::String(s.to_string()),
                            Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
                            Value::Float(n) => crate::serde_json::Value::Number(*n),
                            Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
                            _ => crate::serde_json::Value::String(value.to_string()),
                        };
                        store.set_config_tree(key, &json_val);
                        update_current_config_value(key, value.clone());
                        // Config changes can flip runtime behavior mid-session
                        // (auto_decrypt, auto_encrypt, etc.) — invalidate the
                        // result cache so subsequent reads re-execute against
                        // the new config.
                        self.invalidate_result_cache();
                        Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("config set: {key}"),
                            "set",
                        ))
                    }
                }
            }
            // SET SECRET key = value
            QueryExpr::SetSecret { ref key, ref value } => {
                if key.starts_with("red.config.") {
                    return Err(RedDBError::Query(
                        "red.config.* is reserved for config; use SET CONFIG".to_string(),
                    ));
                }
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SET SECRET requires an enabled, unsealed vault".to_string())
                })?;
                self.check_secret_write_privilege(&auth_store, key)?;
                if matches!(value, Value::Null) {
                    auth_store
                        .vault_kv_try_delete(key)
                        .map_err(|err| RedDBError::Query(err.to_string()))?;
                    update_current_secret_value(key, None);
                    self.invalidate_result_cache();
                    return Ok(RuntimeQueryResult::ok_message(
                        query.to_string(),
                        &format!("secret deleted: {key}"),
                        "delete_secret",
                    ));
                }
                let value = secret_sql_value_to_string(value)?;
                auth_store
                    .vault_kv_try_set(key.clone(), value.clone())
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                update_current_secret_value(key, Some(value));
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret set: {key}"),
                    "set_secret",
                ))
            }
            // DELETE SECRET key
            QueryExpr::DeleteSecret { ref key } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query(
                        "DELETE SECRET requires an enabled, unsealed vault".to_string(),
                    )
                })?;
                self.check_secret_write_privilege(&auth_store, key)?;
                let deleted = auth_store
                    .vault_kv_try_delete(key)
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                if deleted {
                    update_current_secret_value(key, None);
                }
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret deleted: {key}"),
                    if deleted {
                        "delete_secret"
                    } else {
                        "delete_secret_not_found"
                    },
                ))
            }
            // SET KV key = value — plain (non-encrypted) user KV entry (#1602)
            QueryExpr::SetKv { ref key, ref value } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SET KV requires an auth store".to_string())
                })?;
                self.check_kv_write_privilege(&auth_store, key)?;
                // `SET KV k = NULL` deletes, mirroring `SET SECRET`.
                if matches!(value, Value::Null) {
                    auth_store.plain_kv_delete(key);
                    update_current_kv_value(key, None);
                    self.invalidate_result_cache();
                    return Ok(RuntimeQueryResult::ok_message(
                        query.to_string(),
                        &format!("kv deleted: {key}"),
                        "delete_kv",
                    ));
                }
                let value = secret_sql_value_to_string(value)?;
                auth_store.plain_kv_set(key.clone(), value.clone());
                update_current_kv_value(key, Some(value));
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("kv set: {key}"),
                    "set_kv",
                ))
            }
            // DELETE KV key
            QueryExpr::DeleteKv { ref key } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("DELETE KV requires an auth store".to_string())
                })?;
                self.check_kv_write_privilege(&auth_store, key)?;
                let deleted = auth_store.plain_kv_delete(key);
                if deleted {
                    update_current_kv_value(key, None);
                }
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("kv deleted: {key}"),
                    if deleted {
                        "delete_kv"
                    } else {
                        "delete_kv_not_found"
                    },
                ))
            }
            QueryExpr::Scrub { background, budget } => {
                self.execute_scrub_query(query, mode, background, budget)
            }
            // SHOW SECRET[S] [prefix]
            QueryExpr::ShowSecrets { ref prefix } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SHOW SECRET requires an enabled, unsealed vault".to_string())
                })?;
                if !auth_store.is_vault_backed() {
                    return Err(RedDBError::Query(
                        "SHOW SECRET requires an enabled, unsealed vault".to_string(),
                    ));
                }
                let mut keys = auth_store.vault_kv_keys();
                keys.sort();
                let mut result = UnifiedResult::with_columns(vec![
                    "key".into(),
                    "value".into(),
                    "status".into(),
                ]);
                for key in keys {
                    if !show_secrets_allows_key(&key) {
                        continue;
                    }
                    if let Some(ref pfx) = prefix {
                        if !key.starts_with(pfx) {
                            continue;
                        }
                    }
                    let mut record = UnifiedRecord::new();
                    record.set("key", Value::text(key));
                    record.set("value", Value::text("***"));
                    record.set("status", Value::text("active"));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_secrets",
                    engine: "runtime-secret",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            // SHOW CONFIG [prefix] [AS JSON|FORMAT JSON]
            QueryExpr::ShowConfig {
                ref prefix,
                as_json,
            } => {
                let store = self.inner.db.store();
                let all_collections = store.list_collections();
                if !all_collections.contains(&"red_config".to_string()) {
                    if as_json {
                        return Ok(show_config_json_result(
                            query,
                            mode,
                            prefix,
                            crate::serde_json::Value::Object(crate::serde_json::Map::new()),
                        ));
                    }
                    let result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement: "show_config",
                        engine: "runtime-config",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }
                let manager = store
                    .get_collection("red_config")
                    .ok_or_else(|| RedDBError::NotFound("red_config".to_string()))?;
                let entities = manager.query_all(|_| true);
                let mut latest = std::collections::BTreeMap::<String, (u64, Value, Value)>::new();
                for entity in entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let key_val = named.get("key").cloned().unwrap_or(Value::Null);
                            let val = named.get("value").cloned().unwrap_or(Value::Null);
                            let key_str = match &key_val {
                                Value::Text(s) => s.as_ref(),
                                _ => continue,
                            };
                            if let Some(ref pfx) = prefix {
                                if !key_str.starts_with(pfx.as_str()) {
                                    continue;
                                }
                            }
                            let entity_id = entity.id.raw();
                            match latest.get(key_str) {
                                Some((prev_id, _, _)) if *prev_id > entity_id => {}
                                _ => {
                                    latest.insert(key_str.to_string(), (entity_id, key_val, val));
                                }
                            }
                        }
                    }
                }
                if as_json {
                    let mut tree = crate::serde_json::Value::Object(crate::serde_json::Map::new());
                    for (key, (_, _, val)) in latest {
                        let relative = match prefix {
                            Some(pfx) if key == *pfx => "",
                            Some(pfx) => key
                                .strip_prefix(pfx.as_str())
                                .and_then(|tail| tail.strip_prefix('.'))
                                .unwrap_or(key.as_str()),
                            None => key.as_str(),
                        };
                        insert_config_json_path(
                            &mut tree,
                            relative,
                            crate::presentation::entity_json::storage_value_to_json(&val),
                        );
                    }
                    return Ok(show_config_json_result(query, mode, prefix, tree));
                }
                let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                for (_, key_val, val) in latest.into_values() {
                    let mut record = UnifiedRecord::new();
                    record.set("key", key_val);
                    record.set("value", val);
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_config",
                    engine: "runtime-config",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            // Session-local multi-tenancy handle (Phase 2.5.3).
            //
            // SET TENANT 'id' / SET TENANT NULL / RESET TENANT — writes
            // the thread-local; SHOW TENANT returns it. Paired with the
            // CURRENT_TENANT() scalar for use in RLS policies.
            QueryExpr::SetTenant(ref value) => {
                match value {
                    Some(id) => set_current_tenant(id.clone()),
                    None => clear_current_tenant(),
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &match value {
                        Some(id) => format!("tenant set: {id}"),
                        None => "tenant cleared".to_string(),
                    },
                    "set_tenant",
                ))
            }
            QueryExpr::ShowTenant => {
                let mut result = UnifiedResult::with_columns(vec!["tenant".into()]);
                let mut record = UnifiedRecord::new();
                record.set(
                    "tenant",
                    current_tenant().map(Value::text).unwrap_or(Value::Null),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_tenant",
                    engine: "runtime-tenant",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            // Transaction control (Phase 2.3 PG parity).
            //
            // BEGIN allocates a real `Xid` and stores a `TxnContext` keyed by
            // the current connection's id. COMMIT/ROLLBACK release it through
            // the `SnapshotManager` so future snapshots see the correct set of
            // active/aborted transactions.
            //
            // Tuple stamping (xmin/xmax) and read-path visibility filtering
            // land in Phase 2.3.2 — this dispatch only manages the snapshot
            // registry. Statements running outside a TxnContext still behave
            // as autocommit (xid=0 → visible to every snapshot).
            QueryExpr::TransactionControl(ref ctl) => {
                use crate::storage::query::ast::TxnControl;
                use crate::storage::transaction::snapshot::{TxnContext, Xid};
                use crate::storage::transaction::IsolationLevel;

                // Phase 2.3 keys transactions by a thread-local connection id.
                // The stdio/gRPC paths wire a real per-connection id later;
                // for embedded use (one RedDBRuntime per process-ish caller)
                // we fall back to a deterministic placeholder.
                let conn_id = current_connection_id();

                let (kind, msg) = match ctl {
                    TxnControl::Begin(requested_isolation) => {
                        let isolation = requested_isolation
                            .map(IsolationLevel::from)
                            .unwrap_or(IsolationLevel::SnapshotIsolation);
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let xid = mgr.begin();
                        if isolation == IsolationLevel::Serializable {
                            mgr.begin_serializable(xid);
                        }
                        let snapshot = mgr.snapshot(xid);
                        let ctx = TxnContext {
                            xid,
                            isolation,
                            snapshot,
                            savepoints: Vec::new(),
                            released_sub_xids: Vec::new(),
                        };
                        self.inner.tx_contexts.write().insert(conn_id, ctx);
                        ("begin", format!("BEGIN — xid={xid} (snapshot isolation)"))
                    }
                    TxnControl::Commit => {
                        // SET LOCAL TENANT ends with the transaction.
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                let mut own_xids = std::collections::HashSet::new();
                                own_xids.insert(ctx.xid);
                                for (_, sub) in &ctx.savepoints {
                                    own_xids.insert(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    own_xids.insert(*sub);
                                }
                                if let Err(err) = self.check_table_row_write_conflicts(
                                    conn_id,
                                    &ctx.snapshot,
                                    &own_xids,
                                    ctx.isolation,
                                ) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    self.discard_pending_queue_wakes(conn_id);
                                    self.discard_pending_store_wal_actions(conn_id);
                                    self.release_pending_claim_locks(conn_id);
                                    return Err(err);
                                }
                                if let Err(err) = self.check_queue_dedup_write_conflicts(
                                    conn_id,
                                    &ctx.snapshot,
                                    &own_xids,
                                ) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_queue_dedup(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    self.discard_pending_queue_wakes(conn_id);
                                    self.discard_pending_store_wal_actions(conn_id);
                                    self.release_pending_claim_locks(conn_id);
                                    return Err(err);
                                }
                                self.restore_pending_write_stamps(conn_id);
                                if let Err(err) = self.flush_pending_store_wal_actions(conn_id) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_queue_dedup(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    self.discard_pending_queue_wakes(conn_id);
                                    self.release_pending_claim_locks(conn_id);
                                    return Err(err);
                                }
                                // Phase 2.3.2e: commit every open sub-xid
                                // so they also become visible. Their
                                // work is promoted to the parent txn's
                                // result exactly like a RELEASE would
                                // have done.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                self.inner.snapshot_manager.commit(ctx.xid);
                                self.finalize_pending_versioned_updates(conn_id);
                                self.finalize_pending_tombstones(conn_id);
                                self.finalize_pending_queue_dedup(conn_id);
                                self.finalize_pending_kv_watch_events(conn_id);
                                self.finalize_pending_queue_wakes(conn_id);
                                self.release_pending_claim_locks(conn_id);
                                ("commit", format!("COMMIT — xid={} committed", ctx.xid))
                            }
                            None => (
                                "commit",
                                "COMMIT outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    TxnControl::Rollback => {
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                // Phase 2.3.2e: abort every open sub-xid
                                // too so their writes stay hidden.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                self.inner.snapshot_manager.rollback(ctx.xid);
                                // Phase 2.3.2b: tuples that the txn had
                                // xmax-stamped become live again — wipe xmax
                                // back to 0 so later snapshots see them.
                                self.revive_pending_versioned_updates(conn_id);
                                self.revive_pending_tombstones(conn_id);
                                self.discard_pending_queue_dedup(conn_id);
                                self.discard_pending_kv_watch_events(conn_id);
                                self.discard_pending_queue_wakes(conn_id);
                                self.discard_pending_store_wal_actions(conn_id);
                                self.release_pending_claim_locks(conn_id);
                                ("rollback", format!("ROLLBACK — xid={} aborted", ctx.xid))
                            }
                            None => (
                                "rollback",
                                "ROLLBACK outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    // Phase 2.3.2e: savepoints map onto sub-xids. Each
                    // SAVEPOINT allocates a fresh xid and pushes it
                    // onto the per-txn stack so subsequent writes can
                    // be selectively rolled back. RELEASE pops without
                    // aborting; ROLLBACK TO aborts the sub-xid (and
                    // any nested ones) + revives their tombstones.
                    TxnControl::Savepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let sub = mgr.begin();
                                ctx.savepoints.push((name.clone(), sub));
                                ("savepoint", format!("SAVEPOINT {name} — sub_xid={sub}"))
                            }
                            None => (
                                "savepoint",
                                "SAVEPOINT outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::ReleaseSavepoint(name) => {
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                // RELEASE pops the named savepoint and
                                // any nested ones. Their sub-xids move
                                // to `released_sub_xids` so they commit
                                // (or roll back) alongside the parent
                                // xid — PG semantics: released
                                // savepoints still contribute their
                                // work, but their names are gone.
                                let released = ctx.savepoints.len() - pos;
                                let popped: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                ctx.released_sub_xids.extend(popped);
                                (
                                    "release_savepoint",
                                    format!("RELEASE SAVEPOINT {name} — {released} level(s)"),
                                )
                            }
                            None => (
                                "release_savepoint",
                                "RELEASE outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::RollbackToSavepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        // Splice out the savepoint + nested ones under
                        // a narrow lock, then run the snapshot-manager
                        // + tombstone side-effects without the tx map
                        // held so nothing re-enters.
                        let drop_result: Option<(Xid, Vec<Xid>)> = {
                            let mut guard = self.inner.tx_contexts.write();
                            if let Some(ctx) = guard.get_mut(&conn_id) {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                let savepoint_xid = ctx.savepoints[pos].1;
                                let aborted: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                Some((savepoint_xid, aborted))
                            } else {
                                None
                            }
                        };

                        match drop_result {
                            Some((savepoint_xid, aborted)) => {
                                for x in &aborted {
                                    mgr.rollback(*x);
                                }
                                let reverted_updates =
                                    self.revive_versioned_updates_since(conn_id, savepoint_xid);
                                let revived = self.revive_tombstones_since(conn_id, savepoint_xid);
                                (
                                    "rollback_to_savepoint",
                                    format!(
                                        "ROLLBACK TO SAVEPOINT {name} — aborted {} sub_xid(s), reverted {reverted_updates} update(s), revived {revived} tombstone(s)",
                                        aborted.len(),
                                    ),
                                )
                            }
                            None => (
                                "rollback_to_savepoint",
                                "ROLLBACK TO outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
            // Schema + Sequence DDL (Phase 1.3 PG parity).
            //
            // Schemas are lightweight logical namespaces: a CREATE SCHEMA call
            // just registers the name in `red_config` under `schema.{name}`.
            // Table lookups still happen by collection name; clients using
            // `schema.table` qualified names collapse to collection `schema.table`.
            //
            // Sequences persist a 64-bit counter + metadata (start, increment)
            // in `red_config` under `sequence.{name}.*`. Scalar callers
            // `nextval('name')` / `currval('name')` arrive with the MVCC phase
            // once we have a proper mutating-function dispatch path; for now the
            // DDL just establishes the catalog entry so clients don't error.
            QueryExpr::CreateSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                if store.get_config(&key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("schema {} already exists — skipped", q.name),
                            "create_schema",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "schema {} already exists",
                        q.name
                    )));
                }
                store.set_config_tree(&key, &crate::serde_json::Value::Bool(true));
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} created", q.name),
                    "create_schema",
                ))
            }
            QueryExpr::DropSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                let existed = store.get_config(&key).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "schema {} does not exist",
                        q.name
                    )));
                }
                // Remove marker from red_config via set to null.
                store.set_config_tree(&key, &crate::serde_json::Value::Null);
                let suffix = if q.cascade {
                    " (CASCADE accepted — tables untouched)"
                } else {
                    ""
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} dropped{}", q.name, suffix),
                    "drop_schema",
                ))
            }
            QueryExpr::CreateSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let start_key = format!("{base}.start");
                let incr_key = format!("{base}.increment");
                let curr_key = format!("{base}.current");
                if store.get_config(&start_key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("sequence {} already exists — skipped", q.name),
                            "create_sequence",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "sequence {} already exists",
                        q.name
                    )));
                }
                // Persist start + increment, and set current so the first
                // nextval returns `start`.
                let initial_current = q.start - q.increment;
                store.set_config_tree(
                    &start_key,
                    &crate::serde_json::Value::Number(q.start as f64),
                );
                store.set_config_tree(
                    &incr_key,
                    &crate::serde_json::Value::Number(q.increment as f64),
                );
                store.set_config_tree(
                    &curr_key,
                    &crate::serde_json::Value::Number(initial_current as f64),
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "sequence {} created (start={}, increment={})",
                        q.name, q.start, q.increment
                    ),
                    "create_sequence",
                ))
            }
            QueryExpr::DropSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let existed = store.get_config(&format!("{base}.start")).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "sequence {} does not exist",
                        q.name
                    )));
                }
                for k in ["start", "increment", "current"] {
                    store.set_config_tree(&format!("{base}.{k}"), &crate::serde_json::Value::Null);
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("sequence {} dropped", q.name),
                    "drop_sequence",
                ))
            }
            // Views — CREATE [MATERIALIZED] VIEW (Phase 2.1 PG parity).
            //
            // The view definition is stored in-memory on RuntimeInner (not
            // persisted). SELECTs that reference the view name will substitute
            // the stored `QueryExpr` via `resolve_view_reference` during
            // planning (same entry point used by table-name resolution).
            //
            // Materialized views additionally allocate a slot in
            // `MaterializedViewCache`; a REFRESH repopulates that slot.
            QueryExpr::CreateView(ref q) => {
                let mut views = self.inner.views.write();
                if views.contains_key(&q.name) && !q.or_replace {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("view {} already exists — skipped", q.name),
                            "create_view",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "view {} already exists",
                        q.name
                    )));
                }
                views.insert(q.name.clone(), Arc::new(q.clone()));
                drop(views);

                // Materialized view: register cache slot (data is empty until REFRESH).
                if q.materialized {
                    use crate::storage::cache::result::{MaterializedViewDef, RefreshPolicy};
                    let refresh = match q.refresh_every_ms {
                        Some(ms) => RefreshPolicy::Periodic(std::time::Duration::from_millis(ms)),
                        None => RefreshPolicy::Manual,
                    };
                    let dependencies = collect_table_refs(&q.query);
                    let def = MaterializedViewDef {
                        name: q.name.clone(),
                        query: format!("<parsed view {}>", q.name),
                        dependencies: dependencies.clone(),
                        refresh,
                        retention_duration_ms: q.retention_duration_ms,
                    };
                    self.inner.materialized_views.write().register(def);

                    // Issue #593 slice 9a — persist the descriptor to
                    // the system catalog so the definition survives a
                    // restart. Upsert semantics (delete-then-insert by
                    // name) keep the catalog free of duplicate rows
                    // across `CREATE OR REPLACE` churn.
                    let descriptor =
                        crate::runtime::continuous_materialized_view::MaterializedViewDescriptor {
                            name: q.name.clone(),
                            source_sql: query.to_string(),
                            source_collections: dependencies,
                            refresh_every_ms: q.refresh_every_ms,
                            retention_duration_ms: q.retention_duration_ms,
                        };
                    let store = self.inner.db.store();
                    crate::runtime::continuous_materialized_view::persist_descriptor(
                        store.as_ref(),
                        &descriptor,
                    )?;

                    // Issue #594 slice 9b — provision a Table-shaped
                    // backing collection named after the view. The
                    // rewriter skips materialized views (see
                    // `rewrite_view_refs_inner`) so `SELECT FROM v`
                    // resolves to this collection directly. Empty
                    // until REFRESH wires through it in 9c.
                    self.ensure_materialized_view_backing(&q.name)?;
                }
                // Plan cache may have cached a plan that didn't know about this
                // view — invalidate so future references pick up the new binding.
                // Result cache gets flushed too: OR REPLACE must not serve a
                // prior execution of the obsolete body.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "{}view {} created",
                        if q.materialized { "materialized " } else { "" },
                        q.name
                    ),
                    "create_view",
                ))
            }
            QueryExpr::DropView(ref q) => {
                let mut views = self.inner.views.write();
                let removed = views.remove(&q.name);
                let existed = removed.is_some();
                let removed_materialized =
                    removed.as_ref().map(|v| v.materialized).unwrap_or(false);
                drop(views);
                if q.materialized || existed {
                    // Try the materialised cache too — silent if absent.
                    self.inner.materialized_views.write().remove(&q.name);
                    // Issue #593 slice 9a — remove any persisted
                    // catalog row. Idempotent: a no-op when the view
                    // was never materialized (no row was ever written).
                    let store = self.inner.db.store();
                    crate::runtime::continuous_materialized_view::remove_by_name(
                        store.as_ref(),
                        &q.name,
                    )?;
                }
                // Issue #594 slice 9b — drop the backing collection
                // that was provisioned at CREATE time. Only mat views
                // ever had one; regular views never did.
                if removed_materialized || q.materialized {
                    self.drop_materialized_view_backing(&q.name)?;
                }
                // Drop any plan / result cache entries that baked the
                // view body into their QueryExpr.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "view {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("view {} dropped", q.name),
                    "drop_view",
                ))
            }
            QueryExpr::RefreshMaterializedView(ref q) => {
                // Look up the view definition, execute its underlying query,
                // and stash the serialized result in the materialised cache.
                let view = {
                    let views = self.inner.views.read();
                    views.get(&q.name).cloned()
                };
                let view = match view {
                    Some(v) => v,
                    None => {
                        return Err(RedDBError::Internal(format!(
                            "view {} does not exist",
                            q.name
                        )))
                    }
                };
                if !view.materialized {
                    return Err(RedDBError::Internal(format!(
                        "view {} is not materialized — REFRESH requires \
                         CREATE MATERIALIZED VIEW",
                        q.name
                    )));
                }
                // Execute the underlying query fresh.
                let started = std::time::Instant::now();
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                match self.execute_query_expr((*view.query).clone()) {
                    Ok(inner_result) => {
                        // Issue #595 slice 9c — atomically replace the
                        // backing collection's contents under a single
                        // WAL group. Concurrent SELECT from the view
                        // sees either the prior or new contents, never
                        // partial. A crash before the WAL commit lands
                        // leaves the prior contents intact on recovery.
                        let entities =
                            view_records_to_entities(&q.name, &inner_result.result.records);
                        let row_count = entities.len() as u64;
                        let store = self.inner.db.store();
                        let serialized_records = match store.refresh_collection(&q.name, entities) {
                            Ok(records) => records,
                            Err(err) => {
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let msg = err.to_string();
                                self.inner
                                    .materialized_views
                                    .write()
                                    .record_refresh_failure(
                                        &q.name,
                                        msg.clone(),
                                        duration_ms,
                                        now_ms,
                                    );
                                return Err(RedDBError::Internal(format!(
                                    "REFRESH MATERIALIZED VIEW {}: {msg}",
                                    q.name
                                )));
                            }
                        };

                        // Issue #596 slice 9d — emit a Refresh
                        // ChangeRecord into the logical-WAL spool so
                        // replicas deterministically replay the same
                        // backing-collection contents via
                        // `LogicalChangeApplier::apply_record`.
                        if let Some(ref primary) = self.inner.db.replication {
                            let lsn = self.inner.cdc.emit(
                                crate::replication::cdc::ChangeOperation::Refresh,
                                &q.name,
                                0,
                                "refresh",
                            );
                            self.invalidate_result_cache_for_table(&q.name);
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let record = ChangeRecord::for_refresh(
                                lsn,
                                timestamp,
                                q.name.clone(),
                                serialized_records,
                            )
                            .with_term(self.current_replication_term());
                            let encoded = record.encode();
                            primary.append_logical_record(record.lsn, encoded);
                        }

                        let duration_ms = started.elapsed().as_millis() as u64;
                        let serialized = format!("{:?}", inner_result.result);
                        self.inner
                            .materialized_views
                            .write()
                            .record_refresh_success(
                                &q.name,
                                serialized.into_bytes(),
                                row_count,
                                duration_ms,
                                now_ms,
                            );
                        // SELECT FROM v now reads through the rewriter
                        // skip into the backing collection — drop the
                        // result cache so prior empty-backing reads
                        // don't shadow the new contents.
                        self.invalidate_result_cache();
                        Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("materialized view {} refreshed", q.name),
                            "refresh_materialized_view",
                        ))
                    }
                    Err(err) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        let msg = err.to_string();
                        self.inner
                            .materialized_views
                            .write()
                            .record_refresh_failure(&q.name, msg.clone(), duration_ms, now_ms);
                        Err(err)
                    }
                }
            }
            // Row Level Security (Phase 2.5 PG parity).
            //
            // Policies live in an in-memory registry keyed by (table, name).
            // Enforcement (AND-ing the policy's USING clause into every
            // query's WHERE for the table) arrives in Phase 2.5.2 via the
            // filter compiler; this dispatch only manages the catalog.
            QueryExpr::CreatePolicy(ref q) => {
                let key = (q.table.clone(), q.name.clone());
                self.inner
                    .rls_policies
                    .write()
                    .insert(key, Arc::new(q.clone()));
                self.invalidate_plan_cache();
                // Issue #120 — surface policy names in the
                // schema-vocabulary so AskPipeline (#121) can resolve
                // a policy reference back to its table.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::CreatePolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} created", q.name, q.table),
                    "create_policy",
                ))
            }
            QueryExpr::DropPolicy(ref q) => {
                let removed = self
                    .inner
                    .rls_policies
                    .write()
                    .remove(&(q.table.clone(), q.name.clone()))
                    .is_some();
                if !removed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "policy {} on {} does not exist",
                        q.name, q.table
                    )));
                }
                self.invalidate_plan_cache();
                // Issue #120 — keep the schema-vocabulary policy
                // entry in sync.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::DropPolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} dropped", q.name, q.table),
                    "drop_policy",
                ))
            }
            // Foreign Data Wrappers (Phase 3.2 PG parity).
            //
            // CREATE SERVER / CREATE FOREIGN TABLE register into the shared
            // `ForeignTableRegistry`. The read path consults that registry
            // before dispatching a SELECT — when the table name matches a
            // registered foreign table, we forward the scan to the wrapper
            // and skip the normal collection lookup.
            //
            // Phase 3.2 is in-memory only; persistence across restarts is a
            // 3.2.2 follow-up that mirrors the view registry pattern.
            QueryExpr::CreateServer(ref q) => {
                use crate::storage::fdw::FdwOptions;
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.server(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("server {} already exists — skipped", q.name),
                            "create_server",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "server {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                registry
                    .create_server(&q.name, &q.wrapper, opts)
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("server {} created (wrapper {})", q.name, q.wrapper),
                    "create_server",
                ))
            }
            QueryExpr::DropServer(ref q) => {
                let existed = self.inner.foreign_tables.drop_server(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "server {} does not exist",
                        q.name
                    )));
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "server {} dropped{}",
                        q.name,
                        if q.cascade { " (cascade)" } else { "" }
                    ),
                    "drop_server",
                ))
            }
            QueryExpr::CreateForeignTable(ref q) => {
                use crate::storage::fdw::{FdwOptions, ForeignColumn, ForeignTable};
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.foreign_table(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("foreign table {} already exists — skipped", q.name),
                            "create_foreign_table",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                let columns: Vec<ForeignColumn> = q
                    .columns
                    .iter()
                    .map(|c| ForeignColumn {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        not_null: c.not_null,
                    })
                    .collect();
                registry
                    .create_foreign_table(ForeignTable {
                        name: q.name.clone(),
                        server_name: q.server.clone(),
                        columns,
                        options: opts,
                    })
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} created (server {})", q.name, q.server),
                    "create_foreign_table",
                ))
            }
            QueryExpr::DropForeignTable(ref q) => {
                let existed = self.inner.foreign_tables.drop_foreign_table(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} dropped", q.name),
                    "drop_foreign_table",
                ))
            }
            // COPY table FROM 'path' (Phase 1.5 PG parity).
            //
            // Stream CSV rows through the shared `CsvImporter`. The collection
            // is auto-created on first insert (via `insert_auto`-style path);
            // VACUUM/ANALYZE afterwards is up to the caller.
            QueryExpr::CopyFrom(ref q) => {
                use crate::storage::import::{CsvConfig, CsvImporter};
                let store = self.inner.db.store();
                let cfg = CsvConfig {
                    collection: q.table.clone(),
                    has_header: q.has_header,
                    delimiter: q.delimiter.map(|c| c as u8).unwrap_or(b','),
                    ..CsvConfig::default()
                };
                let importer = CsvImporter::new(cfg);
                let stats = importer
                    .import_file(&q.path, store.as_ref())
                    .map_err(|e| RedDBError::Internal(format!("COPY failed: {e}")))?;
                // Tables are written → invalidate cached plans / result cache.
                self.note_table_write(&q.table);
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "COPY imported {} rows into {} ({} errors skipped, {}ms)",
                        stats.records_imported, q.table, stats.errors_skipped, stats.duration_ms
                    ),
                    "copy_from",
                ))
            }
            // Maintenance commands (Phase 1.2 PG parity).
            //
            // - VACUUM [FULL] [table]: refreshes planner stats for the target
            //   collection(s) and — when FULL — triggers a full pager persist
            //   (flushes dirty pages + fsync). Also invalidates the result cache
            //   so subsequent reads re-execute against the freshly compacted
            //   storage. RedDB's segment/btree GC runs continuously via the
            //   background lifecycle; explicit space reclamation for sealed
            //   segments arrives with Phase 2.3 (MVCC + dead-tuple reclamation).
            // - ANALYZE [table]: reruns `analyze_collection` +
            //   `persist_table_stats` via `refresh_table_planner_stats` so the
            //   planner has fresh histograms, distinct estimates, null counts.
            //
            // Both commands accept an optional target; omitting the target
            // iterates every collection in the store.
            QueryExpr::MaintenanceCommand(ref cmd) => {
                use crate::storage::query::ast::MaintenanceCommand as Mc;
                let store = self.inner.db.store();
                let (kind, msg) = match cmd {
                    Mc::Analyze { target } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        (
                            "analyze",
                            format!("ANALYZE refreshed stats for {} table(s)", targets.len()),
                        )
                    }
                    Mc::Vacuum { target, full } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        let cutoff_xid = self.mvcc_vacuum_cutoff_xid();
                        let mut vacuum_stats =
                            crate::storage::unified::store::MvccVacuumStats::default();
                        for t in &targets {
                            let stats = store.vacuum_mvcc_history(t, cutoff_xid).map_err(|e| {
                                RedDBError::Internal(format!(
                                    "VACUUM MVCC history failed for {t}: {e}"
                                ))
                            })?;
                            if stats.reclaimed_versions > 0 {
                                self.rebuild_runtime_indexes_for_table(t)?;
                            }
                            vacuum_stats.add(&stats);
                        }
                        self.inner.snapshot_manager.prune_aborted(cutoff_xid);
                        // Stats refresh covers every target (same as ANALYZE).
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        // FULL forces a pager persist (dirty-page flush + fsync).
                        // Regular VACUUM relies on the background writer / segment
                        // lifecycle so the command is non-blocking.
                        let persisted = if *full {
                            match store.persist() {
                                Ok(()) => true,
                                Err(e) => {
                                    return Err(RedDBError::Internal(format!(
                                        "VACUUM FULL persist failed: {e:?}"
                                    )));
                                }
                            }
                        } else {
                            false
                        };
                        // Result cache depended on pre-vacuum state.
                        self.invalidate_result_cache();
                        (
                            "vacuum",
                            format!(
                                "VACUUM{} processed {} table(s): scanned_versions={}, retained_versions={}, reclaimed_versions={}, retained_history_versions={}, reclaimed_history_versions={}, retained_tombstones={}, reclaimed_tombstones={}{}",
                                if *full { " FULL" } else { "" },
                                targets.len(),
                                vacuum_stats.scanned_versions,
                                vacuum_stats.retained_versions,
                                vacuum_stats.reclaimed_versions,
                                vacuum_stats.retained_history_versions,
                                vacuum_stats.reclaimed_history_versions,
                                vacuum_stats.retained_tombstones,
                                vacuum_stats.reclaimed_tombstones,
                                if persisted {
                                    " (pages flushed to disk)"
                                } else {
                                    ""
                                }
                            ),
                        )
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
            // GRANT / REVOKE / ALTER USER (RBAC milestone).
            //
            // These hit the AuthStore directly. The statement frame /
            // privilege gate has already decided whether the caller may
            // even run the statement; here we just translate the AST into
            // AuthStore calls.
            QueryExpr::Grant(ref g) => self.execute_grant_statement(query, g),
            QueryExpr::Revoke(ref r) => self.execute_revoke_statement(query, r),
            QueryExpr::AlterUser(ref a) => self.execute_alter_user_statement(query, a),
            QueryExpr::CreateUser(ref u) => self.execute_create_user_statement(query, u),
            QueryExpr::CreateIamPolicy { ref id, ref json } => {
                self.execute_create_iam_policy(query, id, json)
            }
            QueryExpr::DropIamPolicy { ref id } => self.execute_drop_iam_policy(query, id),
            QueryExpr::AttachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_attach_policy(query, policy_id, principal),
            QueryExpr::DetachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_detach_policy(query, policy_id, principal),
            QueryExpr::ShowPolicies { ref filter } => {
                self.execute_show_policies(query, filter.as_ref())
            }
            QueryExpr::ShowEffectivePermissions {
                ref user,
                ref resource,
            } => self.execute_show_effective_permissions(query, user, resource.as_ref()),
            QueryExpr::SimulatePolicy {
                ref user,
                ref action,
                ref resource,
            } => self.execute_simulate_policy(query, user, action, resource),
            QueryExpr::LintPolicy { ref source } => self.execute_lint_policy(query, source),
            QueryExpr::MigratePolicyMode {
                ref target,
                dry_run,
            } => self.execute_migrate_policy_mode(query, target, dry_run),
            QueryExpr::CreateMigration(ref q) => self.execute_create_migration(query, q),
            QueryExpr::ApplyMigration(ref q) => self.execute_apply_migration(query, q),
            QueryExpr::RollbackMigration(ref q) => self.execute_rollback_migration(query, q),
            QueryExpr::ExplainMigration(ref q) => self.execute_explain_migration(query, q),
            _ => Err(RedDBError::Query(
                "unsupported command in runtime dispatcher".to_string(),
            )),
        };

        if !control_event_specs.is_empty() {
            let (outcome, reason) = match &query_result {
                Ok(_) => (crate::runtime::control_events::Outcome::Allowed, None),
                Err(err) => (control_event_outcome_for_error(err), Some(err.to_string())),
            };
            for spec in &control_event_specs {
                self.emit_control_event(
                    spec.kind,
                    outcome,
                    spec.action,
                    spec.resource.clone(),
                    reason.clone(),
                    spec.fields.clone(),
                )?;
            }
        }

        if let (Some(plan), Ok(result)) = (&query_audit_plan, &query_result) {
            self.emit_query_audit(
                query,
                plan,
                query_audit_started.elapsed().as_millis() as u64,
                result,
            );
        }

        // Decrypt Value::Secret columns in-place before caching, so
        // cached results match the post-decrypt shape and repeat
        // queries skip the per-row AES-GCM pass.
        let mut query_result = query_result;
        if let Ok(ref mut result) = query_result {
            if result.statement_type == "select" {
                self.apply_secret_decryption(result);
            }
        }

        // Cache SELECT results for 30s.
        // Skip: pre-serialized JSON (large clone), and result sets > 5 rows.
        // Large multi-row results (range scans, filtered scans) are rarely
        // repeated with the same literal values so the cache hit rate is near
        // zero while the clone cost (100 records × ~16 fields each) is high.
        // Aggregations (1 row) and point lookups (1 row) still benefit.
        if let Ok(ref result) = query_result {
            frame.write_result_cache(self, result, result_cache_scopes);
        }

        query_result
    }

    /// Execute a pre-parsed `QueryExpr` directly, bypassing SQL parsing and the
    /// plan cache. Used by the prepared-statement fast path so that `execute_prepared`
    /// calls pay zero parse + cache overhead.
    ///
    /// Applies secret decryption on SELECT results, identical to `execute_query`.
    pub fn execute_query_expr(&self, expr: QueryExpr) -> RedDBResult<RuntimeQueryResult> {
        let _config_snapshot_guard = ConfigSnapshotGuard::install(
            Arc::clone(&self.inner.db),
            self.inner.auth_store.read().clone(),
        );
        let _secret_store_guard = SecretStoreGuard::install(self.inner.auth_store.read().clone());
        // View rewrite (Phase 2.1): substitute any `QueryExpr::Table(tq)`
        // whose `tq.table` matches a registered view with the view's
        // underlying query. Safe to call even when no views are registered.
        let expr = self.rewrite_view_refs(expr);

        self.validate_model_operations_before_auth(&expr)?;
        // Granular RBAC privilege check. Runs before dispatch so a
        // denied caller never reaches storage. Fail-closed: any error
        // resolving the action / resource produces PermissionDenied.
        if let Err(err) = self.check_query_privilege(&expr) {
            return Err(RedDBError::Query(format!("permission denied: {err}")));
        }

        let statement = query_expr_name(&expr);
        let mode = detect_mode(statement);
        let query_str = statement;

        let result = self.dispatch_expr(expr, query_str, mode)?;
        let mut r = result;
        if r.statement_type == "select" {
            self.apply_secret_decryption(&mut r);
        }
        Ok(r)
    }

    pub(super) fn validate_model_operations_before_auth(
        &self,
        expr: &QueryExpr,
    ) -> RedDBResult<()> {
        use crate::catalog::CollectionModel;
        use crate::runtime::ddl::polymorphic_resolver;
        use crate::storage::query::ast::KvCommand;

        let system_schema_target = match expr {
            QueryExpr::DropTable(q) => Some(q.name.as_str()),
            QueryExpr::DropGraph(q) => Some(q.name.as_str()),
            QueryExpr::DropVector(q) => Some(q.name.as_str()),
            QueryExpr::DropDocument(q) => Some(q.name.as_str()),
            QueryExpr::DropKv(q) => Some(q.name.as_str()),
            QueryExpr::DropCollection(q) => Some(q.name.as_str()),
            QueryExpr::Truncate(q) => Some(q.name.as_str()),
            _ => None,
        };
        if system_schema_target.is_some_and(crate::runtime::impl_ddl::is_system_schema_name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }

        let expected = match expr {
            QueryExpr::DropTable(q) => Some((q.name.as_str(), CollectionModel::Table)),
            QueryExpr::DropGraph(q) => Some((q.name.as_str(), CollectionModel::Graph)),
            QueryExpr::DropVector(q) => Some((q.name.as_str(), CollectionModel::Vector)),
            QueryExpr::DropDocument(q) => Some((q.name.as_str(), CollectionModel::Document)),
            QueryExpr::DropKv(q) => Some((q.name.as_str(), q.model)),
            QueryExpr::DropCollection(q) => q.model.map(|model| (q.name.as_str(), model)),
            QueryExpr::Truncate(q) => q.model.map(|model| (q.name.as_str(), model)),
            QueryExpr::KvCommand(cmd) => {
                let (collection, model) = match cmd {
                    KvCommand::Put {
                        collection, model, ..
                    }
                    | KvCommand::Get {
                        collection, model, ..
                    }
                    | KvCommand::Incr {
                        collection, model, ..
                    }
                    | KvCommand::Cas {
                        collection, model, ..
                    }
                    | KvCommand::List {
                        collection, model, ..
                    }
                    | KvCommand::Delete {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Rotate { collection, .. }
                    | KvCommand::History { collection, .. }
                    | KvCommand::Purge { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                    KvCommand::InvalidateTags { collection, .. } => {
                        (collection.as_str(), CollectionModel::Kv)
                    }
                    KvCommand::Watch {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Unseal { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                };
                Some((collection, model))
            }
            QueryExpr::ConfigCommand(cmd) => {
                self.validate_config_command_before_auth(cmd)?;
                None
            }
            _ => None,
        };

        let Some((name, expected_model)) = expected else {
            return Ok(());
        };
        let snapshot = self.inner.db.catalog_model_snapshot();
        let Some(actual_model) = snapshot
            .collections
            .iter()
            .find(|collection| collection.name == name)
            .map(|collection| collection.declared_model.unwrap_or(collection.model))
        else {
            return Ok(());
        };
        polymorphic_resolver::ensure_model_match(expected_model, actual_model)
    }

    /// Walk a `QueryExpr` and replace `QueryExpr::Table(tq)` nodes whose
    /// `tq.table` matches a registered view name with the view's stored
    /// body. Recurses through joins so `SELECT ... FROM t JOIN myview ...`
    /// resolves correctly. Pure operation — no side effects.
    pub(super) fn rewrite_view_refs(&self, expr: QueryExpr) -> QueryExpr {
        // Fast path: no views registered → return original expression.
        if self.inner.views.read().is_empty() {
            return expr;
        }
        self.rewrite_view_refs_inner(expr)
    }

    fn rewrite_view_refs_inner(&self, expr: QueryExpr) -> QueryExpr {
        use crate::storage::query::ast::{Filter, TableSource};
        match expr {
            QueryExpr::Table(mut tq) => {
                // 1. If the TableSource is a subquery, recurse into it so
                //    `SELECT ... FROM (SELECT ... FROM myview) t` expands.
                //    The legacy `table` field (set to a synthetic
                //    "__subq_NNNN" sentinel) stays as-is so callers that
                //    read it keep compiling.
                if let Some(TableSource::Subquery(body)) = tq.source.take() {
                    tq.source = Some(TableSource::Subquery(Box::new(
                        self.rewrite_view_refs_inner(*body),
                    )));
                    return QueryExpr::Table(tq);
                }

                // 2. Restore the source field (took it above for match).
                // When the source was `None` or `TableSource::Name(_)`, the
                // real lookup key is `tq.table` — check the view registry.
                let maybe_view = {
                    let views = self.inner.views.read();
                    views.get(&tq.table).cloned()
                };
                let Some(view) = maybe_view else {
                    return QueryExpr::Table(tq);
                };

                // Issue #594 slice 9b — materialized views are read
                // from their backing collection, not by substituting
                // the body. Returning the TableQuery as-is lets the
                // normal table-read path resolve `SELECT FROM v`
                // against the collection provisioned at CREATE time.
                if view.materialized {
                    return QueryExpr::Table(tq);
                }

                // Recurse into the view body — views may reference other
                // views. The recursion yields the final QueryExpr we need
                // to merge the outer's filter / limit / offset into.
                let inner_expr = self.rewrite_view_refs_inner((*view.query).clone());

                // Phase 5: when the body is a Table we merge the outer
                // TableQuery's WHERE / LIMIT / OFFSET into it so stacked
                // views filter recursively. Non-table bodies (Search,
                // Ask, Vector, Graph, Hybrid) can't meaningfully combine
                // with an outer Table query today — return the body
                // verbatim; outer predicates are lost. Full projection
                // merge lands in Phase 5.2.
                match inner_expr {
                    QueryExpr::Table(mut inner_tq) => {
                        if let Some(outer_filter) = tq.filter.take() {
                            inner_tq.filter = Some(match inner_tq.filter.take() {
                                Some(existing) => {
                                    Filter::And(Box::new(existing), Box::new(outer_filter))
                                }
                                None => outer_filter,
                            });
                            // Keep the `Expr` form in lock-step with the
                            // merged `Filter`. The executor prefers
                            // `where_expr` and nulls `filter` when it is
                            // present (see `execute_query_inner`), so a
                            // stacked view whose outer predicate was only
                            // merged into `filter` would silently drop that
                            // predicate at eval time (#635).
                            inner_tq.where_expr = inner_tq
                                .filter
                                .as_ref()
                                .map(crate::storage::query::sql_lowering::filter_to_expr);
                        }
                        if let Some(outer_limit) = tq.limit {
                            inner_tq.limit = Some(match inner_tq.limit {
                                Some(existing) => existing.min(outer_limit),
                                None => outer_limit,
                            });
                        }
                        if let Some(outer_offset) = tq.offset {
                            inner_tq.offset = Some(match inner_tq.offset {
                                Some(existing) => existing + outer_offset,
                                None => outer_offset,
                            });
                        }
                        QueryExpr::Table(inner_tq)
                    }
                    other => other,
                }
            }
            QueryExpr::Join(mut jq) => {
                jq.left = Box::new(self.rewrite_view_refs_inner(*jq.left));
                jq.right = Box::new(self.rewrite_view_refs_inner(*jq.right));
                QueryExpr::Join(jq)
            }
            // Other variants don't carry nested QueryExpr that can reference
            // a view by table name. Return as-is.
            other => other,
        }
    }

    fn resolve_table_expr_subqueries(
        &self,
        mut table: TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<TableQuery> {
        // Only a `Subquery` source needs recursive resolution. `.take()`
        // would otherwise drop a `Name` / `Function` source on the floor
        // (the `if let` skips the body but the take already cleared it),
        // which silently broke `SELECT * FROM components(g)` — the TVF
        // dispatch downstream keys off `TableSource::Function` and never
        // fired. Restore any non-subquery source unchanged (issue #795).
        match table.source.take() {
            Some(TableSource::Subquery(inner)) => {
                let inner = self.resolve_select_expr_subqueries(*inner, frame)?;
                table.source = Some(TableSource::Subquery(Box::new(inner)));
            }
            other => table.source = other,
        }

        let outer_scopes = relation_scopes_for_query(&QueryExpr::Table(table.clone()));
        for item in &mut table.select_items {
            if let crate::storage::query::ast::SelectItem::Expr { expr, .. } = item {
                *expr = self.resolve_expr_subqueries(expr.clone(), &outer_scopes, frame)?;
            }
        }
        if let Some(where_expr) = table.where_expr.take() {
            table.where_expr =
                Some(self.resolve_expr_subqueries(where_expr, &outer_scopes, frame)?);
            table.filter = None;
        }
        if let Some(having_expr) = table.having_expr.take() {
            table.having_expr =
                Some(self.resolve_expr_subqueries(having_expr, &outer_scopes, frame)?);
            table.having = None;
        }
        for expr in &mut table.group_by_exprs {
            *expr = self.resolve_expr_subqueries(expr.clone(), &outer_scopes, frame)?;
        }
        for clause in &mut table.order_by {
            if let Some(expr) = clause.expr.take() {
                clause.expr = Some(self.resolve_expr_subqueries(expr, &outer_scopes, frame)?);
            }
        }
        Ok(table)
    }

    fn resolve_select_expr_subqueries(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(table) => self
                .resolve_table_expr_subqueries(table, frame)
                .map(QueryExpr::Table),
            QueryExpr::Join(mut join) => {
                join.left = Box::new(self.resolve_select_expr_subqueries(*join.left, frame)?);
                join.right = Box::new(self.resolve_select_expr_subqueries(*join.right, frame)?);
                Ok(QueryExpr::Join(join))
            }
            other => Ok(other),
        }
    }

    fn resolve_expr_subqueries(
        &self,
        expr: crate::storage::query::ast::Expr,
        outer_scopes: &[String],
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<crate::storage::query::ast::Expr> {
        use crate::storage::query::ast::Expr;

        match expr {
            Expr::Subquery { query, span } => {
                let values = self.execute_expr_subquery_values(query, outer_scopes, frame)?;
                if values.len() > 1 {
                    return Err(RedDBError::Query(
                        "scalar subquery returned more than one row".to_string(),
                    ));
                }
                Ok(Expr::Literal {
                    value: values.into_iter().next().unwrap_or(Value::Null),
                    span,
                })
            }
            Expr::BinaryOp { op, lhs, rhs, span } => Ok(Expr::BinaryOp {
                op,
                lhs: Box::new(self.resolve_expr_subqueries(*lhs, outer_scopes, frame)?),
                rhs: Box::new(self.resolve_expr_subqueries(*rhs, outer_scopes, frame)?),
                span,
            }),
            Expr::UnaryOp { op, operand, span } => Ok(Expr::UnaryOp {
                op,
                operand: Box::new(self.resolve_expr_subqueries(*operand, outer_scopes, frame)?),
                span,
            }),
            Expr::Cast {
                inner,
                target,
                span,
            } => Ok(Expr::Cast {
                inner: Box::new(self.resolve_expr_subqueries(*inner, outer_scopes, frame)?),
                target,
                span,
            }),
            Expr::FunctionCall { name, args, span } => {
                let args = args
                    .into_iter()
                    .map(|arg| self.resolve_expr_subqueries(arg, outer_scopes, frame))
                    .collect::<RedDBResult<Vec<_>>>()?;
                Ok(Expr::FunctionCall { name, args, span })
            }
            Expr::Case {
                branches,
                else_,
                span,
            } => {
                let branches = branches
                    .into_iter()
                    .map(|(cond, value)| {
                        Ok((
                            self.resolve_expr_subqueries(cond, outer_scopes, frame)?,
                            self.resolve_expr_subqueries(value, outer_scopes, frame)?,
                        ))
                    })
                    .collect::<RedDBResult<Vec<_>>>()?;
                let else_ = else_
                    .map(|expr| self.resolve_expr_subqueries(*expr, outer_scopes, frame))
                    .transpose()?
                    .map(Box::new);
                Ok(Expr::Case {
                    branches,
                    else_,
                    span,
                })
            }
            Expr::IsNull {
                operand,
                negated,
                span,
            } => Ok(Expr::IsNull {
                operand: Box::new(self.resolve_expr_subqueries(*operand, outer_scopes, frame)?),
                negated,
                span,
            }),
            Expr::InList {
                target,
                values,
                negated,
                span,
            } => {
                let target =
                    Box::new(self.resolve_expr_subqueries(*target, outer_scopes, frame)?);
                let mut resolved = Vec::new();
                for value in values {
                    if let Expr::Subquery { query, .. } = value {
                        resolved.extend(
                            self.execute_expr_subquery_values(query, outer_scopes, frame)?
                                .into_iter()
                                .map(Expr::lit),
                        );
                    } else {
                        resolved.push(self.resolve_expr_subqueries(value, outer_scopes, frame)?);
                    }
                }
                Ok(Expr::InList {
                    target,
                    values: resolved,
                    negated,
                    span,
                })
            }
            Expr::Between {
                target,
                low,
                high,
                negated,
                span,
            } => Ok(Expr::Between {
                target: Box::new(self.resolve_expr_subqueries(*target, outer_scopes, frame)?),
                low: Box::new(self.resolve_expr_subqueries(*low, outer_scopes, frame)?),
                high: Box::new(self.resolve_expr_subqueries(*high, outer_scopes, frame)?),
                negated,
                span,
            }),
            other => Ok(other),
        }
    }

    fn execute_expr_subquery_values(
        &self,
        subquery: crate::storage::query::ast::ExprSubquery,
        outer_scopes: &[String],
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Vec<Value>> {
        let query = *subquery.query;
        if query_references_outer_scope(&query, outer_scopes) {
            return Err(RedDBError::Query(
                "NOT_YET_SUPPORTED: correlated subqueries are not supported yet; track follow-up issue #470-correlated-subqueries".to_string(),
            ));
        }
        let query = self.rewrite_view_refs(query);
        let query = self.resolve_select_expr_subqueries(query, frame)?;
        let query = self.authorize_relational_select_expr(query, frame)?;
        let result = match query {
            QueryExpr::Table(table) => {
                execute_runtime_table_query(&self.inner.db, &table, Some(&self.inner.index_store))?
            }
            QueryExpr::Join(join) => execute_runtime_join_query(&self.inner.db, &join)?,
            other => {
                return Err(RedDBError::Query(format!(
                    "expression subquery must be a SELECT query, got {}",
                    query_expr_name(&other)
                )))
            }
        };
        first_column_values(result)
    }

    fn dispatch_expr(
        &self,
        expr: QueryExpr,
        query_str: &str,
        mode: QueryMode,
    ) -> RedDBResult<RuntimeQueryResult> {
        let statement = query_expr_name(&expr);
        match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Graph queries are not cacheable as prepared statements.
                Err(RedDBError::Query(
                    "graph queries cannot be used as prepared statements".to_string(),
                ))
            }
            QueryExpr::Table(table) => {
                let scope = self.ai_scope();
                let table = self.resolve_table_expr_subqueries(
                    table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?;
                // Table-valued functions (e.g. components(g)) dispatch to a
                // read-only executor before any catalog/virtual-table routing
                // (issue #795).
                if let Some(TableSource::Function {
                    name,
                    args,
                    named_args,
                }) = table.source.clone()
                {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf",
                        result: self.execute_table_function(&name, &args, &named_args)?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }
                // Inline-graph TVF (issue #799) on the prepared-statement /
                // direct-expr path. Result caching is wired on the
                // `execute_query_inner` path; here we just compute and return.
                if let Some(TableSource::InlineGraphFunction {
                    name,
                    nodes,
                    edges,
                    named_args,
                }) = table.source.clone()
                {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf-inline",
                        result: self.execute_inline_graph_function(
                            &name,
                            &nodes,
                            &edges,
                            &named_args,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &scope as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }
                // `<graph>.<output>` analytics virtual view (issue #800).
                if let Some(view_result) = self.try_resolve_analytics_view(
                    &table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )? {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-analytics-view",
                        result: view_result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                }
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-table-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
                    mode,
                    statement,
                    engine: "runtime-table",
                    result: execute_runtime_table_query(
                        &self.inner.db,
                        &table_with_rls,
                        Some(&self.inner.index_store),
                    )?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            QueryExpr::Join(join) => {
                let scope = self.ai_scope();
                let Some(join_with_rls) = self.authorize_relational_join_select(
                    join,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-join-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                        notice: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                    notice: None,
                })
            }
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
                notice: None,
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
                notice: None,
            }),
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => self
                .with_deferred_store_wal_for_dml(self.insert_may_emit_events(insert), || {
                    self.execute_insert(query_str, insert)
                }),
            QueryExpr::Update(ref update) => self
                .with_deferred_store_wal_for_dml(self.update_may_emit_events(update), || {
                    self.execute_update(query_str, update)
                }),
            QueryExpr::Delete(ref delete) => self
                .with_deferred_store_wal_for_dml(self.delete_may_emit_events(delete), || {
                    self.execute_delete(query_str, delete)
                }),
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query_str, cmd),
            QueryExpr::Ask(ref ask) => self.execute_ask(query_str, ask),
            _ => Err(RedDBError::Query(format!(
                "prepared-statement execution does not support {statement} statements"
            ))),
        }
    }

    /// Dispatch a graph-collection table-valued function call in FROM
    /// position (e.g. `SELECT * FROM components(g)`).
    ///
    /// Validates the function name and arity here, materializes the whole
    /// active graph read-only, then runs the algorithm via the shared
    /// `dispatch_graph_algorithm` path. Never mutates the catalog or store.
    fn execute_table_function(
        &self,
        name: &str,
        args: &[String],
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        if name.eq_ignore_ascii_case("red.diff") {
            return self.execute_vcs_diff_tvf(args, named_args);
        }
        if !is_graph_tvf_name(name) {
            return Err(RedDBError::Query(format!("unknown table function: {name}")));
        }
        // Every graph-collection TVF takes exactly one graph argument.
        if args.len() != 1 {
            return Err(RedDBError::Query(format!(
                "table function '{name}' takes exactly 1 graph argument, got {}",
                args.len()
            )));
        }

        // Read-only materialization of the full active graph. Passing `None`
        // for the projection uses the full graph store. Like #795/#796, the
        // v0 form runs over the whole graph store regardless of the collection
        // argument value. Materialization never mutates any store.
        let (nodes, edges) = self.materialize_whole_graph_abstract()?;
        self.dispatch_graph_algorithm(name, nodes, edges, named_args)
    }

    pub(crate) fn revive_versioned_updates_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let mut guard = self.inner.pending_versioned_updates.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut reverted = 0usize;
        pending.retain(|(collection, old_id, new_id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut old) = manager.get(*old_id) {
                    if old.xmax == *xid {
                        old.set_xmax(*previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(collection, &[*new_id]);
            reverted += 1;
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        reverted
    }

    /// Wrap the planner's `RuntimeQueryExplain` as rows on a
    /// `RuntimeQueryResult` so callers over the SQL interface see the
    /// plan tree in the same shape a SELECT produces.
    ///
    /// Columns: `op`, `source`, `estimated_rows`, `estimated_cost`, `depth`.
    /// Nodes are walked depth-first; `depth` counts from 0 at the
    /// root so a text renderer can indent without re-walking.
    fn explain_as_rows(&self, raw_query: &str, inner_sql: &str) -> RedDBResult<RuntimeQueryResult> {
        let explain = self.explain_query(inner_sql)?;

        let columns = vec![
            "op".to_string(),
            "source".to_string(),
            "estimated_rows".to_string(),
            "estimated_cost".to_string(),
            "depth".to_string(),
        ];

        let mut records: Vec<crate::storage::query::unified::UnifiedRecord> = Vec::new();

        // Prepend `CteScan` markers when the query carried a leading
        // WITH clause. The CTE bodies are already inlined into the
        // main plan tree, but operators reading EXPLAIN need to see
        // which named CTEs were resolved — without this row the plan
        // would look indistinguishable from a hand-inlined query.
        for name in &explain.cte_materializations {
            use std::sync::Arc;
            let mut rec = crate::storage::query::unified::UnifiedRecord::default();
            rec.set_arc(Arc::from("op"), Value::text("CteScan".to_string()));
            rec.set_arc(Arc::from("source"), Value::text(name.clone()));
            rec.set_arc(Arc::from("estimated_rows"), Value::Float(0.0));
            rec.set_arc(Arc::from("estimated_cost"), Value::Float(0.0));
            rec.set_arc(Arc::from("depth"), Value::Integer(0));
            records.push(rec);
        }

        walk_plan_node(&explain.logical_plan.root, 0, &mut records);

        let result = crate::storage::query::unified::UnifiedResult {
            columns,
            records,
            stats: Default::default(),
            pre_serialized_json: None,
        };

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: explain.mode,
            statement: "explain",
            engine: "runtime-explain",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    fn explain_analyze_as_rows(
        &self,
        raw_query: &str,
        inner_sql: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        if !starts_with_dml_keyword(inner_sql) {
            return Err(RedDBError::Query(
                "EXPLAIN ANALYZE currently supports INSERT, UPDATE, and DELETE".to_string(),
            ));
        }

        let explain = self.explain_query(inner_sql)?;
        let conn_id = current_connection_id();
        if self.inner.tx_contexts.read().contains_key(&conn_id) {
            return Err(RedDBError::Query(
                "EXPLAIN ANALYZE requires no active transaction".to_string(),
            ));
        }

        self.execute_query_inner("BEGIN")?;
        let started = std::time::Instant::now();
        let execution = self.execute_query_inner(inner_sql);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let rollback = self.execute_query_inner("ROLLBACK");

        let affected_rows = match execution {
            Ok(result) => result.affected_rows,
            Err(err) => {
                rollback?;
                return Err(err);
            }
        };
        rollback?;

        let columns = vec![
            "op".to_string(),
            "source".to_string(),
            "estimated_rows".to_string(),
            "estimated_cost".to_string(),
            "actual_rows".to_string(),
            "actual_ms".to_string(),
            "depth".to_string(),
        ];
        let mut records = Vec::new();
        walk_analyze_plan_node(
            &explain.logical_plan.root,
            0,
            affected_rows,
            elapsed_ms,
            &mut records,
        );

        let result = crate::storage::query::unified::UnifiedResult {
            columns,
            records,
            stats: Default::default(),
            pre_serialized_json: None,
        };

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: explain.mode,
            statement: "explain_analyze",
            engine: "runtime-explain-analyze",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }
}

fn strip_explain_analyze_prefix(sql: &str) -> Option<&str> {
    let rest = strip_keyword_ci(sql.trim_start(), "EXPLAIN")?.trim_start();
    Some(strip_keyword_ci(rest, "ANALYZE")?.trim_start()).filter(|inner| !inner.is_empty())
}

fn strip_keyword_ci<'a>(sql: &'a str, keyword: &str) -> Option<&'a str> {
    if sql.len() < keyword.len() {
        return None;
    }
    let (head, rest) = sql.split_at(keyword.len());
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    if rest
        .chars()
        .next()
        .is_some_and(|ch| !ch.is_ascii_whitespace())
    {
        return None;
    }
    Some(rest)
}

fn starts_with_dml_keyword(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|ch: char| ch.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    let head = &trimmed[..head_end];
    head.eq_ignore_ascii_case("INSERT")
        || head.eq_ignore_ascii_case("UPDATE")
        || head.eq_ignore_ascii_case("DELETE")
}

fn walk_plan_node(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    depth: usize,
    out: &mut Vec<crate::storage::query::unified::UnifiedRecord>,
) {
    use std::sync::Arc;
    let mut rec = crate::storage::query::unified::UnifiedRecord::default();
    rec.set_arc(Arc::from("op"), Value::text(node.operator.clone()));
    rec.set_arc(
        Arc::from("source"),
        node.source.clone().map(Value::text).unwrap_or(Value::Null),
    );
    rec.set_arc(
        Arc::from("estimated_rows"),
        Value::Float(node.estimated_rows),
    );
    rec.set_arc(
        Arc::from("estimated_cost"),
        Value::Float(node.operator_cost),
    );
    rec.set_arc(Arc::from("depth"), Value::Integer(depth as i64));
    out.push(rec);
    for child in &node.children {
        walk_plan_node(child, depth + 1, out);
    }
}

fn walk_analyze_plan_node(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    depth: usize,
    affected_rows: u64,
    elapsed_ms: f64,
    out: &mut Vec<crate::storage::query::unified::UnifiedRecord>,
) {
    use std::sync::Arc;
    let mut rec = crate::storage::query::unified::UnifiedRecord::default();
    rec.set_arc(Arc::from("op"), Value::text(node.operator.clone()));
    rec.set_arc(
        Arc::from("source"),
        node.source.clone().map(Value::text).unwrap_or(Value::Null),
    );
    rec.set_arc(
        Arc::from("estimated_rows"),
        Value::Float(node.estimated_rows),
    );
    rec.set_arc(
        Arc::from("estimated_cost"),
        Value::Float(node.operator_cost),
    );
    rec.set_arc(
        Arc::from("actual_rows"),
        Value::UnsignedInteger(if depth == 0 { affected_rows } else { 0 }),
    );
    rec.set_arc(
        Arc::from("actual_ms"),
        Value::Float(if depth == 0 { elapsed_ms } else { 0.0 }),
    );
    rec.set_arc(Arc::from("depth"), Value::Integer(depth as i64));
    out.push(rec);

    for child in &node.children {
        walk_analyze_plan_node(child, depth + 1, 0, 0.0, out);
    }
}

#[cfg(test)]
mod inline_graph_tvf_tests {
    use super::*;

    fn scopes_for(sql: &str) -> HashSet<String> {
        let expr = crate::storage::query::parser::parse(sql)
            .expect("parse")
            .query;
        query_expr_result_cache_scopes(&expr)
    }

    #[test]
    fn inline_tvf_cache_scopes_include_source_collections() {
        // The result-cache key for the inline form must derive from the
        // `nodes`/`edges` source collections so a write to either invalidates
        // the cached result (issue #799).
        let scopes = scopes_for(
            "SELECT * FROM components(nodes => (SELECT id FROM hosts), edges => (SELECT src, dst FROM links))",
        );
        assert!(scopes.contains("hosts"), "nodes source scoped: {scopes:?}");
        assert!(scopes.contains("links"), "edges source scoped: {scopes:?}");
    }

    #[test]
    fn graph_collection_tvf_cache_scope_is_graph_argument() {
        // The graph-collection form still materializes the active graph, but
        // result-cache invalidation is scoped to the named graph argument so
        // INSERT INTO g NODE/EDGE invalidates cached TVF rows.
        let scopes = scopes_for("SELECT * FROM components(g)");
        assert!(scopes.contains("g"), "collection form scoped: {scopes:?}");
    }

    #[test]
    fn abstract_degree_centrality_counts_undirected_endpoints() {
        let nodes = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let edges = vec![
            ("a".to_string(), "b".to_string(), 1.0_f32),
            ("b".to_string(), "c".to_string(), 1.0_f32),
        ];
        let degrees = abstract_degree_centrality(&nodes, &edges);
        assert_eq!(
            degrees,
            vec![
                ("a".to_string(), 1),
                ("b".to_string(), 2),
                ("c".to_string(), 1),
            ]
        );
    }
}
