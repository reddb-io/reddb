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
    // Strip alias suffix if present (e.g. "GEO_DISTANCE:dist_km" → "GEO_DISTANCE")
    let func_name = name.split(':').next().unwrap_or(name);

    match func_name {
        "ADD" | "SUB" | "MUL" | "DIV" | "MOD" => {
            let a = resolve_scalar_arg(args, 0, source)?;
            let b = resolve_scalar_arg(args, 1, source)?;
            Some(arith_binop(func_name, a, b))
        }
        "CONCAT" => {
            let mut out = String::new();
            for idx in 0..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                out.push_str(&value.plain_text());
            }
            Some(Value::text(out))
        }
        "CASE" => {
            // CASE WHEN cond THEN val ... ELSE val END is encoded as
            //   Function("CASE", [Expression(cond1), val1,
            //                      Expression(cond2), val2,
            //                      ..., else_val?])
            // Even-length args => no ELSE; odd-length => last arg is ELSE.
            // Walk WHEN/THEN pairs left-to-right, short-circuit on first
            // matching predicate. Fall through to ELSE (or Null) when no
            // branch matches.
            let mut i = 0;
            while i + 1 < args.len() {
                if let Projection::Expression(filter, _) = &args[i] {
                    let matched = evaluate_runtime_filter(source, filter, None, None);
                    if matched {
                        return resolve_scalar_arg(args, i + 1, source).or(Some(Value::Null));
                    }
                    i += 2;
                } else {
                    break;
                }
            }
            if args.len() % 2 == 1 {
                return resolve_scalar_arg(args, args.len() - 1, source).or(Some(Value::Null));
            }
            Some(Value::Null)
        }
        "CAST" => {
            // CAST(expr AS type) is parsed into Function("CAST", [inner, Column("TYPE:<name>")]).
            // Resolve the source value, look up the target type by SQL name,
            // and reuse the existing string→Value coerce path. On any
            // failure (unknown type, coerce error) we emit Null so queries
            // keep running — CAST is advisory, not a hard assertion.
            let src = resolve_scalar_arg(args, 0, source)?;
            let Some(Projection::Column(col)) = args.get(1) else {
                return Some(Value::Null);
            };
            let Some(type_name) = col.strip_prefix("TYPE:") else {
                return Some(Value::Null);
            };
            let Some(target) = crate::storage::schema::types::DataType::from_sql_name(type_name)
            else {
                return Some(Value::Null);
            };
            Some(cast_value_to(&src, target))
        }
        "GEO_DISTANCE" | "HAVERSINE" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::haversine_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "TIME_BUCKET" => {
            let bucket_ns = resolve_time_bucket_duration(args, 0)?;
            let timestamp_ns = resolve_time_bucket_timestamp(args, source)?;
            let bucket_start = timestamp_ns
                .checked_div(bucket_ns)
                .map(|bucket| bucket * bucket_ns)
                .unwrap_or(timestamp_ns);
            Some(Value::UnsignedInteger(bucket_start))
        }
        "GEO_DISTANCE_VINCENTY" | "VINCENTY" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::vincenty_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_BEARING" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::bearing(lat1, lon1, lat2, lon2)))
        }
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
        "GEO_MIDPOINT" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            let (lat, lon) = crate::geo::midpoint(lat1, lon1, lat2, lon2);
            Some(Value::GeoPoint(
                crate::geo::deg_to_micro(lat),
                crate::geo::deg_to_micro(lon),
            ))
        }
        "UPPER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::text(s.to_uppercase())),
                _ => Some(val),
            }
        }
        "LOWER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::text(s.to_lowercase())),
                _ => Some(val),
            }
        }
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.chars().count() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                Value::Array(a) => Some(Value::Integer(a.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "OCTET_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.len() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "BIT_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer((s.len() * 8) as i64)),
                Value::Blob(b) => Some(Value::Integer((b.len() * 8) as i64)),
                _ => Some(Value::Null),
            }
        }
        "SUBSTRING" | "SUBSTR" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            match resolve_scalar_arg(args, 1, source)? {
                Value::Text(pattern) if func_name == "SUBSTRING" && args.len() == 2 => {
                    Some(match substring_pattern_text(&text, &pattern) {
                        Some(matched) => Value::text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(&start_value)?;
                    let count = args.get(2).and_then(|_| {
                        resolve_scalar_arg(args, 2, source).and_then(|value| value_as_i64(&value))
                    });
                    Some(Value::text(substring_text(&text, start, count)?))
                }
            }
        }
        "POSITION" => {
            let needle = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let haystack = match resolve_scalar_arg(args, 1, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Integer(position_text(&needle, &haystack)))
        }
        "TRIM" | "BTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), true, true)))
        }
        "LTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), true, false)))
        }
        "RTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), false, true)))
        }
        "CONCAT_WS" => {
            let separator: String = match resolve_scalar_arg(args, 0, source)? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text.to_string(),
                other => other.display_string(),
            };
            let mut parts = Vec::new();
            for idx in 1..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                parts.push(value.display_string());
            }
            Some(Value::text(parts.join(&separator)))
        }
        "REVERSE" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::text(text.chars().rev().collect::<String>()))
        }
        "LEFT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::text(slice_left_text(&text, count)))
        }
        "RIGHT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::text(slice_right_text(&text, count)))
        }
        "QUOTE_LITERAL" => match resolve_scalar_arg(args, 0, source)? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::text(quote_literal_text(&text))),
            other => Some(Value::text(quote_literal_text(&other.display_string()))),
        },
        "ABS" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.abs())),
                Value::Integer(n) => Some(Value::Integer(n.abs())),
                _ => Some(Value::Null),
            }
        }
        "ROUND" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.round())),
                other => Some(other),
            }
        }
        "COALESCE" => {
            for (i, _) in args.iter().enumerate() {
                if let Some(val) = resolve_scalar_arg(args, i, source) {
                    if val != Value::Null {
                        return Some(val);
                    }
                }
            }
            Some(Value::Null)
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
        "MONEY" => money_from_scalar_args(args, source),
        "MONEY_ASSET" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { asset_code, .. } => Some(Value::AssetCode(asset_code)),
            _ => Some(Value::Null),
        },
        "MONEY_MINOR" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { minor_units, .. } => Some(Value::BigInt(minor_units)),
            _ => Some(Value::Null),
        },
        "MONEY_SCALE" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { scale, .. } => Some(Value::Integer(i64::from(scale))),
            _ => Some(Value::Null),
        },
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
        "NOW" | "CURRENT_TIMESTAMP" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(Value::TimestampMs(ms))
        }
        "CURRENT_DATE" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(Value::Date((ms / 86_400_000) as i32))
        }
        "CURRENT_TIME" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(Value::Time(ms.rem_euclid(86_400_000) as u32))
        }
        _ => Some(Value::Null),
    }
}

