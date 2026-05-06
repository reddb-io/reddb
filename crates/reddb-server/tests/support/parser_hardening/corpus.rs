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

/// Adversarial inputs that target the ASK / AI extension surface
/// (issue #101). Exercise `parse_ask_query` and `parse_search_context`
/// on malformed shapes that historically cause parser-loop bugs:
/// missing closing quotes, oversized questions, stray unicode, repeats
/// of the same optional clause, etc.
///
/// Provider / model identifiers in error paths are routed through the
/// shared snapshot redactor when consumed by snapshot tests — none of
/// these fixtures contain a literal secret, but the redaction guard is
/// still installed at every call site as defense in depth (#98).
pub fn ask_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("ask_eof_after_keyword", "ASK".to_string()),
        ("ask_eof_after_question", "ASK 'why?'".to_string()),
        ("ask_missing_question", "ASK USING openai".to_string()),
        (
            "ask_unterminated_string",
            "ASK 'open question without closing quote".to_string(),
        ),
        (
            "ask_using_no_provider",
            "ASK 'q' USING".to_string(),
        ),
        (
            "ask_model_no_string",
            "ASK 'q' MODEL".to_string(),
        ),
        (
            "ask_model_unquoted_ident",
            // MODEL slot expects a string literal, not a bare ident.
            "ASK 'q' MODEL gpt4".to_string(),
        ),
        (
            "ask_depth_no_int",
            "ASK 'q' DEPTH".to_string(),
        ),
        (
            "ask_depth_negative",
            "ASK 'q' DEPTH -1".to_string(),
        ),
        (
            "ask_limit_garbage",
            "ASK 'q' LIMIT @#$%".to_string(),
        ),
        (
            "ask_collection_no_ident",
            "ASK 'q' COLLECTION".to_string(),
        ),
        (
            "ask_more_than_five_clauses",
            // Parser caps the optional-clause loop at 5 iterations;
            // a 6th repeat must round-trip without panic (it parses
            // partially and the trailing tokens become a follow-on
            // statement which the top-level loop rejects).
            "ASK 'q' USING a MODEL 'm' DEPTH 1 LIMIT 2 COLLECTION c USING b".to_string(),
        ),
        (
            "ask_oversized_question",
            format!("ASK '{}'", "x".repeat(100_000)),
        ),
        (
            "ask_unicode_question",
            "ASK '雪花飘落 ❄ ε≈μ' USING openai".to_string(),
        ),
        (
            "ask_zero_width_unicode",
            "ASK '\u{200b}\u{feff}q' USING openai".to_string(),
        ),
        (
            "ask_nul_byte_in_question",
            "ASK 'q\0nul' USING openai".to_string(),
        ),
        (
            "ask_garbage_payload",
            "ASK @#$%".to_string(),
        ),
        // SEARCH CONTEXT adversarial shapes
        (
            "search_context_eof",
            "SEARCH CONTEXT".to_string(),
        ),
        (
            "search_context_missing_string",
            "SEARCH CONTEXT FIELD x".to_string(),
        ),
        (
            "search_context_unterminated_string",
            "SEARCH CONTEXT 'open".to_string(),
        ),
        (
            "search_context_field_no_ident",
            "SEARCH CONTEXT 'q' FIELD".to_string(),
        ),
        (
            "search_context_collection_no_ident",
            "SEARCH CONTEXT 'q' COLLECTION".to_string(),
        ),
        (
            "search_context_oversized_query",
            format!("SEARCH CONTEXT '{}'", "x".repeat(100_000)),
        ),
    ]
}
