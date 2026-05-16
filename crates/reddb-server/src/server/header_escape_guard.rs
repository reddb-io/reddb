//! `HeaderEscapeGuard` — typed boundary guard for HTTP response header values.
//!
//! Per ADR 0010 (`.red/adr/0010-serialization-boundary-discipline.md`)
//! and issue #176, the producing side of every serialization boundary
//! is owned by a typed guard whose only job is to know the boundary's
//! escape contract. This module is the guard for HTTP response header
//! values.
//!
//! ## Why this guard exists
//!
//! HTTP/1.1 frames headers as `name: value\r\n` pairs terminated by a
//! double `\r\n`. If a header value contains a raw CR or LF, an
//! attacker can splice a second header (or the entire body) past the
//! original framing — the classic CRLF-injection / response-splitting
//! shape called out by the Whiz / Babeld disclosure (March 2026).
//!
//! ## Contract
//!
//! `HeaderEscapeGuard::header_value(s)` returns a typed
//! `http::HeaderValue` if and only if `s` is safe for an HTTP/1.1
//! response header value:
//!
//! - No CR (`\r`) or LF (`\n`) — these terminate the header line.
//! - No NUL (`\0`) — proxies and intermediaries truncate on NUL.
//! - No tab (`\t`) — RFC 7230 admits HTAB inside header values, but
//!   it is the most common smuggling lever for downstream log
//!   pipelines that split on whitespace, and there is no legitimate
//!   producer-side reason for RedDB to emit one.
//! - No other ASCII control byte (0x00–0x1F, 0x7F).
//! - Bounded length: 8 KiB ceiling per value. Real HTTP intermediaries
//!   start dropping connections well before this; the guard rejects
//!   early so a misuse becomes a typed error, not a runtime hang.
//!
//! Non-ASCII bytes (0x80–0xFF) are *permitted* — RFC 7230 §3.2.6
//! discourages them but does not forbid them, and `http::HeaderValue`
//! accepts them. Producers should emit ASCII; the guard does not
//! police that.
//!
//! ## Failure mode
//!
//! Every rejection path returns a typed `EscapeError`. Callers must
//! propagate the error to the HTTP boundary — the guard never silently
//! truncates, replaces, or escapes-around a control byte. Silent
//! mangling at this layer is the exact failure shape ADR 0010 is
//! designed to prevent.
//!
//! ## Out of scope
//!
//! - Header *names*. RedDB sets header names from `&'static str`
//!   literals only; the names live in source code, not in user input.
//!   If a future surface admits user-supplied header names, that
//!   needs its own guard.
//! - Request-side headers. Inbound parsing already happens in
//!   `transport::HttpRequest::read_from`; the inbound parser is a
//!   separate concern.

use std::fmt;

use http::HeaderValue;

/// Maximum permitted header value length, in bytes.
///
/// Chosen to be permissive enough for any realistic header value
/// (URLs, JWT tokens, Set-Cookie payloads with attributes) yet small
/// enough that a misuse — an attacker pushing megabytes through a
/// header — surfaces as a typed error long before it eats memory or
/// stalls the connection. 8 KiB matches the `request headers too
/// large` ceiling already enforced by `HttpRequest::read_from` for
/// inbound headers, keeping the inbound and outbound limits
/// symmetric.
pub const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

/// Reasons `HeaderEscapeGuard::header_value` rejects a string.
///
/// Each variant names the exact byte class that triggered the
/// rejection so the caller can build a useful 4xx / 500 response and
/// the audit log gets a structured diagnostic, not a hand-formatted
/// string. The byte payload on `ContainsNonPrintable` is the
/// offending byte itself, useful for debug logs and for tests
/// asserting the guard caught the right byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeError {
    /// Value contained CR (`\r`) or LF (`\n`).
    ContainsCrlf,
    /// Value contained NUL (`\0`).
    ContainsNull,
    /// Value contained HTAB (`\t`).
    ContainsTab,
    /// Value contained another non-printable ASCII byte
    /// (0x01–0x08, 0x0B, 0x0C, 0x0E–0x1F, or 0x7F). The payload is
    /// the offending byte for diagnostic clarity.
    ContainsNonPrintable(u8),
    /// Value exceeds [`MAX_HEADER_VALUE_BYTES`]. The payload is the
    /// observed length so the caller can include it in the error
    /// reply.
    OversizeForBoundary(usize),
}

impl fmt::Display for EscapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainsCrlf => {
                f.write_str("header value contains CR or LF (would smuggle a second header line)")
            }
            Self::ContainsNull => f.write_str(
                "header value contains NUL (proxies and intermediaries truncate on NUL)",
            ),
            Self::ContainsTab => f.write_str(
                "header value contains TAB (downstream log pipelines split on whitespace)",
            ),
            Self::ContainsNonPrintable(b) => {
                write!(f, "header value contains non-printable byte 0x{b:02X}")
            }
            Self::OversizeForBoundary(n) => write!(
                f,
                "header value length {n} exceeds the {MAX_HEADER_VALUE_BYTES}-byte boundary limit"
            ),
        }
    }
}