fn money_from_scalar_args(args: &[Projection], source: &UnifiedRecord) -> Option<Value> {
    let input = match args {
        [single] => money_arg_text(resolve_scalar_arg(std::slice::from_ref(single), 0, source)?)?,
        [left, right] => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            format!("{} {}", lhs, rhs)
        }
        _ => return Some(Value::Null),
    };
    match crate::storage::schema::coerce::coerce(
        &input,
        crate::storage::schema::DataType::Money,
        None,
    ) {
        Ok(value) => Some(value),
        Err(_) if args.len() == 2 => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            crate::storage::schema::coerce::coerce(
                &format!("{} {}", lhs, rhs),
                crate::storage::schema::DataType::Money,
                None,
            )
            .ok()
        }
        Err(_) => Some(Value::Null),
    }
}

fn money_arg_text(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.to_string()),
        Value::AssetCode(code) => Some(code),
        Value::Currency(code) => Some(String::from_utf8_lossy(&code).to_string()),
        other => Some(other.display_string()),
    }
}

/// Resolve a single scalar argument from a function's arg list.
/// Evaluate an arithmetic binary operator on two values. Promotes
/// heterogeneous numeric operands to Float when either side is Float;
/// preserves Integer when both sides are Integer. Non-numeric operands
/// and zero divisors collapse to `Value::Null` so queries keep running
/// — SQL-style "erroring on bad arithmetic" is the job of the type
/// system v2 (Fase 3), not Fase 1.3.
fn arith_binop(op: &str, a: Value, b: Value) -> Value {
    if let Some(value) = timestamp_ms_arith(op, &a, &b) {
        return value;
    }

    let (lhs, rhs) = match (value_as_number(&a), value_as_number(&b)) {
        (Some(l), Some(r)) => (l, r),
        _ => return Value::Null,
    };
    // Integer fast path when both operands are integers and the op
    // doesn't force a float (division always floats for predictability
    // — avoids surprising truncation).
    let force_float = matches!(op, "DIV") || lhs.is_float || rhs.is_float;
    let out = match op {
        "ADD" => lhs.as_f64() + rhs.as_f64(),
        "SUB" => lhs.as_f64() - rhs.as_f64(),
        "MUL" => lhs.as_f64() * rhs.as_f64(),
        "DIV" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() / rhs.as_f64()
        }
        "MOD" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() % rhs.as_f64()
        }
        _ => return Value::Null,
    };
    if force_float {
        Value::Float(out)
    } else {
        Value::Integer(out as i64)
    }
}

fn timestamp_ms_arith(op: &str, a: &Value, b: &Value) -> Option<Value> {
    match (op, a, b) {
        ("ADD", Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(rhs)?)?,
        )),
        ("ADD", lhs, Value::TimestampMs(ts)) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(lhs)?)?,
        )),
        ("SUB", Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_sub(duration_ms_operand(rhs)?)?,
        )),
        _ => None,
    }
}

