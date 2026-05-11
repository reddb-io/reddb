//! Pinned queue-DSL parse-error snapshots (issue #103).
//!
//! Mirrors `migration_parser_snapshots.rs` for the queue grammar.
//! Each test calls `assert_parse_error_snapshot` on a hand-crafted
//! bad input; snapshot files live in `tests/snapshots/`.
//!
//! Every snapshot installs the shared `secret_redactor` filter chain
//! from #98 before calling `insta::assert_snapshot!` so any
//! credential-shaped substring an error message echoes is masked
//! before insta computes the diff. The matching
//! `snapshot_redaction_lint.rs` integration test re-greps the
//! committed `*.snap` files with the same patterns and fails CI if
//! one slips through.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::parser;
use support::parser_hardening::secret_redactor;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Parse `input` under explicit limits and format the resulting
/// error. Used by the DoS-limit snapshots below.
fn fmt_parse_error_with_limits(input: &str, limits: parser::ParserLimits) -> String {
    let result = parser::Parser::with_limits(input, limits).and_then(|mut p| p.parse());
    match result {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    }
}

macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _guard = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- CREATE QUEUE error scenarios -----------------------------

snap!(create_queue_eof_after_keyword, "CREATE QUEUE");
snap!(
    create_queue_missing_name_max_size,
    "CREATE QUEUE MAX_SIZE 100"
);
snap!(create_queue_max_size_eof, "CREATE QUEUE q MAX_SIZE");
snap!(
    create_queue_max_size_non_numeric,
    "CREATE QUEUE q MAX_SIZE forever"
);
// `MAX_SIZE 0` is now rejected by the parser via
// `ValueOutOfRange` (issue #115). The snapshot pins the structured
// error message so future tightening lands as a reviewable diff.
snap!(
    create_queue_max_size_zero_rejected,
    "CREATE QUEUE q MAX_SIZE 0"
);
snap!(create_queue_with_ttl_no_value, "CREATE QUEUE q WITH TTL");
snap!(create_queue_with_dlq_no_name, "CREATE QUEUE q WITH DLQ");
snap!(create_queue_garbage_after_name, "CREATE QUEUE q @#$%");

// ----- QUEUE PUSH error scenarios -------------------------------

snap!(queue_push_eof, "QUEUE PUSH");
snap!(queue_push_missing_payload, "QUEUE PUSH q");
snap!(queue_push_unterminated_string, "QUEUE PUSH q 'no closing");
snap!(queue_push_priority_no_value, "QUEUE PUSH q 'x' PRIORITY");

// ----- QUEUE POP error scenarios --------------------------------

snap!(queue_pop_eof, "QUEUE POP");
snap!(queue_pop_count_no_value, "QUEUE POP q COUNT");

// ----- consumer-group error scenarios ---------------------------

snap!(queue_group_create_eof, "QUEUE GROUP CREATE");
snap!(queue_read_missing_group_keyword, "QUEUE READ q workers");
snap!(
    queue_claim_missing_min_idle,
    "QUEUE CLAIM q GROUP g CONSUMER c"
);
snap!(queue_unknown_subcommand, "QUEUE FROBNICATE q");

// ----- DoS limits surface as structured errors ------------------

#[test]
fn queue_dos_input_too_large_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_input_bytes: 16,
        ..parser::ParserLimits::default()
    };
    let formatted =
        fmt_parse_error_with_limits("CREATE QUEUE tasks MAX_SIZE 1000 PRIORITY", limits);
    insta::assert_snapshot!("queue_dos_input_too_large", formatted);
}

#[test]
fn queue_dos_identifier_too_long_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_identifier_chars: 8,
        ..parser::ParserLimits::default()
    };
    // The user-supplied queue name exceeds the cap.
    let formatted = fmt_parse_error_with_limits("CREATE QUEUE queue_name_long_long", limits);
    insta::assert_snapshot!("queue_dos_identifier_too_long", formatted);
}
