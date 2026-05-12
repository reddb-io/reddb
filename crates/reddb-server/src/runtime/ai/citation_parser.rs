//! `CitationParser` — pure text-to-citations extractor.
//!
//! Issue #393 (PRD #391): scan an LLM-produced answer for inline
//! `[^N]` markers and emit a structured `Vec<Citation>` plus
//! `Vec<CitationWarning>` for anomalies. The module is pure — no I/O,
//! no allocations beyond the result vectors, no panics on adversarial
//! input — so it can be unit-tested in isolation and reused by every
//! transport.
//!
//! ## Grammar
//!
//! ```text
//! marker     = "[^" digits "]"
//! digits     = '1'..='9' ('0'..='9')*     # N ≥ 1, no leading zero
//! escape     = "\\[^"                       # literal `\[^…]` is NOT a marker
//! code-fence = "```"                        # inside fences, markers are ignored
//! ```
//!
//! Only ASCII digits count. `N` is parsed as `u32`; values that
//! overflow `u32::MAX` produce a `WarningKind::Malformed` and are
//! dropped (we don't truncate silently — a runaway value is almost
//! certainly an LLM hallucination).
//!
//! `source_index` is `N - 1` (markers are 1-indexed for humans, the
//! sources array is 0-indexed). Out-of-range indices still produce a
//! `Citation` entry — callers decide whether to surface them — and
//! also produce a `WarningKind::OutOfRange` for the validator path.
//!
//! ## Code fences
//!
//! Toggled on a line whose first non-whitespace bytes are ```` ``` ````.
//! Inside a fence we skip every byte until the closing fence. Inline
//! single-backtick spans are NOT honoured because the LLM occasionally
//! cites things like `` `result_field` [^1] `` and we still want the
//! citation parsed.
//!
//! ## Escape
//!
//! A backslash directly before `[` suppresses parsing: `\[^1]` is
//! treated as literal text. We do NOT consume the backslash from the
//! span — the parser only emits citation spans, not rewritten text.

use std::ops::Range;

/// A parsed `[^N]` citation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// The number `N` as it appeared in the marker (1-indexed).
    pub marker: u32,
    /// Byte span of the marker inside the original text, including
    /// both brackets.
    pub span: Range<usize>,
    /// `marker - 1`, intended to index into the flat sources array.
    /// Note: this can equal or exceed the actual source count; check
    /// `warnings` for `OutOfRange` entries before dereferencing.
    pub source_index: u32,
}

/// A non-fatal problem encountered while scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationWarning {
    pub kind: CitationWarningKind,
    pub span: Range<usize>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationWarningKind {
    /// Saw `[^` but the body wasn't a positive decimal terminated by `]`.
    Malformed,
    /// `N - 1 >= sources_count`. Always emitted in addition to the
    /// `Citation` entry so callers can choose to suppress.
    OutOfRange,
}

