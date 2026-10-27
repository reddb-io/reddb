use super::*;
use super::{
    evaluate_ml_scalar, evaluate_projection_config_function, evaluate_projection_kv_function,
    evaluate_projection_kv_ref, evaluate_projection_secret_ref,
};

pub(crate) fn query_expr_name(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
        QueryExpr::Insert(_) => "insert",
        QueryExpr::Update(_) => "update",
        QueryExpr::Delete(_) => "delete",
        QueryExpr::Function(_) => "function",
        QueryExpr::CreateTable(_) => "create_table",
        QueryExpr::CreateCollection(_) => "create_collection",
        QueryExpr::CreateVector(_) => "create_vector",
        QueryExpr::DropTable(_) => "drop_table",
        QueryExpr::DropGraph(_) => "drop_graph",
        QueryExpr::DropVector(_) => "drop_vector",
        QueryExpr::DropDocument(_) => "drop_document",
        QueryExpr::DropKv(_) => "drop_kv",
        QueryExpr::DropCollection(_) => "drop_collection",
        QueryExpr::Truncate(_) => "truncate",
        QueryExpr::AlterTable(_) => "alter_table",
        QueryExpr::CreateVcsRef(_) => "create_vcs_ref",
        QueryExpr::DropVcsRef(_) => "drop_vcs_ref",
        QueryExpr::GraphCommand(_) => "graph_command",
        QueryExpr::SearchCommand(_) => "search_command",
        QueryExpr::CreateIndex(_) => "create_index",
        QueryExpr::DropIndex(_) => "drop_index",
        QueryExpr::ProbabilisticCommand(_) => "probabilistic_command",
        QueryExpr::Ask(_) => "ask",
        QueryExpr::SetConfig { .. } => "set_config",
        QueryExpr::ShowConfig { .. } => "show_config",
        QueryExpr::SetSecret { .. } => "set_secret",
        QueryExpr::DeleteSecret { .. } => "delete_secret",
        QueryExpr::SetKv { .. } => "set_kv",
        QueryExpr::DeleteKv { .. } => "delete_kv",
        QueryExpr::ShowSecrets { .. } => "show_secrets",
        QueryExpr::SetTenant(_) => "set_tenant",
        QueryExpr::ShowTenant => "show_tenant",
        QueryExpr::CreateTimeSeries(_) => "create_timeseries",
        QueryExpr::CreateMetric(_) => "create_metric",
        QueryExpr::AlterMetric(_) => "alter_metric",
        QueryExpr::CreateSlo(_) => "create_slo",
        QueryExpr::DropTimeSeries(_) => "drop_timeseries",
        QueryExpr::CreateQueue(_) => "create_queue",
        QueryExpr::AlterQueue(_) => "alter_queue",
        QueryExpr::DropQueue(_) => "drop_queue",
        QueryExpr::QueueSelect(_) => "queue_select",
        QueryExpr::QueueCommand(_) => "queue_command",
        QueryExpr::KvCommand(_) => "kv_command",
        QueryExpr::ConfigCommand(_) => "config_command",
        QueryExpr::CreateTree(_) => "create_tree",
        QueryExpr::DropTree(_) => "drop_tree",
        QueryExpr::TreeCommand(_) => "tree_command",
        QueryExpr::ExplainAlter(_) => "explain_alter",
        QueryExpr::TransactionControl(_) => "transaction_control",
        QueryExpr::MaintenanceCommand(_) => "maintenance_command",
        QueryExpr::CreateSchema(_) => "create_schema",
        QueryExpr::DropSchema(_) => "drop_schema",
        QueryExpr::CreateSequence(_) => "create_sequence",
        QueryExpr::DropSequence(_) => "drop_sequence",
        QueryExpr::CopyFrom(_) => "copy_from",
        QueryExpr::CreateView(_) => "create_view",
        QueryExpr::DropView(_) => "drop_view",
        QueryExpr::RefreshMaterializedView(_) => "refresh_materialized_view",
        QueryExpr::CreatePolicy(_) => "create_policy",
        QueryExpr::DropPolicy(_) => "drop_policy",
        QueryExpr::CreateServer(_) => "create_server",
        QueryExpr::DropServer(_) => "drop_server",
        QueryExpr::CreateForeignTable(_) => "create_foreign_table",
        QueryExpr::DropForeignTable(_) => "drop_foreign_table",
        QueryExpr::Grant(_) => "grant",
        QueryExpr::Revoke(_) => "revoke",
        QueryExpr::AlterUser(_) => "alter_user",
        QueryExpr::CreateUser(_) => "create_user",
        QueryExpr::CreateIamPolicy { .. } => "create_iam_policy",
        QueryExpr::DropIamPolicy { .. } => "drop_iam_policy",
        QueryExpr::AttachPolicy { .. } => "attach_policy",
        QueryExpr::DetachPolicy { .. } => "detach_policy",
        QueryExpr::ShowPolicies { .. } => "show_policies",
        QueryExpr::ShowEffectivePermissions { .. } => "show_effective_permissions",
        QueryExpr::RankOf(_) => "rank_of",
        QueryExpr::ApproxRankOf(_) => "approx_rank_of",
        QueryExpr::RankRange(_) => "rank_range",
        QueryExpr::SimulatePolicy { .. } => "simulate_policy",
        QueryExpr::LintPolicy { .. } => "lint_policy",
        QueryExpr::MigratePolicyMode { .. } => "migrate_policy_mode",
        QueryExpr::CreateMigration(_) => "create_migration",
        QueryExpr::ApplyMigration(_) => "apply_migration",
        QueryExpr::RollbackMigration(_) => "rollback_migration",
        QueryExpr::ExplainMigration(_) => "explain_migration",
        QueryExpr::EventsBackfill(_) => "events_backfill",
        QueryExpr::EventsBackfillStatus { .. } => "events_backfill_status",
        _ => "command",
    }
}

