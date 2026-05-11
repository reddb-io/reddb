//! Pinned migration-DSL parse-error snapshots (issue #88).
//!
//! Mirrors `parser_snapshots.rs` for the migration grammar. Each
//! test in this file calls `assert_parse_error_snapshot` on a hand-
//! crafted bad input; snapshot files live in `tests/snapshots/`.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.

use reddb_server::storage::query::parser;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- CREATE MIGRATION error scenarios --------------------------

snap!(create_migration_eof_after_keyword, "CREATE MIGRATION");
snap!(
    create_migration_missing_name,
    "CREATE MIGRATION DEPENDS ON x"
);
snap!(
    create_migration_eof_after_depends_on,
    "CREATE MIGRATION m1 DEPENDS ON"
);
snap!(
    create_migration_dangling_depends_comma,
    "CREATE MIGRATION m1 DEPENDS ON a, AS CREATE TABLE t (id INTEGER)"
);
snap!(
    create_migration_bad_column_type,
    "CREATE MIGRATION m1 AS CREATE TABLE t (id NOTATYPE)"
);
snap!(
    create_migration_dangling_column_comma,
    "CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER,)"
);
snap!(
    create_migration_unbalanced_paren_body,
    "CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER"
);
snap!(
    create_migration_garbage_after_name,
    "CREATE MIGRATION m1 @#$%"
);
snap!(create_migration_only_keyword, "CREATE");
snap!(
    create_migration_reserved_word_as_name,
    "CREATE MIGRATION SELECT AS CREATE TABLE t (id INTEGER)"
);

// ----- APPLY MIGRATION error scenarios ---------------------------

snap!(apply_migration_eof, "APPLY MIGRATION");
snap!(apply_migration_missing_keyword, "APPLY m1");
snap!(apply_migration_for_no_tenant, "APPLY MIGRATION m1 FOR");
snap!(
    apply_migration_for_tenant_no_id,
    "APPLY MIGRATION m1 FOR TENANT"
);
snap!(apply_migration_garbage, "APPLY MIGRATION @#$%");

// ----- ROLLBACK MIGRATION error scenarios ------------------------

snap!(rollback_migration_eof, "ROLLBACK MIGRATION");
snap!(rollback_migration_missing_name_eof, "ROLLBACK MIGRATION ");
snap!(rollback_migration_garbage_name, "ROLLBACK MIGRATION @#$%");

// ----- EXPLAIN MIGRATION error scenarios -------------------------

snap!(explain_migration_eof, "EXPLAIN MIGRATION");
snap!(explain_migration_missing_keyword, "EXPLAIN MIGRATION 12345");

// ----- DoS limits surface as structured errors -------------------

#[test]
fn migration_dos_input_too_large_message_is_pinned() {
    let limits = parser::ParserLimits {
        max_input_bytes: 16,
        ..parser::ParserLimits::default()
    };
    let result =
        parser::Parser::with_limits("CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER)", limits);
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("migration_dos_input_too_large", formatted);
}

#[test]
fn migration_dos_identifier_too_long_message_is_pinned() {
    let limits = parser::ParserLimits {
        max_identifier_chars: 8,
        ..parser::ParserLimits::default()
    };
    // "CREATE" + "MIGRATION" are keywords; the user-supplied
    // identifier `migration_name_long` exceeds the cap.
    let result =
        parser::Parser::with_limits("CREATE MIGRATION migration_name_long_long_long", limits)
            .and_then(|mut p| p.parse());
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("migration_dos_identifier_too_long", formatted);
}

#[test]
fn migration_dos_depth_limit_message_is_pinned() {
    let limits = parser::ParserLimits {
        max_depth: 4,
        ..parser::ParserLimits::default()
    };
    let mut p =
        parser::Parser::with_limits("CREATE MIGRATION m1 AS SELECT (((((1))))) FROM t", limits)
            .expect("ctor ok");
    let formatted = match p.parse() {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("migration_dos_depth_limit", formatted);
}
