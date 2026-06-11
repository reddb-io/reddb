//! Pure WebSocket upgrade-gate policy for the RedWire-over-WSS edge
//! (issue #935, ADR 0036).
//!
//! A browser cannot speak RedWire-over-TCP, so it upgrades a binary
//! WebSocket on the TLS edge. WebSocket is *not* covered by CORS, so the
//! upgrade is **default-deny** and validated here: TLS-only, then an
//! exact-match `Origin` allowlist (Cross-Site WebSocket Hijacking
//! defence).
//!
//! This module is transport-agnostic and free of axum/HTTP types so the
//! security decision is unit-testable without a live TLS edge or socket.
//! The server's `ws_edge` axum handler maps its `Origin`/transport into
//! these inputs and renders the refusal as an HTTP response — it owns the
//! I/O, reddb-wire owns the policy.

/// Why a RedWire WebSocket upgrade was refused (ADR 0036).
///
/// Checks are ordered so the TLS gate precedes the origin checks: an
/// upgrade on the clear-text edge is rejected before the allowlist is
/// even consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsUpgradeRefusal {
    /// Arrived on a non-TLS edge — `ws://` is never accepted (WSS-only).
    NotTls,
    /// No `Origin` header — a browser always sends one; its absence is a
    /// non-browser caller or a stripped header. Reject.
    OriginMissing,
    /// `Origin` is not on the configured allowlist, or it carried a
    /// smuggled CR/LF and so can never match a well-formed allowlist
    /// entry.
    OriginRejected,
}

/// Evaluate the RedWire WS upgrade gate (ADR 0036). Pure: TLS-only, then
/// an exact-match `Origin` against the allowlist.
///
/// * `is_tls_edge` — `true` only when the request arrived on the TLS
///   listener; a clear-text edge is always refused with [`WsUpgradeRefusal::NotTls`].
/// * `origin` — the request's `Origin` header, if any. Absence is refused
///   ([`WsUpgradeRefusal::OriginMissing`]); an `Origin` carrying a CR or
///   LF is treated as a header-smuggling attempt and refused
///   ([`WsUpgradeRefusal::OriginRejected`]).
/// * `allowlist` — exact-match origins. An empty allowlist denies every
///   origin (default-deny).
pub fn evaluate_ws_upgrade(
    is_tls_edge: bool,
    origin: Option<&str>,
    allowlist: &[String],
) -> Result<(), WsUpgradeRefusal> {
    if !is_tls_edge {
        return Err(WsUpgradeRefusal::NotTls);
    }
    match origin {
        None => Err(WsUpgradeRefusal::OriginMissing),
        Some(o) if o.bytes().any(|b| b == b'\r' || b == b'\n') => {
            Err(WsUpgradeRefusal::OriginRejected)
        }
        Some(o) if allowlist.iter().any(|allowed| allowed == o) => Ok(()),
        Some(_) => Err(WsUpgradeRefusal::OriginRejected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist() -> Vec<String> {
        vec![
            "https://app.example.com".to_string(),
            "https://admin.example.com".to_string(),
        ]
    }

    #[test]
    fn allowed_origin_over_tls_is_accepted() {
        assert_eq!(
            evaluate_ws_upgrade(true, Some("https://app.example.com"), &allowlist()),
            Ok(())
        );
    }

    #[test]
    fn non_tls_edge_is_refused_before_origin_is_consulted() {
        // WSS-only: the TLS check precedes the origin check, so an
        // otherwise-allowed origin on the clear-text edge still fails.
        assert_eq!(
            evaluate_ws_upgrade(false, Some("https://app.example.com"), &allowlist()),
            Err(WsUpgradeRefusal::NotTls)
        );
    }

    #[test]
    fn missing_origin_is_refused() {
        assert_eq!(
            evaluate_ws_upgrade(true, None, &allowlist()),
            Err(WsUpgradeRefusal::OriginMissing)
        );
    }

    #[test]
    fn empty_allowlist_denies_every_origin() {
        assert_eq!(
            evaluate_ws_upgrade(true, Some("https://app.example.com"), &[]),
            Err(WsUpgradeRefusal::OriginRejected)
        );
    }

    #[test]
    fn origin_match_is_exact_not_prefix_or_suffix() {
        assert_eq!(
            evaluate_ws_upgrade(true, Some("https://app.example.com.evil.com"), &allowlist()),
            Err(WsUpgradeRefusal::OriginRejected)
        );
        assert_eq!(
            evaluate_ws_upgrade(true, Some("https://app.example.co"), &allowlist()),
            Err(WsUpgradeRefusal::OriginRejected)
        );
    }

    #[test]
    fn crlf_smuggled_origin_is_rejected() {
        // A CR/LF-bearing Origin must never match — it is a header
        // injection / smuggling attempt, refused outright.
        for smuggled in [
            "https://app.example.com\r\nX-Injected: 1",
            "https://app.example.com\n",
            "https://app.example.com\r",
        ] {
            assert_eq!(
                evaluate_ws_upgrade(true, Some(smuggled), &allowlist()),
                Err(WsUpgradeRefusal::OriginRejected)
            );
        }
    }
}
