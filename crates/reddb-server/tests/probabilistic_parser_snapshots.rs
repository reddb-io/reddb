//! Pinned probabilistic-DSL parse-error snapshots (issue #105).
//!
//! Mirrors `geo_parser_snapshots.rs` and `queue_parser_snapshots.rs`
//! for the HLL / SKETCH / FILTER grammars. Each test calls
//! `fmt_parse_error` on a hand-crafted bad input; snapshot files
//! live in `tests/snapshots/`.
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

macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _guard = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- CREATE envelope errors ------------------------------------

snap!(prob_create_hll_no_name, "CREATE HLL");
snap!(prob_create_sketch_no_name, "CREATE SKETCH");
snap!(prob_create_filter_no_name, "CREATE FILTER");
snap!(prob_create_unknown_kind, "CREATE BLOOM b1");

// ----- CREATE FILTER capacity errors -----------------------------

snap!(
    prob_filter_capacity_no_value,
    "CREATE FILTER f1 CAPACITY"
);
snap!(
    prob_filter_capacity_negative,
    "CREATE FILTER f1 CAPACITY -1"
);
snap!(
    prob_filter_capacity_non_numeric,
    "CREATE FILTER f1 CAPACITY many"
);

// ----- CREATE SKETCH width / depth errors ------------------------

snap!(prob_sketch_width_no_value, "CREATE SKETCH s1 WIDTH");
snap!(prob_sketch_width_negative, "CREATE SKETCH s1 WIDTH -1");
// `Token::Depth` short-circuits the modifier loop; the trailing
// integer is then consumed by the top-level dispatcher's "expect
// EOF" check. This snapshot pins the resulting error so the
// upcoming fix (FIXME #105-followup-1) flips it deliberately.
snap!(
    prob_sketch_depth_after_name_breaks_top_level,
    "CREATE SKETCH s1 DEPTH 5"
);

// ----- HLL operational errors ------------------------------------

snap!(prob_hll_eof_after_keyword, "HLL");
snap!(prob_hll_unknown_subcmd, "HLL FROBNICATE x");
snap!(prob_hll_add_no_name, "HLL ADD");
snap!(
    prob_hll_add_unterminated_string,
    "HLL ADD visitors 'open"
);

// ----- SKETCH operational errors ---------------------------------

snap!(prob_sketch_add_no_element, "SKETCH ADD events");
snap!(
    prob_sketch_add_unquoted_element,
    "SKETCH ADD events bareword"
);

// ----- FILTER operational errors ---------------------------------

snap!(prob_filter_eof_after_keyword, "FILTER");
snap!(prob_filter_check_no_element, "FILTER CHECK seen");
snap!(prob_filter_delete_no_element, "FILTER DELETE seen");

// ----- DROP envelope errors --------------------------------------

snap!(prob_drop_unknown_kind, "DROP BLOOM b1");