impl std::error::Error for EscapeError {}

/// Typed guard for HTTP response header values.
///
/// The struct is zero-sized; it exists for the namespace and for
/// future extensions (e.g., per-boundary length policies). Callers
/// invoke the guard exclusively through associated functions.
///
/// ```ignore
/// use crate::server::header_escape_guard::{HeaderEscapeGuard, EscapeError};
///
/// let value = HeaderEscapeGuard::header_value("max-age=3600")?;
/// // value is now an `http::HeaderValue` safe to attach to a
/// // response. Attempting to splice a second header line is
/// // rejected at the type boundary:
/// assert!(matches!(
///     HeaderEscapeGuard::header_value("evil\r\nX-Forged: 1"),
///     Err(EscapeError::ContainsCrlf),
/// ));
/// # Ok::<(), EscapeError>(())
/// ```
pub struct HeaderEscapeGuard;

impl HeaderEscapeGuard {
    /// Validate `s` and wrap it in a typed `http::HeaderValue`.
    ///
    /// Returns the typed error (`EscapeError`) on the first byte that
    /// violates the contract. The order of checks is: oversize →
    /// CRLF → NUL → TAB → other non-printable. Callers must not
    /// assume the order — only that some violation triggered the
    /// rejection.
    pub fn header_value(s: &str) -> Result<HeaderValue, EscapeError> {
        let bytes = s.as_bytes();
        if bytes.len() > MAX_HEADER_VALUE_BYTES {
            return Err(EscapeError::OversizeForBoundary(bytes.len()));
        }
        for &b in bytes {
            match b {
                b'\r' | b'\n' => return Err(EscapeError::ContainsCrlf),
                0 => return Err(EscapeError::ContainsNull),
                b'\t' => return Err(EscapeError::ContainsTab),
                // Other ASCII control bytes: 0x01–0x08, 0x0B, 0x0C,
                // 0x0E–0x1F, plus DEL (0x7F).
                0x01..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F => {
                    return Err(EscapeError::ContainsNonPrintable(b));
                }
                _ => {}
            }
        }
        // SAFETY-equivalent: every byte we accepted is a printable
        // ASCII byte, a space, or 0x80..=0xFF — all of which
        // `HeaderValue::from_bytes` accepts. The construction can
        // only fail for the exact bytes we already rejected, so an
        // error here is unreachable in well-formed code; we surface
        // it as the closest typed error rather than panicking so a
        // future tightening of `http`'s rules degrades gracefully.
        HeaderValue::from_bytes(bytes).map_err(|_| EscapeError::ContainsNonPrintable(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Happy paths --------------------------------------------------

    #[test]
    fn accepts_simple_ascii() {
        let v = HeaderEscapeGuard::header_value("application/json").unwrap();
        assert_eq!(v.as_bytes(), b"application/json");
    }

    #[test]
    fn accepts_empty_string() {
        // RFC 7230 §3.2 admits empty header values.
        let v = HeaderEscapeGuard::header_value("").unwrap();
        assert_eq!(v.as_bytes(), b"");
    }

    #[test]
    fn accepts_value_with_spaces_and_punctuation() {
        let v = HeaderEscapeGuard::header_value("text/html; charset=utf-8, q=0.9").unwrap();
        assert_eq!(v.as_bytes(), b"text/html; charset=utf-8, q=0.9");
    }

    #[test]
    fn accepts_max_length_value() {
        let s = "a".repeat(MAX_HEADER_VALUE_BYTES);
        let v = HeaderEscapeGuard::header_value(&s).unwrap();
        assert_eq!(v.as_bytes().len(), MAX_HEADER_VALUE_BYTES);
    }

    #[test]
    fn accepts_high_bit_bytes() {
        // 0x80..=0xFF are discouraged by RFC 7230 but not forbidden,
        // and `http::HeaderValue` accepts them. The guard mirrors
        // `http`'s policy so we don't second-guess the upstream.
        let v = HeaderEscapeGuard::header_value("café").unwrap();
        assert_eq!(v.as_bytes(), "café".as_bytes());
    }

    // --- Rejection paths ---------------------------------------------

    #[test]
    fn rejects_carriage_return() {
        assert_eq!(
            HeaderEscapeGuard::header_value("evil\rinjected"),
            Err(EscapeError::ContainsCrlf)
        );
    }

    #[test]
    fn rejects_line_feed() {
        assert_eq!(
            HeaderEscapeGuard::header_value("evil\ninjected"),
            Err(EscapeError::ContainsCrlf)
        );
    }

    #[test]
    fn rejects_crlf_pair_for_response_splitting() {
        // The classic response-splitting shape: terminate the
        // current header, splice a second header, splice a body.
        let payload = "ok\r\nX-Forged: 1\r\n\r\n<html>pwned</html>";
        assert_eq!(
            HeaderEscapeGuard::header_value(payload),
            Err(EscapeError::ContainsCrlf)
        );
    }

    #[test]
    fn rejects_nul() {
        assert_eq!(
            HeaderEscapeGuard::header_value("trunc\0ate"),
            Err(EscapeError::ContainsNull)
        );
    }

    #[test]
    fn rejects_tab() {
        assert_eq!(
            HeaderEscapeGuard::header_value("split\tlog"),
            Err(EscapeError::ContainsTab)
        );
    }

    #[test]
    fn rejects_backspace() {
        assert_eq!(
            HeaderEscapeGuard::header_value("over\u{0008}type"),
            Err(EscapeError::ContainsNonPrintable(0x08))
        );
    }

    #[test]
    fn rejects_bell() {
        assert_eq!(
            HeaderEscapeGuard::header_value("ding\u{0007}!"),
            Err(EscapeError::ContainsNonPrintable(0x07))
        );
    }

    #[test]
    fn rejects_form_feed() {
        assert_eq!(
            HeaderEscapeGuard::header_value("page\u{000C}break"),
            Err(EscapeError::ContainsNonPrintable(0x0C))
        );
    }

    #[test]
    fn rejects_vertical_tab() {
        assert_eq!(
            HeaderEscapeGuard::header_value("vert\u{000B}tab"),
            Err(EscapeError::ContainsNonPrintable(0x0B))
        );
    }

    #[test]
    fn rejects_escape_byte() {
        assert_eq!(
            HeaderEscapeGuard::header_value("\u{001B}[31mred"),
            Err(EscapeError::ContainsNonPrintable(0x1B))
        );
    }

    #[test]
    fn rejects_del_byte() {
        assert_eq!(
            HeaderEscapeGuard::header_value("hello\u{007F}"),
            Err(EscapeError::ContainsNonPrintable(0x7F))
        );
    }

    #[test]
    fn rejects_oversize() {
        let s = "a".repeat(MAX_HEADER_VALUE_BYTES + 1);
        assert_eq!(
            HeaderEscapeGuard::header_value(&s),
            Err(EscapeError::OversizeForBoundary(MAX_HEADER_VALUE_BYTES + 1))
        );
    }

    #[test]
    fn oversize_check_runs_before_byte_scan() {
        // Even a value full of CRLFs reports as oversize when it
        // also exceeds the length cap. Cheap test that fixes the
        // observable order; if a future refactor flips the order
        // we want a deliberate signal.
        let mut s = String::with_capacity(MAX_HEADER_VALUE_BYTES + 4);
        for _ in 0..(MAX_HEADER_VALUE_BYTES / 2 + 1) {
            s.push_str("\r\n");
        }
        let n = s.len();
        assert_eq!(
            HeaderEscapeGuard::header_value(&s),
            Err(EscapeError::OversizeForBoundary(n))
        );
    }

    // --- Error display formatting ------------------------------------

    #[test]
    fn error_display_mentions_byte_class() {
        assert!(EscapeError::ContainsCrlf.to_string().contains("CR or LF"));
        assert!(EscapeError::ContainsNull.to_string().contains("NUL"));
        assert!(EscapeError::ContainsTab.to_string().contains("TAB"));
        assert!(EscapeError::ContainsNonPrintable(0x07)
            .to_string()
            .contains("0x07"));
        assert!(EscapeError::OversizeForBoundary(99_999)
            .to_string()
            .contains("99999"));
    }

    // --- Snapshot of escaped output for known fixtures ---------------
    //
    // Per issue #176 acceptance criteria. We don't pull in `insta` for
    // a single snapshot; the assertion is inline so it survives a
    // refactor without depending on a dev-only crate.

    #[test]
    fn snapshot_known_fixtures() {
        // (input, expected outcome). Order is documentation: each
        // line shows a known-shape attacker string and the verdict
        // the guard must return.
        let cases: &[(&str, Result<&[u8], EscapeError>)] = &[
            ("application/json", Ok(b"application/json")),
            (
                "max-age=31536000; includeSubDomains",
                Ok(b"max-age=31536000; includeSubDomains"),
            ),
            ("nosniff", Ok(b"nosniff")),
            ("DENY", Ok(b"DENY")),
            ("\"abc-123\"", Ok(b"\"abc-123\"")),
            ("evil\r\nLocation: /pwned", Err(EscapeError::ContainsCrlf)),
            ("set-cookie\nset-cookie", Err(EscapeError::ContainsCrlf)),
            (
                "bell\x07alarm",
                Err(EscapeError::ContainsNonPrintable(0x07)),
            ),
            ("trunc\0ate", Err(EscapeError::ContainsNull)),
            ("split\there", Err(EscapeError::ContainsTab)),
        ];
        for (input, expected) in cases {
            let got = HeaderEscapeGuard::header_value(input);
            match (expected, &got) {
                (Ok(bytes), Ok(v)) => {
                    assert_eq!(v.as_bytes(), *bytes, "input {input:?} produced wrong bytes")
                }
                (Err(want), Err(got_err)) => {
                    assert_eq!(want, got_err, "input {input:?} produced wrong error")
                }
                (Ok(_), Err(e)) => panic!("input {input:?} unexpectedly rejected: {e:?}"),
                (Err(want), Ok(v)) => panic!(
                    "input {input:?} unexpectedly accepted (bytes={:?}); wanted {want:?}",
                    v.as_bytes()
                ),
            }
        }
    }

    // --- Byte-level fuzz / proptest-style coverage --------------------
    //
    // The `proptest` crate is a dev-dep at the workspace root. The
    // assertion shape we want is small enough that we hand-roll a
    // deterministic byte-level sweep here rather than pull `proptest`
    // into this module, keeping the test fast and reproducible.

    #[test]
    fn fuzz_every_single_byte_position() {
        // Inserting any rejected byte at any position in an
        // otherwise-clean value must trigger the typed error for
        // that byte class.
        for byte in 0u8..=0x1F {
            for pos in [0usize, 5, 9] {
                let mut bytes = b"abcdefghij".to_vec();
                bytes.insert(pos, byte);
                let s = String::from_utf8(bytes).unwrap();
                let got = HeaderEscapeGuard::header_value(&s);
                let want = match byte {
                    b'\r' | b'\n' => EscapeError::ContainsCrlf,
                    0 => EscapeError::ContainsNull,
                    b'\t' => EscapeError::ContainsTab,
                    _ => EscapeError::ContainsNonPrintable(byte),
                };
                assert_eq!(got, Err(want), "byte 0x{byte:02X} at pos {pos}");
            }
        }
        // DEL is the lone non-control rejected byte above 0x20.
        assert_eq!(
            HeaderEscapeGuard::header_value("a\u{007F}b"),
            Err(EscapeError::ContainsNonPrintable(0x7F))
        );
    }

    #[test]
    fn fuzz_every_printable_ascii_accepted() {
        for byte in 0x20u8..0x7F {
            let s = format!("x{}y", byte as char);
            assert!(
                HeaderEscapeGuard::header_value(&s).is_ok(),
                "byte 0x{byte:02X} should be accepted",
            );
        }
    }

    #[test]
    fn fuzz_every_high_bit_byte_accepted() {
        // 0x80..=0xFF must round-trip — the guard does not enforce
        // ASCII-only output. Note we build the value as raw bytes
        // and convert via from_utf8_unchecked-equivalent: we keep
        // the test memory-safe by constructing a single-byte
        // payload that is valid UTF-8 only when the byte is < 0x80
        // and otherwise wrapping it in a multi-byte UTF-8 lead.
        // The guard takes `&str`, so we route every high-bit byte
        // through a UTF-8-valid encoding.
        for codepoint in 0x80u32..=0xFF {
            let s = char::from_u32(codepoint).unwrap().to_string();
            let v = HeaderEscapeGuard::header_value(&s).unwrap();
            // The bytes round-trip exactly as the input UTF-8.
            assert_eq!(v.as_bytes(), s.as_bytes());
        }
    }

    #[test]
    fn fuzz_oversize_boundary() {
        // The exact boundary is accepted; one byte past is rejected.
        let exact = "a".repeat(MAX_HEADER_VALUE_BYTES);
        assert!(HeaderEscapeGuard::header_value(&exact).is_ok());
        let over = "a".repeat(MAX_HEADER_VALUE_BYTES + 1);
        assert_eq!(
            HeaderEscapeGuard::header_value(&over),
            Err(EscapeError::OversizeForBoundary(MAX_HEADER_VALUE_BYTES + 1))
        );
    }

    #[test]
    fn fuzz_concatenation_attacks() {
        // The shape the Whiz / Babeld disclosure made famous:
        // suffix a control sequence after a benign-looking prefix.
        let trailers = [
            "\r\n",
            "\n",
            "\r",
            "\r\nX-Forged: 1",
            "\r\nLocation: http://attacker/",
            "\r\n\r\n<html>",
        ];
        for trailer in trailers {
            let payload = format!("application/json{trailer}");
            assert_eq!(
                HeaderEscapeGuard::header_value(&payload),
                Err(EscapeError::ContainsCrlf),
                "payload {payload:?} must reject"
            );
        }
    }
}
