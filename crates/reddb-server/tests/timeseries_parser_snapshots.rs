//! Pinned time-series-DSL parse-error snapshots (issue #102).
//!
//! Mirrors `migration_parser_snapshots.rs` for the time-series
//! grammar. Each test calls `assert_parse_error_snapshot` on a hand-
//! crafted bad input; snapshot files live in `tests/snapshots/`.
//!
//! Per #98, every snapshot in this file installs the shared secret-
//! redactor guard so credential-shaped substrings are masked before
//! `insta` diffs the snapshot. This is enforced both here and by the
//! `snapshot_redaction_lint.rs` integration test (which re-greps every
//! committed `*.snap` file with the same patterns).
//!
//! Phase A: tests-only. Inputs that surface latent grammar gaps are
//! pinned with a `// FIXME(#NN)` marker pointing at the follow-up
//! issue — no parser source mods land in this slice.
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

/// Macro wrapper around `insta::assert_snapshot!` that:
///   1. installs the shared secret-redactor guard so every snapshot in
///      this file inherits the four-pattern filter chain (`#98`),
///   2. names the snapshot after the test function,
///   3. pins the formatted parse-error shape.
macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _guard = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- CREATE TIMESERIES error scenarios -------------------------

snap!(create_timeseries_eof_after_keyword, "CREATE TIMESERIES");
snap!(create_timeseries_missing_name, "CREATE TIMESERIES RETENTION 90 d");
snap!(
    create_timeseries_retention_no_value,
    "CREATE TIMESERIES m1 RETENTION"
);

// `parse_float` rejects the leading `-` outright (the unary-minus
// operator is not consumed inside the duration slot), so the user
// sees a clear "expected: number" error. Pin that wording so a
// future grammar tweak that quietly accepts `-90 d` (which would
// underflow when cast to `u64`) shows up as a snapshot diff.
snap!(
    create_timeseries_retention_negative_value,
    "CREATE TIMESERIES m1 RETENTION -90 d"
);

snap!(
    create_timeseries_retention_unknown_unit,
    "CREATE TIMESERIES m1 RETENTION 90 fortnights"
);

// `parse_duration_unit` now recognises `Token::Min` as the minute
// alias, so `RETENTION 1 min` parses cleanly. The snapshot still
// records the parse outcome (now `UNEXPECTED OK`) so any future
// regression that re-introduces the silent-default branch shows up
// as a snapshot diff.
snap!(
    create_timeseries_retention_min_unit_parses,
    "CREATE TIMESERIES m1 RETENTION 1 min"
);
snap!(
    create_timeseries_downsample_dangling_comma,
    "CREATE TIMESERIES m1 DOWNSAMPLE 1h:5m:avg,"
);

// ----- CREATE HYPERTABLE error scenarios -------------------------

snap!(create_hypertable_eof_after_keyword, "CREATE HYPERTABLE");
snap!(
    create_hypertable_missing_time_column,
    "CREATE HYPERTABLE metrics CHUNK_INTERVAL '1d'"
);
snap!(
    create_hypertable_missing_chunk_interval,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts"
);

// `parse_duration_ns` now accepts the long-form duration literal
// (`'1 day'`, `'2 hours'`) alongside the compact suffix form. The
// snapshot pins the resulting `UNEXPECTED OK` so any future
// regression that drops the long-form path shows up as a diff.
snap!(
    create_hypertable_chunk_interval_long_form_parses,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1 day'"
);

snap!(
    create_hypertable_chunk_interval_negative,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '-1d'"
);
snap!(
    create_hypertable_chunk_interval_bare_int,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 86400"
);
snap!(
    create_hypertable_chunk_interval_unknown_unit,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1y'"
);
snap!(
    create_hypertable_ttl_unknown_unit,
    "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '1 fortnight'"
);

// ----- DROP TIMESERIES error scenarios ---------------------------

snap!(drop_timeseries_eof, "DROP TIMESERIES");

// ----- Continuous-aggregate envelope -----------------------------

snap!(continuous_aggregate_eof_after_view, "CREATE MATERIALIZED VIEW");
snap!(
    continuous_aggregate_missing_as,
    "CREATE MATERIALIZED VIEW mv SELECT 1 FROM t"
);

// ----- DoS limits surface as structured errors -------------------

#[test]
fn timeseries_dos_input_too_large_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_input_bytes: 16,
        ..parser::ParserLimits::default()
    };
    let result = parser::Parser::with_limits(
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'",
        limits,
    );
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("timeseries_dos_input_too_large", formatted);
}

#[test]
fn timeseries_dos_identifier_too_long_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_identifier_chars: 8,
        ..parser::ParserLimits::default()
    };
    let result = parser::Parser::with_limits(
        "CREATE TIMESERIES timeseries_name_long_long_long RETENTION 90 d",
        limits,
    )
    .and_then(|mut p| p.parse());
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("timeseries_dos_identifier_too_long", formatted);
}
