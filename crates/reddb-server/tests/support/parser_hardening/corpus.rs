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

/// Adversarial inputs that target the vector-search surface (issue
/// #100). These exercise the `parse_search_similar`,
/// `parse_search_hybrid`, `parse_vector_query`, and `WITH AUTO EMBED`
/// branches without overlapping the SQL or migration corpora.
///
/// None of these should panic; all should either parse or return an
/// `Err`. NaN / Infinity / oversized-dim cases test grammar surface
/// only — the parser doesn't reject NaN at parse time today (it
/// stores the f32 verbatim) so several of these inputs return Ok.
/// That's deliberate: the corpus exists to prove the parser doesn't
/// crash, not to enforce a semantic policy.
pub fn vector_search_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ---- malformed vector literals -------------------------
        ("vector_eof_after_lbracket", "SEARCH SIMILAR [".to_string()),
        (
            "vector_unterminated_literal",
            "SEARCH SIMILAR [0.1, 0.2 COLLECTION c".to_string(),
        ),
        (
            "vector_dangling_comma",
            "SEARCH SIMILAR [0.1, 0.2,] COLLECTION c".to_string(),
        ),
        (
            "vector_empty_literal",
            "VECTOR SEARCH e SIMILAR TO [] LIMIT 5".to_string(),
        ),
        (
            "vector_garbage_in_literal",
            "SEARCH SIMILAR [0.1, @#$, 0.3] COLLECTION c".to_string(),
        ),
        // ---- gigantic dims -------------------------------------
        (
            "vector_huge_dim_4096",
            {
                let dims: Vec<String> = (0..4096).map(|i| format!("{:.4}", i as f32 / 4096.0)).collect();
                format!("SEARCH SIMILAR [{}] COLLECTION big LIMIT 5", dims.join(", "))
            },
        ),
        (
            "vector_silly_dim_50000_under_limit",
            // Stays under the 1 MiB max_input_bytes default by
            // virtue of short floats. Targets the vector-literal
            // comma-loop without tripping the size guard.
            {
                let dims: Vec<String> = (0..50_000).map(|_| "0.1".to_string()).collect();
                format!("SEARCH SIMILAR [{}] COLLECTION big", dims.join(","))
            },
        ),
        // ---- NaN / Infinity floats -----------------------------
        // The lexer accepts these as identifiers (NaN / Infinity
        // are not numeric tokens), so the parser bails inside
        // `parse_float`. Pin the no-panic invariant.
        (
            "vector_nan_in_literal",
            "SEARCH SIMILAR [0.1, NaN, 0.3] COLLECTION c".to_string(),
        ),
        (
            "vector_inf_in_literal",
            "SEARCH SIMILAR [0.1, Infinity, 0.3] COLLECTION c".to_string(),
        ),
        (
            "vector_neg_inf_in_literal",
            "SEARCH SIMILAR [-Infinity, 0.0] COLLECTION c".to_string(),
        ),
        (
            "vector_huge_float",
            "SEARCH SIMILAR [1e308, -1e308] COLLECTION c".to_string(),
        ),
        (
            "vector_too_huge_float",
            "SEARCH SIMILAR [1e500] COLLECTION c".to_string(),
        ),
        // ---- structural breakage -------------------------------
        ("similar_eof", "SEARCH SIMILAR".to_string()),
        ("similar_no_collection", "SEARCH SIMILAR [0.1, 0.2]".to_string()),
        (
            "similar_no_collection_name",
            "SEARCH SIMILAR [0.1] COLLECTION".to_string(),
        ),
        (
            "similar_text_no_string",
            "SEARCH SIMILAR TEXT COLLECTION c".to_string(),
        ),
        (
            "similar_unterminated_text",
            "SEARCH SIMILAR TEXT 'unterminated COLLECTION c".to_string(),
        ),
        // ---- AUTO EMBED malformed shapes -----------------------
        (
            "auto_embed_eof_after_keyword",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED".to_string(),
        ),
        (
            "auto_embed_empty_field_list",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED ()".to_string(),
        ),
        (
            "auto_embed_dangling_comma",
            "INSERT INTO t (a, b) VALUES ('x', 'y') WITH AUTO EMBED (a, b,)".to_string(),
        ),
        (
            "auto_embed_using_no_provider",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING".to_string(),
        ),
        (
            "auto_embed_model_no_string",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING openai MODEL".to_string(),
        ),
        // ---- HYBRID FROM malformed -----------------------------
        (
            "hybrid_no_fusion",
            "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] LIMIT 10".to_string(),
        ),
        (
            "hybrid_no_vector_search",
            "HYBRID FROM hosts FUSION RERANK".to_string(),
        ),
        (
            "hybrid_unknown_fusion",
            "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION GLORP LIMIT 10".to_string(),
        ),
        (
            "hybrid_search_neither_set",
            "SEARCH HYBRID COLLECTION c LIMIT 10".to_string(),
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
