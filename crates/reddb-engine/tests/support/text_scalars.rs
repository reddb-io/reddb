//! The native text algorithms execute unchanged in native and WASM fixtures.
use reddb_engine::query::runtime::text::*;

pub const CASES: &[(&str, fn())] = &[
    (
        "text_scalar_unicode_and_wide_bounds",
        unicode_and_wide_bounds,
    ),
    (
        "text_scalar_regex_capture_and_limits",
        regex_capture_and_limits,
    ),
    (
        "text_scalar_trim_position_and_quote",
        trim_position_and_quote,
    ),
];

fn unicode_and_wide_bounds() {
    let text = "aé🦀z";
    for (start, count, expected) in [
        (1, None, Some(text)),
        (2, Some(2), Some("é🦀")),
        (0, Some(2), Some("aé")),
        (i64::MIN, Some(1), Some("a")),
        (2, Some(0), Some("")),
        (2, Some(-1), None),
        (5, None, Some("")),
        (i64::MAX, None, Some("")),
        (4_294_967_297, None, Some("")),
        (2, Some(4_294_967_296), Some("é🦀z")),
        (2, Some(i64::MAX), Some("é🦀z")),
    ] {
        assert_eq!(
            substring_text(text, start, count).as_deref(),
            expected,
            "{start} {count:?}"
        );
    }
    for (count, left, right) in [
        (0, "", ""),
        (2, "aé", "🦀z"),
        (-1, "aé🦀", "é🦀z"),
        (-4, "", ""),
        (-4_294_967_296, "", ""),
        (i64::MIN, "", ""),
        (i64::MAX, text, text),
    ] {
        assert_eq!(slice_left_text(text, count), left, "LEFT {count}");
        assert_eq!(slice_right_text(text, count), right, "RIGHT {count}");
    }
}

fn regex_capture_and_limits() {
    for (text, pattern, expected) in [
        ("é42z", "[0-9]+", Some("42")),
        ("é42z", "é([0-9]+)z", Some("42")),
        ("42z", "([0-9]+)(z)", Some("42")),
        ("b", "(a)?b", None), // First capture is absent; do not substitute the whole match.
        ("é42z", "[A-Z]+", None),
        ("abc", "[", None),
        ("é🦀", "(é🦀)", Some("é🦀")),
        ("a", "", Some("")),
    ] {
        assert_eq!(
            substring_pattern_text(text, pattern).as_deref(),
            expected,
            "{pattern}"
        );
    }
    let at_limit = format!("{}a{}", "(".repeat(250), ")".repeat(250));
    assert_eq!(substring_pattern_text("a", &at_limit), Some("a".into()));
    let beyond_limit = format!("({at_limit})");
    assert_eq!(substring_pattern_text("a", &beyond_limit), None);
}

fn trim_position_and_quote() {
    assert_eq!(position_text("🦀", "aé🦀z"), 3);
    assert_eq!(position_text("", "aé"), 1);
    assert_eq!(position_text("missing", "aé"), 0);
    assert_eq!(trim_text("\u{2003}é ", None, true, true), "é");
    assert_eq!(trim_text("xyéyx", Some("xy"), true, true), "é");
    assert_eq!(trim_text("xyéyx", Some("xy"), true, false), "éyx");
    assert_eq!(trim_text("xyéyx", Some("xy"), false, true), "xyé");
    assert_eq!(trim_text("xyéyx", Some(""), true, true), "xyéyx");
    assert_eq!(quote_literal_text("a'b"), "'a''b'");
    assert_eq!(quote_literal_text(r"a\b"), r"E'a\\b'");
}
