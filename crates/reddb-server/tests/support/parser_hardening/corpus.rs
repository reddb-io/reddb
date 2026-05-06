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

/// Adversarial inputs that target the geo / spatial surface (issue
/// #104). These exercise `parse_search_spatial`, RTREE index DDL,
/// and the geo scalar functions in projection position.
///
/// Out-of-range coordinates (lat=91, lon=-181) are included on
/// purpose: the parser does not semantically validate ranges today,
/// so these inputs *parse* (which is fine — the harness only
/// guarantees no panics). Once range validation is added in a future
/// slice the FIXME pin in `tests/geo_parser.rs` will start to fail
/// and force a snapshot refresh.
pub fn geo_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ----- bare / EOF shapes ---------------------------------
        ("geo_eof_after_search_spatial", "SEARCH SPATIAL".to_string()),
        (
            "geo_eof_after_radius",
            "SEARCH SPATIAL RADIUS".to_string(),
        ),
        (
            "geo_eof_after_nearest",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K".to_string(),
        ),
        (
            "geo_eof_after_bbox",
            "SEARCH SPATIAL BBOX 0.0 0.0".to_string(),
        ),
        // ----- out-of-range latitude / longitude -----------------
        (
            "geo_lat_91_out_of_range",
            "SEARCH SPATIAL RADIUS 91.0 0.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_lon_181_out_of_range",
            "SEARCH SPATIAL RADIUS 0.0 181.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_lat_neg91",
            // Note: leading unary `-` does not lex as a float here;
            // it tokenises as Minus + Float(91.0). The parse_float
            // call only accepts Float/Integer, so this *errors*
            // before even reaching the range check. See FIXME pin.
            "SEARCH SPATIAL NEAREST -91.0 0.0 K 5 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_lon_neg181",
            "SEARCH SPATIAL NEAREST 0.0 -181.0 K 5 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- numeric edge cases --------------------------------
        (
            "geo_radius_negative",
            // `parse_float` rejects unary minus; the parser surfaces
            // an "expected number" error at the radius position.
            "SEARCH SPATIAL RADIUS 0.0 0.0 -10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_zero",
            "SEARCH SPATIAL RADIUS 0.0 0.0 0.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_k_zero",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K 0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_k_negative",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K -1 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_nan_literal",
            // `NaN` is not a recognised literal in the lexer; this
            // tokenises as an Ident and `parse_float` errors with
            // "expected number". The harness only asserts no panic.
            "SEARCH SPATIAL RADIUS NaN NaN 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_infinity_literal",
            "SEARCH SPATIAL RADIUS Infinity 0.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- structural malformations --------------------------
        (
            "geo_radius_missing_collection_kw",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 sites COLUMN col".to_string(),
        ),
        (
            "geo_radius_missing_column_kw",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION sites col".to_string(),
        ),
        (
            "geo_nearest_missing_k_kw",
            "SEARCH SPATIAL NEAREST 0.0 0.0 5 COLLECTION sites COLUMN col".to_string(),
        ),
        (
            "geo_unknown_subcommand",
            "SEARCH SPATIAL POLYGON 0.0 0.0 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- RTREE index DDL -----------------------------------
        (
            "geo_rtree_no_columns",
            "CREATE INDEX gix ON sites () USING RTREE".to_string(),
        ),
        (
            "geo_rtree_unknown_method",
            "CREATE INDEX gix ON sites (location) USING WRONGTREE".to_string(),
        ),
        // ----- distance fns --------------------------------------
        (
            "geo_distance_no_args",
            "SELECT GEO_DISTANCE() FROM t".to_string(),
        ),
        (
            "geo_haversine_dangling_comma",
            "SELECT HAVERSINE(0.0, 0.0, 1.0,) FROM t".to_string(),
        ),
        // ----- bulk / DoS shapes ---------------------------------
        (
            "geo_radius_oversized",
            format!(
                "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION {} COLUMN col",
                "c".repeat(10_000),
            ),
        ),
        (
            "geo_nul_byte",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION c COLUMN col\0".to_string(),
        ),
        (
            "geo_garbage_after_radius",
            "SEARCH SPATIAL RADIUS @#$% COLLECTION c COLUMN col".to_string(),
        ),
    ]
}
