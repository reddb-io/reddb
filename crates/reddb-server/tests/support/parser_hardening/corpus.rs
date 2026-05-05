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
        ("deep_parens_50", format!(
            "SELECT {}1{} FROM t",
            "(".repeat(50),
            ")".repeat(50),
        )),
        ("deep_parens_500", format!(
            "SELECT {}1{} FROM t",
            "(".repeat(500),
            ")".repeat(500),
        )),
        ("deep_not_chain", format!(
            "SELECT * FROM t WHERE {} a = 1",
            "NOT ".repeat(500),
        )),
        ("long_identifier", format!(
            "SELECT * FROM {}",
            "x".repeat(10_000),
        )),
        ("oversized_input", "a".repeat(2 * 1024 * 1024)),
        ("unbalanced_parens", "SELECT (((1 FROM t".to_string()),
        ("dangling_comma", "SELECT a, b, FROM t".to_string()),
        ("missing_from", "SELECT x WHERE y = 1".to_string()),
        ("eof_mid_stmt", "SELECT * FROM".to_string()),
        ("garbage_bytes", "@#$%^&*()_+|}{:?><".to_string()),
        ("invalid_escape_in_string", r"SELECT '\\xff' FROM t".to_string()),
        ("leading_number_ident", "SELECT 1abc FROM t".to_string()),
        ("nul_byte", "SELECT * FROM t\0".to_string()),
        ("very_long_string_lit", format!(
            "SELECT '{}' FROM t",
            "x".repeat(100_000),
        )),
    ]
}
