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

/// Adversarial inputs that target the Queue DSL surface (issue
/// #103). Each entry probes a different parser arm in
/// `parse_create_queue_body` / `parse_queue_command` /
/// `parse_drop_queue_body`. None should panic; all should either
/// parse or return an `Err`.
pub fn queue_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // CREATE QUEUE — invalid MAX_SIZE values.
        ("queue_create_eof_after_keyword", "CREATE QUEUE".to_string()),
        ("queue_create_missing_name", "CREATE QUEUE MAX_SIZE 100".to_string()),
        (
            "queue_create_max_size_negative",
            "CREATE QUEUE q MAX_SIZE -1".to_string(),
        ),
        (
            "queue_create_max_size_zero",
            "CREATE QUEUE q MAX_SIZE 0".to_string(),
        ),
        (
            "queue_create_max_size_non_numeric",
            "CREATE QUEUE q MAX_SIZE forever".to_string(),
        ),
        (
            "queue_create_max_size_eof",
            "CREATE QUEUE q MAX_SIZE".to_string(),
        ),
        (
            "queue_create_max_size_overflow",
            "CREATE QUEUE q MAX_SIZE 99999999999999999999".to_string(),
        ),
        (
            "queue_create_dangling_with",
            "CREATE QUEUE q WITH".to_string(),
        ),
        (
            "queue_create_with_ttl_no_value",
            "CREATE QUEUE q WITH TTL".to_string(),
        ),
        (
            "queue_create_with_dlq_no_name",
            "CREATE QUEUE q WITH DLQ".to_string(),
        ),
        // PUSH — malformed and oversized payloads.
        ("queue_push_eof", "QUEUE PUSH".to_string()),
        ("queue_push_missing_payload", "QUEUE PUSH q".to_string()),
        (
            "queue_push_unterminated_string",
            "QUEUE PUSH q 'no closing".to_string(),
        ),
        (
            "queue_push_unbalanced_json",
            "QUEUE PUSH q {job: 'hello'".to_string(),
        ),
        (
            "queue_push_oversized_string_payload",
            format!("QUEUE PUSH q '{}'", "x".repeat(2 * 1024 * 1024)),
        ),
        (
            "queue_push_oversized_input",
            "QUEUE PUSH q ".to_string() + &"x".repeat(2 * 1024 * 1024),
        ),
        (
            "queue_push_priority_no_value",
            "QUEUE PUSH q 'x' PRIORITY".to_string(),
        ),
        // POP / aliases.
        ("queue_pop_eof", "QUEUE POP".to_string()),
        (
            "queue_pop_count_no_value",
            "QUEUE POP q COUNT".to_string(),
        ),
        // Consumer group syntax.
        ("queue_group_create_eof", "QUEUE GROUP CREATE".to_string()),
        (
            "queue_group_create_missing_group",
            "QUEUE GROUP CREATE q".to_string(),
        ),
        (
            "queue_read_missing_group_keyword",
            "QUEUE READ q workers".to_string(),
        ),
        (
            "queue_read_missing_consumer_name",
            "QUEUE READ q GROUP g CONSUMER".to_string(),
        ),
        (
            "queue_claim_missing_min_idle",
            "QUEUE CLAIM q GROUP g CONSUMER c".to_string(),
        ),
        (
            "queue_claim_min_idle_non_numeric",
            "QUEUE CLAIM q GROUP g CONSUMER c MIN_IDLE forever".to_string(),
        ),
        (
            "queue_ack_missing_message_id",
            "QUEUE ACK q GROUP g".to_string(),
        ),
        (
            "queue_unknown_subcommand",
            "QUEUE FROBNICATE q".to_string(),
        ),
        // Bytes-level adversarial inputs.
        (
            "queue_garbage_after_keyword",
            "QUEUE @#$%".to_string(),
        ),
        (
            "queue_nul_byte",
            "CREATE QUEUE q\0".to_string(),
        ),
        (
            "queue_long_name",
            format!("CREATE QUEUE {}", "q".repeat(10_000)),
        ),
    ]
}