/// Parse `[^N]` citation markers out of `text`.
///
/// `sources_count` is used only to flag `OutOfRange` warnings; the
/// citations themselves are returned regardless of bounds.
pub fn parse_citations(text: &str, sources_count: usize) -> CitationParseResult {
    let bytes = text.as_bytes();
    let mut citations: Vec<Citation> = Vec::new();
    let mut warnings: Vec<CitationWarning> = Vec::new();

    let mut i = 0usize;
    let mut in_fence = false;

    while i < bytes.len() {
        // Code-fence toggle: a `` ``` `` at the start of a line (after
        // optional whitespace) flips the fence state.
        if is_line_start(bytes, i) {
            let line_first = first_non_ws_on_line(bytes, i);
            if line_first + 2 < bytes.len()
                && bytes[line_first] == b'`'
                && bytes[line_first + 1] == b'`'
                && bytes[line_first + 2] == b'`'
            {
                in_fence = !in_fence;
                // skip past the fence marker; don't try to parse the
                // info-string. Advance to end of line.
                i = advance_to_newline(bytes, line_first + 3);
                continue;
            }
        }

        if in_fence {
            i += 1;
            continue;
        }

        if bytes[i] == b'[' {
            // Escape check: preceding char is an unescaped backslash.
            if i > 0 && bytes[i - 1] == b'\\' {
                // Must not be `\\[` (i.e. an escaped backslash before
                // the bracket); count backslashes.
                let backslashes = count_preceding_backslashes(bytes, i);
                if backslashes % 2 == 1 {
                    i += 1;
                    continue;
                }
            }

            if i + 1 < bytes.len() && bytes[i + 1] == b'^' {
                // Attempt to consume `[^digits]`.
                match read_marker(bytes, i) {
                    MarkerScan::Ok { marker, end } => {
                        let span = i..end;
                        let source_index = marker.saturating_sub(1);
                        if (source_index as usize) >= sources_count {
                            warnings.push(CitationWarning {
                                kind: CitationWarningKind::OutOfRange,
                                span: span.clone(),
                                detail: format!(
                                    "marker [^{marker}] references source #{} but only {} sources available",
                                    source_index + 1,
                                    sources_count
                                ),
                            });
                        }
                        citations.push(Citation {
                            marker,
                            span,
                            source_index,
                        });
                        i = end;
                        continue;
                    }
                    MarkerScan::Malformed { end, reason } => {
                        warnings.push(CitationWarning {
                            kind: CitationWarningKind::Malformed,
                            span: i..end,
                            detail: reason,
                        });
                        i = end;
                        continue;
                    }
                    MarkerScan::NotAMarker => {
                        // `[^` followed by something that can't start
                        // a marker (e.g. `[^abc]`, `[^]`). Advance 1 so
                        // we re-scan from the next byte.
                        i += 1;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }

    CitationParseResult {
        citations,
        warnings,
    }
}

/// Outcome of `parse_citations`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CitationParseResult {
    pub citations: Vec<Citation>,
    pub warnings: Vec<CitationWarning>,
}

enum MarkerScan {
    Ok { marker: u32, end: usize },
    Malformed { end: usize, reason: String },
    NotAMarker,
}

fn read_marker(bytes: &[u8], start: usize) -> MarkerScan {
    // Caller guarantees bytes[start] == b'[' and bytes[start+1] == b'^'.
    let body_start = start + 2;
    if body_start >= bytes.len() {
        return MarkerScan::NotAMarker;
    }

    // Find the closing `]`. We accept the marker only if every byte
    // between `[^` and `]` is an ASCII digit and the number is ≥ 1.
    let mut j = body_start;
    while j < bytes.len() && bytes[j] != b']' {
        if !bytes[j].is_ascii_digit() {
            // Recognise the `[^anything-non-digit…]` shape so we can
            // emit a precise warning. Cap the scan at 16 bytes so a
            // malicious input can't make us scan to EOF.
            let mut k = body_start;
            let mut all_inside = true;
            while k < bytes.len() && k - body_start < 16 {
                if bytes[k] == b']' {
                    break;
                }
                k += 1;
                if k < bytes.len() && bytes[k] == b'\n' {
                    all_inside = false;
                    break;
                }
            }
            if all_inside && k < bytes.len() && bytes[k] == b']' {
                return MarkerScan::Malformed {
                    end: k + 1,
                    reason: format!(
                        "expected digits inside [^…], got `{}`",
                        String::from_utf8_lossy(&bytes[body_start..k])
                    ),
                };
            }
            return MarkerScan::NotAMarker;
        }
        j += 1;
    }
    if j >= bytes.len() {
        return MarkerScan::NotAMarker;
    }
    // Empty body `[^]`.
    if j == body_start {
        return MarkerScan::Malformed {
            end: j + 1,
            reason: "empty marker body".to_string(),
        };
    }
    // Leading zero (e.g. `[^01]`) is not the canonical form. We accept
    // single `0` as malformed (N ≥ 1) and reject any multi-digit value
    // with a leading zero.
    if bytes[body_start] == b'0' {
        return MarkerScan::Malformed {
            end: j + 1,
            reason: format!(
                "marker must be a positive integer with no leading zero, got `{}`",
                String::from_utf8_lossy(&bytes[body_start..j])
            ),
        };
    }

    // Parse the digits as u32. A value that overflows u32 is treated
    // as malformed — an LLM emitting `[^99999999999]` is almost
    // certainly hallucinating.
    let digits = &bytes[body_start..j];
    let mut acc: u64 = 0;
    for &d in digits {
        acc = acc * 10 + (d - b'0') as u64;
        if acc > u32::MAX as u64 {
            return MarkerScan::Malformed {
                end: j + 1,
                reason: format!(
                    "marker value `{}` exceeds u32::MAX",
                    String::from_utf8_lossy(digits)
                ),
            };
        }
    }
    let marker = acc as u32;
    if marker == 0 {
        // Defensive — should have been caught by the leading-zero check.
        return MarkerScan::Malformed {
            end: j + 1,
            reason: "marker must be ≥ 1".to_string(),
        };
    }

    MarkerScan::Ok {
        marker,
        end: j + 1,
    }
}

fn is_line_start(bytes: &[u8], i: usize) -> bool {
    i == 0 || bytes[i - 1] == b'\n'
}

fn first_non_ws_on_line(bytes: &[u8], i: usize) -> usize {
    let mut k = i;
    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
        k += 1;
    }
    k
}

fn advance_to_newline(bytes: &[u8], i: usize) -> usize {
    let mut k = i;
    while k < bytes.len() && bytes[k] != b'\n' {
        k += 1;
    }
    // Step past the newline if we're sitting on one.
    if k < bytes.len() {
        k + 1
    } else {
        k
    }
}

fn count_preceding_backslashes(bytes: &[u8], i: usize) -> usize {
    let mut k = i;
    let mut count = 0;
    while k > 0 && bytes[k - 1] == b'\\' {
        count += 1;
        k -= 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, n_sources: usize) -> CitationParseResult {
        parse_citations(text, n_sources)
    }

    #[test]
    fn well_formed_single_marker() {
        let r = parse("Churn was driven by pricing[^1].", 1);
        assert_eq!(r.citations.len(), 1);
        assert!(r.warnings.is_empty());
        assert_eq!(r.citations[0].marker, 1);
        assert_eq!(r.citations[0].source_index, 0);
        // span covers `[^1]`
        let c = &r.citations[0];
        assert_eq!(&"Churn was driven by pricing[^1]."[c.span.clone()], "[^1]");
    }

    #[test]
    fn well_formed_multi_digit_marker() {
        let r = parse("see [^42] and [^1234]", 1300);
        assert_eq!(
            r.citations.iter().map(|c| c.marker).collect::<Vec<_>>(),
            vec![42, 1234]
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn repeated_markers_are_each_emitted() {
        let r = parse("a[^1] b[^1] c[^2]", 2);
        assert_eq!(r.citations.len(), 3);
        assert_eq!(r.citations[0].marker, 1);
        assert_eq!(r.citations[1].marker, 1);
        assert_eq!(r.citations[2].marker, 2);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn empty_marker_body_is_malformed() {
        let r = parse("a[^] b", 0);
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(r.warnings[0].kind, CitationWarningKind::Malformed));
    }

    #[test]
    fn non_digit_marker_is_malformed() {
        let r = parse("see [^abc] for context", 0);
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(r.warnings[0].kind, CitationWarningKind::Malformed));
    }

    #[test]
    fn negative_looking_marker_is_malformed() {
        let r = parse("nope[^-1]nope", 0);
        // `-` is not a digit → malformed.
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(r.warnings[0].kind, CitationWarningKind::Malformed));
    }

    #[test]
    fn leading_zero_marker_is_malformed() {
        let r = parse("nope[^01]nope", 5);
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(r.warnings[0].kind, CitationWarningKind::Malformed));
    }

    #[test]
    fn lone_zero_marker_is_malformed() {
        let r = parse("nope[^0]nope", 5);
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn very_large_marker_within_u32() {
        let r = parse("see [^4294967295]", 1);
        assert_eq!(r.citations.len(), 1);
        assert_eq!(r.citations[0].marker, u32::MAX);
        // Out of range vs 1 source.
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(
            r.warnings[0].kind,
            CitationWarningKind::OutOfRange
        ));
    }

    #[test]
    fn marker_over_u32_is_malformed() {
        let r = parse("see [^9999999999999]", 0);
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(matches!(r.warnings[0].kind, CitationWarningKind::Malformed));
    }

    #[test]
    fn escaped_marker_is_not_parsed() {
        let r = parse(r"literal \[^1\] in text", 1);
        assert!(r.citations.is_empty());
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn double_backslash_does_not_escape() {
        // `\\[^1]` — the backslash before `[` is itself escaped, so
        // the marker should parse.
        let r = parse(r"path\\[^1] continues", 1);
        assert_eq!(r.citations.len(), 1);
    }

    #[test]
    fn marker_inside_code_fence_is_ignored() {
        let text = "before[^1]\n```\nthe code uses [^2] internally\n```\nafter[^3]";
        let r = parse(text, 3);
        let markers: Vec<u32> = r.citations.iter().map(|c| c.marker).collect();
        assert_eq!(markers, vec![1, 3]);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn fenced_with_info_string_still_ignored() {
        let text = "head[^1]\n```rust\nlet x = [^99];\n```\ntail[^2]";
        let r = parse(text, 2);
        let markers: Vec<u32> = r.citations.iter().map(|c| c.marker).collect();
        assert_eq!(markers, vec![1, 2]);
    }

    #[test]
    fn unicode_neighbors_are_safe() {
        let text = "感谢[^1]谢谢";
        let r = parse(text, 1);
        assert_eq!(r.citations.len(), 1);
        let span = r.citations[0].span.clone();
        assert_eq!(&text[span], "[^1]");
    }

    #[test]
    fn out_of_range_emits_citation_and_warning() {
        let r = parse("see [^5] and [^1]", 2);
        assert_eq!(r.citations.len(), 2);
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].kind, CitationWarningKind::OutOfRange);
        // Out-of-range citation still present so the caller can render
        // it as a soft error.
        assert_eq!(r.citations[0].marker, 5);
        assert_eq!(r.citations[0].source_index, 4);
    }

    #[test]
    fn empty_text_yields_empty_result() {
        let r = parse("", 0);
        assert!(r.citations.is_empty());
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn no_panics_on_truncated_markers() {
        // Adversarial inputs that look like the start of a marker but
        // never close. None of these should panic or allocate
        // unbounded.
        for bad in ["[", "[^", "[^1", "[^123", "[^abc", "[^\n1]", "[^99"] {
            let _ = parse(bad, 0);
        }
    }

    #[test]
    fn malformed_with_newline_inside_body() {
        let r = parse("see [^12\n] here", 0);
        // Newline aborts the scan; nothing emitted.
        assert!(r.citations.is_empty());
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn back_to_back_markers() {
        let r = parse("[^1][^2][^3]", 3);
        assert_eq!(
            r.citations.iter().map(|c| c.marker).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(r.warnings.is_empty());
    }
}