fn duration_ms_operand(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) | Value::Duration(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct NumOperand {
    pub(super) int_val: i64,
    pub(super) float_val: f64,
    pub(super) is_float: bool,
}

impl NumOperand {
    pub(super) fn as_f64(self) -> f64 {
        if self.is_float {
            self.float_val
        } else {
            self.int_val as f64
        }
    }
}

pub(super) fn value_as_number(v: &Value) -> Option<NumOperand> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some(NumOperand {
            int_val: *n,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::UnsignedInteger(n) => Some(NumOperand {
            int_val: *n as i64,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::Float(f) => Some(NumOperand {
            int_val: *f as i64,
            float_val: *f,
            is_float: true,
        }),
        Value::Decimal(d) => Some(NumOperand {
            int_val: (*d / 10_000),
            float_val: *d as f64 / 10_000.0,
            is_float: true,
        }),
        Value::Text(s) => {
            if let Ok(n) = s.parse::<i64>() {
                Some(NumOperand {
                    int_val: n,
                    float_val: n as f64,
                    is_float: false,
                })
            } else if let Ok(f) = s.parse::<f64>() {
                Some(NumOperand {
                    int_val: f as i64,
                    float_val: f,
                    is_float: true,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert a `Value` to a new `Value` of the requested `DataType`. Used
/// by the CAST scalar function. Covers the common numeric/text/boolean
/// paths directly (so `CAST(123 AS TEXT)` doesn't round-trip through the
/// schema coercion layer) and falls back to `schema::coerce::coerce`
/// on the value's `display_string()` for everything else — that reuses
/// the battle-tested input validators we already have for INSERT.
fn cast_value_to(src: &Value, target: crate::storage::schema::types::DataType) -> Value {
    use crate::storage::schema::types::DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::text(v.display_string()),
        (Value::Integer(n), DT::Float) => Value::Float(*n as f64),
        (Value::Integer(n), DT::BigInt) => Value::BigInt(*n),
        (Value::Integer(n), DT::UnsignedInteger) if *n >= 0 => Value::UnsignedInteger(*n as u64),
        (Value::UnsignedInteger(n), DT::Integer) if *n <= i64::MAX as u64 => {
            Value::Integer(*n as i64)
        }
        (Value::UnsignedInteger(n), DT::Float) => Value::Float(*n as f64),
        (Value::Float(f), DT::Integer) => Value::Integer(*f as i64),
        (Value::Float(f), DT::UnsignedInteger) if *f >= 0.0 => Value::UnsignedInteger(*f as u64),
        (Value::Boolean(b), DT::Integer) => Value::Integer(if *b { 1 } else { 0 }),
        (Value::Integer(n), DT::Boolean) => Value::Boolean(*n != 0),
        (Value::Text(s), target) => match crate::storage::schema::coerce::coerce(s, target, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
        (v, target) => {
            match crate::storage::schema::coerce::coerce(&v.display_string(), target, None) {
                Ok(v) => v,
                Err(_) => Value::Null,
            }
        }
    }
}

pub(super) fn resolve_scalar_arg(
    args: &[Projection],
    index: usize,
    source: &UnifiedRecord,
) -> Option<Value> {
    let arg = args.get(index)?;
    eval_projection_value(arg, source)
}

/// Parse the `@RL:` sentinel payload written by
/// `sql_lowering::serialize_value_json`. Handles arrays `[...]`, vectors
/// `V[...]`, and scalar fallbacks. Not a full JSON parser — only
/// covers the shapes the serializer emits.
pub(super) fn parse_rl_literal(payload: &str) -> Option<Value> {
    let trimmed = payload.trim();
    if let Some(inner) = trimmed.strip_prefix("V[").and_then(|s| s.strip_suffix(']')) {
        let mut out = Vec::new();
        if !inner.trim().is_empty() {
            for part in inner.split(',') {
                out.push(part.trim().parse::<f32>().ok()?);
            }
        }
        return Some(Value::Vector(out));
    }
    if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut out = Vec::new();
        if !inner.trim().is_empty() {
            for part in split_top_level(inner) {
                out.push(parse_rl_atom(part.trim())?);
            }
        }
        return Some(Value::Array(out));
    }
    parse_rl_atom(trimmed)
}

fn parse_rl_atom(s: &str) -> Option<Value> {
    if s == "null" {
        return Some(Value::Null);
    }
    if s == "true" {
        return Some(Value::Boolean(true));
    }
    if s == "false" {
        return Some(Value::Boolean(false));
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(Value::text(
            inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        ));
    }
    if s.starts_with('[') || s.starts_with("V[") {
        return parse_rl_literal(s);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(Value::Integer(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    None
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        out.push(&s[start..]);
    }
    out
}