/// Evaluate a scalar function on a record's values.
pub(super) fn evaluate_scalar_function(
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    evaluate_scalar_function_with_db(None, name, args, source)
}

pub(crate) fn evaluate_scalar_function_with_db(
    db: Option<&RedDB>,
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let func_name = name.split(':').next().unwrap_or(name);
    if func_name.eq_ignore_ascii_case("CONFIG") {
        return evaluate_projection_config_function(db, args, source);
    }
    if func_name.eq_ignore_ascii_case("KV") {
        return evaluate_projection_kv_function(db, args, source);
    }
    if func_name.eq_ignore_ascii_case("__SECRET_REF") {
        return evaluate_projection_secret_ref(args);
    }
    if func_name.eq_ignore_ascii_case("__KV_REF") {
        return evaluate_projection_kv_ref(args);
    }
    if matches!(
        func_name.to_ascii_uppercase().as_str(),
        "ML_CLASSIFY" | "ML_PREDICT_PROBA" | "SEMANTIC_CACHE_GET" | "SEMANTIC_CACHE_PUT" | "EMBED"
    ) {
        return evaluate_ml_scalar(db?, &func_name.to_ascii_uppercase(), args, source);
    }
    if matches!(
        func_name.to_ascii_uppercase().as_str(),
        "CA_REGISTER" | "CA_DROP" | "CA_STATE" | "CA_LIST" | "CA_REFRESH" | "CA_QUERY"
    ) {
        // Resolve every arg to a Value first, then route to the
        // expr_eval dispatcher so the two surfaces share exactly
        // one code path.
        let resolved: Vec<Value> = (0..args.len())
            .map(|i| resolve_scalar_arg(args, i, source).unwrap_or(Value::Null))
            .collect();
        return super::super::expr_eval::dispatch_ca_function_public(
            db?,
            &func_name.to_ascii_uppercase(),
            &resolved,
        );
    }
    if matches!(
        func_name.to_ascii_uppercase().as_str(),
        "LIST_HYPERTABLES" | "LIST_MODELS" | "SHOW_HYPERTABLES" | "SHOW_MODELS"
    ) {
        return super::super::expr_eval::dispatch_introspection_function_public(
            db?,
            &func_name.to_ascii_uppercase(),
        );
    }
    if func_name.eq_ignore_ascii_case("HYPERTABLE_PRUNE_CHUNKS") {
        let resolved: Vec<Value> = (0..args.len())
            .map(|i| resolve_scalar_arg(args, i, source).unwrap_or(Value::Null))
            .collect();
        return super::super::expr_eval::dispatch_hypertable_prune_public(db?, &resolved);
    }
    if matches!(
        func_name.to_ascii_uppercase().as_str(),
        "HYPERTABLE_DROP_CHUNKS_BEFORE"
            | "HYPERTABLE_SWEEP_EXPIRED"
            | "HYPERTABLE_SHOW_CHUNKS"
            | "HYPERTABLE_SWEEP_ALL_EXPIRED"
            | "HYPERTABLE_SET_TTL"
            | "HYPERTABLE_GET_TTL"
            | "HYPERTABLE_CHUNKS_EXPIRING_WITHIN"
    ) {
        let resolved: Vec<Value> = (0..args.len())
            .map(|i| resolve_scalar_arg(args, i, source).unwrap_or(Value::Null))
            .collect();
        return super::super::expr_eval::dispatch_hypertable_retention_public(
            db?,
            &func_name.to_ascii_uppercase(),
            &resolved,
        );
    }
    if matches!(
        func_name.to_ascii_uppercase().as_str(),
        "MODEL_REGISTER" | "MODEL_DROP"
    ) {
        let resolved: Vec<Value> = (0..args.len())
            .map(|i| resolve_scalar_arg(args, i, source).unwrap_or(Value::Null))
            .collect();
        return super::super::expr_eval::dispatch_model_function_public(
            db?,
            &func_name.to_ascii_uppercase(),
            &resolved,
        );
    }
    if func_name.eq_ignore_ascii_case("red.lca") {
        let resolved: Vec<Value> = (0..args.len())
            .map(|i| resolve_scalar_arg(args, i, source).unwrap_or(Value::Null))
            .collect();
        return super::super::expr_eval::dispatch_vcs_lca_function_public(db?, &resolved);
    }
    evaluate_scalar_function_legacy(name, args, source)
}

