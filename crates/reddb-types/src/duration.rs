//! Duration-string parsing — the leaf `parse_duration_ns` helper.
//!
//! Re-homed from `reddb-server`'s `storage::timeseries::retention`
//! (ADR 0053): the function depends only on `std`, so it belongs in the
//! neutral keystone crate where the RQL parser front-end can reach it
//! without a back-edge to the server engine. The server keeps a
//! re-export shim so existing call-sites stay untouched.

/// Parse a duration string into nanoseconds.
///
/// Accepts both the compact suffix form (`"5m"`, `"1h"`, `"30s"`) and
/// the long, TimescaleDB-compatible form (`"1 day"`, `"2 hours"`,
/// `"30 minutes"`, `"90 days"`). The number and unit may be separated
/// by any run of ASCII whitespace; unit comparison is case-insensitive
/// for the long form.
pub fn parse_duration_ns(s: &str) -> Option<u64> {
    let s = s.trim();
    if s == "raw" {
        return Some(0);
    }

    // Try the long form first: a leading integer, optional whitespace,
    // then a word-style unit. If that splits cleanly we are done.
    let split = s.find(|c: char| !c.is_ascii_digit()).map(|i| s.split_at(i));
    if let Some((num_part, rest)) = split {
        if !num_part.is_empty() {
            let unit_word = rest.trim_start();
            // If the unit slot contains whitespace before the suffix
            // it is the long form; if it is glued to the digits it
            // is the short form and we fall through.
            if rest.starts_with(|c: char| c.is_ascii_whitespace()) {
                if let Some(mult) = long_form_multiplier(unit_word) {
                    let num: u64 = num_part.parse().ok()?;
                    return num.checked_mul(mult);
                }
                return None;
            }
        }
    }

    // Compact suffix form: `"5m"`, `"1h"`, `"30s"`, `"100ms"`.
    let (num_str, unit) = if let Some(stripped) = s.strip_suffix("ms") {
        (stripped, "ms")
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else if let Some(stripped) = s.strip_suffix('d') {
        (stripped, "d")
    } else {
        return None;
    };

    let num: u64 = num_str.parse().ok()?;
    let multiplier = match unit {
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        "d" => 86_400_000_000_000,
        _ => return None,
    };

    Some(num * multiplier)
}

/// Long-form duration unit (e.g. `"day"`, `"hours"`, `"minutes"`) →
/// nanosecond multiplier. Returns `None` for unrecognised words; the
/// caller treats that as a parse failure.
fn long_form_multiplier(unit: &str) -> Option<u64> {
    match unit.to_ascii_lowercase().as_str() {
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => Some(1_000_000),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1_000_000_000),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60_000_000_000),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(3_600_000_000_000),
        "d" | "day" | "days" => Some(86_400_000_000_000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_ns("5m"), Some(300_000_000_000));
        assert_eq!(parse_duration_ns("1h"), Some(3_600_000_000_000));
        assert_eq!(parse_duration_ns("30s"), Some(30_000_000_000));
        assert_eq!(parse_duration_ns("1d"), Some(86_400_000_000_000));
        assert_eq!(parse_duration_ns("100ms"), Some(100_000_000));
        assert_eq!(parse_duration_ns("raw"), Some(0));
        assert_eq!(parse_duration_ns("invalid"), None);
    }

    #[test]
    fn test_parse_duration_long_form() {
        assert_eq!(parse_duration_ns("1 day"), Some(86_400_000_000_000));
        assert_eq!(parse_duration_ns("2 hours"), Some(7_200_000_000_000));
        assert_eq!(parse_duration_ns("30 minutes"), Some(1_800_000_000_000));
        assert_eq!(parse_duration_ns("90 days"), Some(7_776_000_000_000_000));
    }
}
