//! Pinned `ParseError` message snapshots for the connection-string
//! parser (issue #90). Mirrors `reddb-server/tests/parser_snapshots.rs`
//! shipped in #87 so future grammar tweaks that change error wording
//! produce a snapshot diff in CI instead of a silent regression.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review` shows pending diffs.
//!   - CI: snapshots must match exactly.

use reddb_wire::{parse, parse_with_limits, ConnStringLimits};

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses are formatted as `UNEXPECTED OK` so a missing
/// error path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parse(input) {
        Ok(t) => format!("UNEXPECTED OK\ninput: {:?}\nparsed: {:?}\n", input, t),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Snapshot helper that names the snapshot after the test fn.
macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- Empty / missing scheme ------------------------------------

snap!(empty_input, "");
snap!(only_whitespace, "    \n\t  ");
snap!(missing_scheme, "primary.svc:5050");
snap!(only_question_mark, "?route=primary");

// ----- UnsupportedScheme -----------------------------------------

snap!(unknown_scheme_mongodb, "mongodb://localhost");
snap!(unknown_scheme_uppercase, "MONGODB://localhost");
snap!(scheme_with_plus, "red+tls://host");

// ----- InvalidUri: structural ------------------------------------

snap!(file_no_path, "file://");
snap!(red_no_host, "red://");
snap!(reds_no_host, "reds://");
snap!(grpc_no_host, "grpc://");
snap!(http_no_host, "http://");

// ----- InvalidUri: ports -----------------------------------------

snap!(port_non_numeric, "red://host:nope");
snap!(port_overflow, "red://host:99999");
snap!(port_negative, "red://host:-1");
snap!(port_empty, "red://host:");

// ----- InvalidUri: cluster ---------------------------------------

snap!(cluster_empty_middle, "grpc://primary,,replica");
snap!(cluster_empty_first, "grpc://,b");
snap!(cluster_only_commas, "grpc://,,,");
snap!(cluster_trailing_comma, "grpc://a,");
snap!(cluster_non_numeric_port, "grpc://a:nope,b:5055");
snap!(cluster_ipv6_unterminated, "grpc://[::1:5050,b:5055");

// ----- InvalidUri: BIDI / control / weird codepoints -------------

snap!(bom_prefix, "\u{feff}red://host:5050");
snap!(rtl_override_in_host, "red://h\u{202e}ost:5050");
snap!(nul_byte_in_host, "red://host\0:5050");
snap!(control_char_in_host, "red://h\u{0001}:5050");

// ----- LimitExceeded variants ------------------------------------

#[test]
fn dos_uri_too_long() {
    let limits = ConnStringLimits {
        max_uri_bytes: 16,
        ..ConnStringLimits::default()
    };
    let r = parse_with_limits("red://very-long-host-name:5050", limits);
    let formatted = match r {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_uri_too_long", formatted);
}

#[test]
fn dos_too_many_query_params() {
    let limits = ConnStringLimits {
        max_query_params: 2,
        ..ConnStringLimits::default()
    };
    let r = parse_with_limits("red://h?a=1&b=2&c=3&d=4", limits);
    let formatted = match r {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_too_many_query_params", formatted);
}

#[test]
fn dos_too_many_cluster_hosts() {
    let limits = ConnStringLimits {
        max_cluster_hosts: 2,
        ..ConnStringLimits::default()
    };
    let r = parse_with_limits("grpc://a,b,c,d", limits);
    let formatted = match r {
        Ok(_) => "UNEXPECTED OK".to_string(),
        Err(e) => format!("kind:  {:?}\nerror: {}\n", e.kind, e),
    };
    insta::assert_snapshot!("dos_too_many_cluster_hosts", formatted);
}