fn evaluate_scalar_function_legacy(
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    reddb_engine::query::runtime::scalar_functions::evaluate_scalar_function_with_host(
        &RuntimeScalarFunctionHost,
        name,
        args,
        source,
    )
}

struct RuntimeScalarFunctionHost;
impl reddb_engine::query::runtime::scalar_functions::ScalarFunctionHost
    for RuntimeScalarFunctionHost
{
    fn current_unix_ms(&self) -> i64 {
        reddb_engine::query::runtime::temporal::current_unix_ms()
    }
    fn resolve_argument(&self, argument: &Projection, source: &UnifiedRecord) -> Option<Value> {
        eval_projection_value(argument, source)
    }
    fn evaluate_condition(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        evaluate_runtime_filter(source, filter, None, None)
    }
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        evaluate_native_scalar_function(name, arguments, source)
    }
}

fn evaluate_native_scalar_function(
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    // Strip alias suffix if present (e.g. "GEO_DISTANCE:dist_km" → "GEO_DISTANCE")
    let func_name = name.split(':').next().unwrap_or(name);

    match func_name {
        // H3 hexagonal index scalars (PRD #1574 slice 1, #1575). Mirror
        // of the `expr_eval::dispatch_builtin_function` arms for the
        // legacy projection / WHERE-clause path.
        "H3_INDEX" => {
            let lat = value_as_number(&resolve_scalar_arg(args, 0, source)?)?.as_f64();
            let lon = value_as_number(&resolve_scalar_arg(args, 1, source)?)?.as_f64();
            let res = value_as_i64(&resolve_scalar_arg(args, 2, source)?)?.clamp(0, 15) as u8;
            Some(Value::UnsignedInteger(crate::geo::h3::lat_lng_to_cell(
                lat, lon, res,
            )))
        }
        "H3_CELL" => {
            let Some((lat, lon)) = resolve_geo_arg(args.first()?, source) else {
                return Some(Value::Null);
            };
            let res_value = value_as_i64(&resolve_scalar_arg(args, 1, source)?)?;
            let Some(res) = crate::geo::h3::valid_resolution(res_value) else {
                return Some(Value::Null);
            };
            let cell = crate::geo::h3::lat_lng_to_cell(lat, lon, res);
            if cell == 0 {
                Some(Value::Null)
            } else {
                Some(Value::UnsignedInteger(cell))
            }
        }
        "H3_PARENT" => {
            let cell = value_as_u64(&resolve_scalar_arg(args, 0, source)?)?;
            let res_value = value_as_i64(&resolve_scalar_arg(args, 1, source)?)?;
            let Some(res) = crate::geo::h3::valid_resolution(res_value) else {
                return Some(Value::Null);
            };
            let parent = crate::geo::h3::cell_to_parent(cell, res);
            if parent == 0 {
                Some(Value::Null)
            } else {
                Some(Value::UnsignedInteger(parent))
            }
        }
        "H3_TO_LATLNG" => {
            let cell = value_as_u64(&resolve_scalar_arg(args, 0, source)?)?;
            let (lat, lon) = crate::geo::h3::cell_to_lat_lng(cell);
            Some(Value::Array(vec![Value::Float(lat), Value::Float(lon)]))
        }
        "H3_RING" => {
            let cell = value_as_u64(&resolve_scalar_arg(args, 0, source)?)?;
            let k = value_as_i64(&resolve_scalar_arg(args, 1, source)?)?.max(0) as u32;
            Some(Value::Array(
                crate::geo::h3::grid_disk(cell, k)
                    .into_iter()
                    .map(Value::UnsignedInteger)
                    .collect(),
            ))
        }
        "VERIFY_PASSWORD" => {
            // VERIFY_PASSWORD(column, 'candidate') — compares a
            // plaintext candidate against the argon2id hash stored in
            // a Value::Password column. Returns a boolean.
            let stored = resolve_scalar_arg(args, 0, source)?;
            let candidate = resolve_scalar_arg(args, 1, source)?;
            let hash: String = match stored {
                Value::Password(h) => h,
                Value::Text(h) => h.to_string(),
                _ => return Some(Value::Boolean(false)),
            };
            let plain: String = match candidate {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Boolean(false)),
            };
            Some(Value::Boolean(crate::auth::store::verify_password(
                &plain, &hash,
            )))
        }
        // Session-context scalars — match the `expr_eval` filter-side
        // dispatcher so `SELECT CURRENT_TENANT(), CURRENT_USER, …`
        // (no FROM, scalar projection path) returns the same values
        // RLS policies see in their predicates. Honours `WITHIN …`,
        // `SET LOCAL TENANT`, and `SET TENANT` overrides via the
        // shared accessors.
        "CURRENT_TENANT" => Some(
            crate::runtime::impl_core::current_tenant()
                .map(Value::text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_USER" | "SESSION_USER" | "USER" => Some(
            crate::runtime::impl_core::current_user_projected()
                .map(Value::text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_ROLE" => Some(
            crate::runtime::impl_core::current_role_projected()
                .map(Value::text)
                .unwrap_or(Value::Null),
        ),
        "PG_ADVISORY_LOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            crate::auth::locks::global()
                .acquire(key, crate::runtime::impl_core::current_connection_id());
            Some(Value::Null)
        }
        "PG_TRY_ADVISORY_LOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            Some(Value::Boolean(crate::auth::locks::global().try_acquire(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            Some(Value::Boolean(crate::auth::locks::global().release(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK_ALL" => {
            let dropped = crate::auth::locks::global()
                .release_all(crate::runtime::impl_core::current_connection_id());
            Some(Value::Integer(dropped as i64))
        }
        _ => Some(Value::Null),
    }
}

pub(super) use reddb_engine::query::runtime::numeric::value_as_number;

pub(super) fn resolve_scalar_arg(
    args: &[Projection],
    index: usize,
    source: &UnifiedRecord,
) -> Option<Value> {
    let arg = args.get(index)?;
    eval_projection_value(arg, source)
}
