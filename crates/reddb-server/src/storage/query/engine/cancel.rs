//! Cooperative query cancellation token.
//!
//! Issue #808 / PRD #750 (750d) — the propagation primitive for query
//! cancellation. A [`CancelToken`] is a cheap, cloneable handle over a
//! shared atomic flag. The streaming / cursor layer owns one token per
//! live stream; the executor's pull-based iterators ([`super::iterator`])
//! observe it between rows so a cancel raised by a client disconnect or an
//! explicit cancel request stops scans, filters, joins, and the aggregates
//! that consume them promptly, rather than after the whole result has been
//! materialised.
//!
//! The token carries no reason of its own — the wire-visible reason lives
//! at the stream layer ([`crate::server::output_stream::CloseReason`]).
//! Here it is a single boolean: "should this query stop now?". Cooperative
//! by design: an operator that never re-checks the token cannot be
//! interrupted, so every long-running loop in the executor must poll
//! [`CancelToken::is_cancelled`] on each iteration.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cloneable handle over a shared cancellation flag.
///
/// Clones share the same underlying flag: cancelling any clone is observed
/// by all of them. This is what lets the stream layer hold one handle while
/// the executor holds another and still agree on whether to stop.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Raise the cancel signal. Idempotent — cancelling an already-cancelled
    /// token is a no-op. Visible to every clone of this token.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// `true` once any clone of this token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observed() {
        let token = CancelToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let token = CancelToken::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn clones_share_the_same_flag() {
        let token = CancelToken::new();
        let clone = token.clone();
        // Cancelling the clone is observed through the original handle —
        // this is the property the stream layer and the executor rely on.
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_crosses_thread_boundary() {
        let token = CancelToken::new();
        let remote = token.clone();
        let handle = std::thread::spawn(move || {
            remote.cancel();
        });
        handle.join().expect("canceller thread joins");
        assert!(token.is_cancelled());
    }
}
