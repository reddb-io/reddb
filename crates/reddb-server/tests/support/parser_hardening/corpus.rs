//! Adversarial-input fixtures.
//!
//! Each entry is an `(name, input)` pair. The same corpus is
//! consumed by:
//!   - the panic-safety property tests in
//!     `tests/parser_hardening.rs`
//!   - the fuzz seed corpus loaded by `fuzz_targets/sql_parser.rs`
//!
//! Adding a regression case here automatically extends both
//! safety nets.

/// Adversarial inputs that historically (or theoretically) trip
/// recursion / memory paths. None of these should panic; all
/// should either parse or return an `Err`.
pub fn adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("only_whitespace", "    \n\t  ".to_string()),
        (
            "deep_parens_50",
            format!("SELECT {}1{} FROM t", "(".repeat(50), ")".repeat(50),),
        ),
        (
            "deep_parens_500",
            format!("SELECT {}1{} FROM t", "(".repeat(500), ")".repeat(500),),
        ),
        (
            "deep_not_chain",
            format!("SELECT * FROM t WHERE {} a = 1", "NOT ".repeat(500),),
        ),
        (
            "long_identifier",
            format!("SELECT * FROM {}", "x".repeat(10_000),),
        ),
        ("oversized_input", "a".repeat(2 * 1024 * 1024)),
        ("unbalanced_parens", "SELECT (((1 FROM t".to_string()),
        ("dangling_comma", "SELECT a, b, FROM t".to_string()),
        ("missing_from", "SELECT x WHERE y = 1".to_string()),
        ("eof_mid_stmt", "SELECT * FROM".to_string()),
        ("garbage_bytes", "@#$%^&*()_+|}{:?><".to_string()),
        (
            "invalid_escape_in_string",
            r"SELECT '\\xff' FROM t".to_string(),
        ),
        ("leading_number_ident", "SELECT 1abc FROM t".to_string()),
        ("nul_byte", "SELECT * FROM t\0".to_string()),
        (
            "very_long_string_lit",
            format!("SELECT '{}' FROM t", "x".repeat(100_000),),
        ),
    ]
}

/// Adversarial inputs that target the time-series DSL surface (issue
/// #102). These exercise the `parse_create_timeseries_body`,
/// `parse_create_hypertable_body`, the bare `CHUNK_INTERVAL` literal
/// validator, and the materialized-view envelope continuous
/// aggregates ride through today.
///
/// Every entry must surface as `Ok` *or* a structured `Err` — never a
/// panic. The fuzz seed corpus in `fuzz/corpus/sql_parser/` is
/// derived from this same list.
pub fn timeseries_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // CREATE TIMESERIES surface --------------------------------
        (
            "ts_eof_after_create",
            "CREATE TIMESERIES".to_string(),
        ),
        (
            "ts_eof_after_name",
            "CREATE TIMESERIES m1".to_string(),
        ),
        (
            "ts_retention_no_value",
            "CREATE TIMESERIES m1 RETENTION".to_string(),
        ),
        (
            "ts_retention_negative",
            "CREATE TIMESERIES m1 RETENTION -90 d".to_string(),
        ),
        (
            "ts_retention_unknown_unit",
            "CREATE TIMESERIES m1 RETENTION 90 fortnights".to_string(),
        ),
        (
            "ts_retention_zero",
            "CREATE TIMESERIES m1 RETENTION 0 d".to_string(),
        ),
        (
            "ts_chunk_size_negative",
            "CREATE TIMESERIES m1 CHUNK_SIZE -1".to_string(),
        ),
        (
            "ts_downsample_dangling_comma",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h:5m:avg,".to_string(),
        ),
        (
            "ts_downsample_bad_aggregation_separator",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h-5m-avg".to_string(),
        ),
        // CREATE HYPERTABLE surface --------------------------------
        (
            "ht_eof_after_create",
            "CREATE HYPERTABLE".to_string(),
        ),
        (
            "ht_missing_time_column",
            "CREATE HYPERTABLE metrics CHUNK_INTERVAL '1d'".to_string(),
        ),
        (
            "ht_missing_chunk_interval",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts".to_string(),
        ),
        (
            "ht_chunk_interval_long_form",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1 day'".to_string(),
        ),
        (
            "ht_chunk_interval_negative",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '-1d'".to_string(),
        ),
        (
            "ht_chunk_interval_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1y'".to_string(),
        ),
        (
            "ht_chunk_interval_bare_int",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 86400".to_string(),
        ),
        (
            "ht_ttl_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '1 fortnight'"
                .to_string(),
        ),
        (
            "ht_oversized_body",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' {}",
                "TTL '1d' ".repeat(2_000)
            ),
        ),
        (
            "ht_deep_paren_after_name",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL {}'1d'{}",
                "(".repeat(50),
                ")".repeat(50),
            ),
        ),
        (
            "ht_nul_byte",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'\0".to_string(),
        ),
        // Continuous aggregate envelope ---------------------------
        (
            "cagg_eof_after_view",
            "CREATE MATERIALIZED VIEW".to_string(),
        ),
        (
            "cagg_missing_as",
            "CREATE MATERIALIZED VIEW mv SELECT 1 FROM t".to_string(),
        ),
        (
            "cagg_garbage_body",
            "CREATE MATERIALIZED VIEW mv AS @#$%".to_string(),
        ),
    ]
}

/// Adversarial inputs that target the migration DSL surface (issue
/// #88). These exercise the `parse_create_migration_body`,
/// `parse_apply_migration`, `parse_rollback_migration_after_keyword`,
/// and `parse_explain_migration_after_keyword` entry points.
pub fn migration_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("migration_eof_after_create", "CREATE MIGRATION".to_string()),
        (
            "migration_eof_after_name",
            "CREATE MIGRATION m1".to_string(),
        ),
        (
            "migration_eof_after_depends",
            "CREATE MIGRATION m1 DEPENDS ON".to_string(),
        ),
        (
            "migration_dangling_depends_comma",
            "CREATE MIGRATION m1 DEPENDS ON a, AS CREATE TABLE t (id INTEGER)".to_string(),
        ),
        (
            "migration_apply_eof",
            "APPLY MIGRATION".to_string(),
        ),
        (
            "migration_rollback_eof",
            "ROLLBACK MIGRATION".to_string(),
        ),
        (
            "migration_explain_eof",
            "EXPLAIN MIGRATION".to_string(),
        ),
        (
            "migration_apply_for_no_tenant",
            "APPLY MIGRATION m1 FOR".to_string(),
        ),
        (
            "migration_long_name",
            format!("CREATE MIGRATION {} AS CREATE TABLE t (id INTEGER)", "m".repeat(10_000)),
        ),
        (
            "migration_deep_paren_body",
            format!(
                "CREATE MIGRATION m1 AS SELECT {}1{} FROM t",
                "(".repeat(500),
                ")".repeat(500),
            ),
        ),
        (
            "migration_oversized_body",
            format!("CREATE MIGRATION m1 AS {}", "a".repeat(2 * 1024 * 1024)),
        ),
        (
            "migration_nul_byte",
            "CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER)\0".to_string(),
        ),
        (
            "migration_garbage",
            "CREATE MIGRATION @#$%".to_string(),
        ),
    ]
}
