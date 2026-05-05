//! Pinned parse-error message snapshots (issue #87).
//!
//! Each test in this file calls `assert_parse_error_snapshot` on
//! a hand-crafted bad input. The snapshot files live in
//! `tests/snapshots/` and are committed alongside the source.
//! Future grammar tweaks that change error wording produce a
//! snapshot diff in CI instead of a silent regression.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review` shows pending
//!     snapshot diffs.
//!   - CI: snapshots must match exactly.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::parser::{self, ParseError};

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses are formatted as `UNEXPECTED OK` so a missing
/// error path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!(
            "input: {:?}\nkind:  {:?}\nerror: {}\n",
            input, e.kind, e
        ),
    }
}

/// Macro wrapper around `insta::assert_snapshot!` that names the
/// snapshot after the test function and pins the error format.
macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// Use the test that exists to make rust-analyzer / linter happy.
#[allow(dead_code)]
fn _unused_warn_silencer() {
    let _ = ParseError::new("", reddb_server::storage::query::lexer::Position::new(1, 1, 0));
}

// ----- 30+ pinned error scenarios --------------------------------

snap!(eof_after_select, "SELECT");
snap!(eof_after_from, "SELECT * FROM");
snap!(eof_mid_where, "SELECT * FROM t WHERE");
snap!(eof_after_insert_into, "INSERT INTO");
snap!(eof_after_update_set, "UPDATE t SET");

snap!(unbalanced_lparen, "SELECT (((1 FROM t");
snap!(unbalanced_rparen, "SELECT 1)) FROM t");
snap!(unbalanced_brackets, "SELECT a[1 FROM t");

snap!(dangling_comma_select, "SELECT a, b, FROM t");
snap!(dangling_comma_insert_cols, "INSERT INTO t (a, b,) VALUES (1, 2)");
snap!(dangling_comma_insert_values, "INSERT INTO t (a, b) VALUES (1, 2,)");
snap!(dangling_comma_orderby, "SELECT * FROM t ORDER BY a,");

snap!(missing_from_keyword, "SELECT a, b WHERE x = 1");
snap!(missing_set_keyword, "UPDATE t a = 1");
snap!(missing_into_keyword, "INSERT t (a) VALUES (1)");
snap!(missing_values_keyword, "INSERT INTO t (a) (1)");

snap!(reserved_word_as_table, "SELECT * FROM SELECT");
snap!(reserved_word_as_column, "SELECT FROM FROM t");
snap!(reserved_word_as_alias_target, "SELECT a AS WHERE FROM t");

snap!(leading_number_in_ident, "SELECT 1abc FROM t");
snap!(bare_garbage_punct, "@#$%^&*()_+|}{:?><");
snap!(unterminated_string, "SELECT 'unterminated FROM t");
snap!(only_whitespace, "    \n\t  ");
snap!(empty_input, "");

snap!(invalid_operator_chain, "SELECT * FROM t WHERE x === 1");
snap!(double_where, "SELECT * FROM t WHERE x = 1 WHERE y = 2");
snap!(limit_without_value, "SELECT * FROM t LIMIT");
snap!(limit_negative_text, "SELECT * FROM t LIMIT abc");

snap!(orderby_without_column, "SELECT * FROM t ORDER BY");
snap!(groupby_without_column, "SELECT * FROM t GROUP BY");

snap!(insert_arity_mismatch, "INSERT INTO t (a, b) VALUES (1)");
snap!(update_missing_target, "UPDATE SET a = 1");
snap!(delete_missing_table, "DELETE FROM");

// ----- DoS limits surface as structured errors -------------------

#[test]
fn dos_input_too_large_message_is_pinned() {
    let limits = parser::ParserLimits {
        max_input_bytes: 16,
        ..parser::ParserLimits::default()
    };
    let result = parser::Parser::with_limits("SELECT * FROM very_long_input", limits);
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_input_too_large", formatted);
}

#[test]
fn dos_identifier_too_long_message_is_pinned() {
    // Cap = 8: SELECT (6) and FROM (4) clear the cap, but the
    // user-supplied table name `userstable_long_ident` does not.
    let limits = parser::ParserLimits {
        max_identifier_chars: 8,
        ..parser::ParserLimits::default()
    };
    let result = parser::Parser::with_limits(
        "SELECT * FROM userstable_long_ident",
        limits,
    )
    .and_then(|mut p| p.parse());
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_identifier_too_long", formatted);
}

#[test]
fn dos_depth_limit_message_is_pinned() {
    let limits = parser::ParserLimits {
        max_depth: 4,
        ..parser::ParserLimits::default()
    };
    let mut p = parser::Parser::with_limits(
        "SELECT (((((1))))) FROM t",
        limits,
    )
    .expect("ctor ok");
    let formatted = match p.parse() {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_depth_limit", formatted);
}
