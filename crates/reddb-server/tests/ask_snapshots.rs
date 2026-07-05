//! Pinned ASK / SEARCH CONTEXT parse-error snapshots (issue #101).
//!
//! Mirrors `parser_snapshots.rs` and `migration_parser_snapshots.rs`
//! for the ASK / AI-extension surface. Each test in this file calls
//! `fmt_parse_error` on a hand-crafted bad input; the formatted
//! string flows through the shared `secret_redactor` filter chain
//! so provider names + model names + any embedded credentials are
//! masked before insta records the snapshot.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.
//!
//! Phase A constraint (#101): tests-only. Bugs uncovered here are
//! pinned with `FIXME(#101): …` comments next to the `snap!` line so
//! the parser source stays untouched until a follow-up issue lands.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::parser;
use support::parser_hardening::secret_redactor;

// Note on file naming: the more natural filename
// `ask_parser_snapshots.rs` collides with the shared `api-key`
// redactor regex `(sk|rs|reddb)_[A-Za-z0-9_]{16,}` — the substring
// `sk_parser_snapshots` matches as 17 chars after `sk_`. The
// snapshot redaction lint (#98) re-greps every committed `*.snap`
// file with that regex and would fail on the YAML `source:` header
// insta writes at the top of each snapshot. `ask_snapshots.rs` keeps
// the source path below the 16-char trigger (`sk_snapshots` = 9 body
// chars), so the lint stays clean without per-file filter overrides.

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Macro wrapper around `insta::assert_snapshot!`. Every snapshot in
/// this file installs the shared secret redactor (#98) — provider /
/// model names sometimes leak into error paths and the redactor is
/// the line of defense against accidentally pinning a real token
/// into a `*.snap` file.
macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _guard = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- ASK error scenarios ---------------------------------------

snap!(ask_eof_after_keyword, "ASK");
snap!(ask_missing_question, "ASK USING openai");
snap!(
    ask_unterminated_string,
    "ASK 'open question without closing quote"
);
snap!(ask_using_no_provider, "ASK 'q' USING");
snap!(ask_model_no_string, "ASK 'q' MODEL");
// `MODEL` slot is a string literal — `MODEL gpt4` (bare ident)
// must error. The snapshot pins the rejection message.
snap!(ask_model_unquoted_ident, "ASK 'q' MODEL gpt4");
snap!(ask_depth_no_int, "ASK 'q' DEPTH");
snap!(ask_depth_negative, "ASK 'q' DEPTH -1");
snap!(ask_limit_garbage, "ASK 'q' LIMIT @#$%");
snap!(ask_collection_no_ident, "ASK 'q' COLLECTION");
snap!(ask_garbage_after_question, "ASK 'q' @#$%");

// ----- clean-break didactic errors (ADR 0068, #1751) -------------
// `AS RQL` and `EXECUTE` were removed; each rejects with a didactic
// error that names the `PLAN` replacement. The snapshots pin the
// exact messages so a wording regression is caught in review.
snap!(
    ask_as_rql_removed,
    "ASK 'who owns passport FDD-12313?' AS RQL"
);
snap!(ask_execute_removed, "ASK 'list travelers' EXECUTE");
snap!(ask_plan_specified_twice, "ASK 'q' PLAN PLAN");

// ----- SEARCH CONTEXT error scenarios ----------------------------

snap!(search_context_eof, "SEARCH CONTEXT");
snap!(search_context_missing_string, "SEARCH CONTEXT FIELD x");
snap!(search_context_unterminated_string, "SEARCH CONTEXT 'open");
snap!(search_context_field_no_ident, "SEARCH CONTEXT 'q' FIELD");
snap!(
    search_context_collection_no_ident,
    "SEARCH CONTEXT 'q' COLLECTION"
);

// ----- DoS limits surface as structured errors -------------------

#[test]
fn ask_dos_input_too_large_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_input_bytes: 16,
        ..parser::ParserLimits::default()
    };
    let result =
        parser::Parser::with_limits("ASK 'this is too long for the limit' USING openai", limits);
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("ask_dos_input_too_large", formatted);
}

#[test]
fn ask_dos_identifier_too_long_message_is_pinned() {
    let _guard = secret_redactor::install_redactions();
    let limits = parser::ParserLimits {
        max_identifier_chars: 8,
        ..parser::ParserLimits::default()
    };
    // `provider_name_long_long_long` is the user-supplied identifier
    // that exceeds the cap; ASK and USING are keywords.
    let result = parser::Parser::with_limits("ASK 'q' USING provider_name_long_long_long", limits)
        .and_then(|mut p| p.parse());
    let formatted = match result {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("ask_dos_identifier_too_long", formatted);
}
