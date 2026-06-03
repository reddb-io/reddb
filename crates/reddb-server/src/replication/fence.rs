//! Stale-term fencing of a returning ex-primary (issue #835, PRD #819, ADR 0030).
//!
//! After a failover the cluster serves a *new* term. A former primary that
//! was partitioned away (or crashed) during the handover can come back still
//! believing it is the leader, stamping records and opening replication
//! streams under its **stale** term. Left unchecked it would push its old
//! timeline onto replicas that have already moved on — corrupting the new
//! timeline the way Valkey loses acknowledged writes.
//!
//! The defence is a single, boring rule applied at every boundary a
//! term-stamped attempt can enter through:
//!
//! > **An attempt stamped with a term *behind* the current term is fenced.**
//!
//! "Behind" means strictly older. Equal is admitted (a same-term retry), and
//! newer is admitted *and adopted* (the node has learned of a more recent
//! term and follows it — the only legitimate way a returning ex-primary
//! re-joins). The rule is intentionally identical at all three boundaries so
//! there is one place to reason about it:
//!
//! * [`FenceBoundary::Apply`] — the replica apply path
//!   ([`super::logical::LogicalChangeApplier`]) rejects a *record* whose term
//!   is behind the highest term it has applied. This is the data-path gate:
//!   even if a stale stream slips past the handshake, no stale record lands.
//! * [`FenceBoundary::Handshake`] — a replica refuses to *open a stream* for a
//!   peer that announces a stale term in its handshake, so a stale ex-primary
//!   cannot even attach as a source.
//! * [`FenceBoundary::Lease`] — the serverless writer lease ([`super::lease`])
//!   is stamped with the term it was taken under; a holder whose term is
//!   behind the current term **fails closed** and may not keep mutating.
//!
//! Together these guarantee the issue #835 contract: *a returning ex-primary
//! cannot advance any watermark or apply on replicas until it adopts the new
//! term.* The moment it re-syncs and adopts the current term, every boundary
//! admits it again — fencing is a gate, not a ban.
//!
//! ## Module shape
//!
//! Like the rest of the consensus core ([`super::election`],
//! [`super::failover`]), this is **pure logic**: [`TermFence`] holds a single
//! `u64` and decides admission; it owns no clock, socket, or engine. The
//! apply-path integration lives in [`super::logical`]; the lease integration
//! lives in [`super::lease`]; both route their term check through
//! [`term_is_stale`] so the rule cannot drift between boundaries.

/// Which boundary a term-stamped attempt arrived at. Purely informational —
/// carried in [`StaleTermRejection`] so a log line or metric can tell an
/// apply-path fence from a handshake or lease fence apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceBoundary {
    /// A replica applying a streamed change record.
    Apply,
    /// A replica accepting a replication-stream handshake from a peer.
    Handshake,
    /// A serverless writer attempting to mutate under its lease.
    Lease,
}

impl FenceBoundary {
    pub fn label(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Handshake => "handshake",
            Self::Lease => "lease",
        }
    }
}

/// A term-stamped attempt was refused because its term is behind the term in
/// force. The fields are the evidence: what term arrived, what term is
/// current, and at which boundary the refusal happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTermRejection {
    pub boundary: FenceBoundary,
    pub incoming_term: u64,
    pub current_term: u64,
}

impl std::fmt::Display for StaleTermRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "fenced stale term at {} boundary: incoming term {} is behind current term {}",
            self.boundary.label(),
            self.incoming_term,
            self.current_term
        )
    }
}

impl std::error::Error for StaleTermRejection {}

/// The one rule, factored out so every boundary calls the same predicate: an
/// `incoming_term` strictly older than `current_term` is stale. Equal and
/// newer terms are not stale.
#[inline]
pub fn term_is_stale(incoming_term: u64, current_term: u64) -> bool {
    incoming_term < current_term
}

/// A replication-stream handshake as seen by the admitting replica. The fence
/// only cares about the announced `term`; `peer_id` rides along for the log
/// trail so a fenced stale ex-primary is identifiable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHandshake {
    pub peer_id: String,
    pub term: u64,
}

impl StreamHandshake {
    pub fn new(peer_id: impl Into<String>, term: u64) -> Self {
        Self {
            peer_id: peer_id.into(),
            term,
        }
    }
}

/// The node's view of "what term is in force now", and the gate every
/// term-stamped inbound attempt passes through. Monotonic: the current term
/// only ever moves forward, mirroring the durable term bump the election
/// performs ([`super::election::ElectionTransport::bump_term`]).
#[derive(Debug, Clone)]
pub struct TermFence {
    current_term: u64,
}

impl TermFence {
    /// A fence anchored at `current_term` — typically the node's durable
    /// replication term at boot.
    pub fn new(current_term: u64) -> Self {
        Self { current_term }
    }

    /// The term currently in force.
    pub fn current_term(&self) -> u64 {
        self.current_term
    }

    /// Adopt `term` if it is newer than the current term, returning `true`
    /// when the fence advanced. Never moves backwards: a stale or equal term
    /// is ignored and returns `false`. This is how a node follows a failover
    /// — and how a returning ex-primary, once it learns the new term,
    /// re-synchronises its fence so its own subsequent attempts pass.
    pub fn adopt_term(&mut self, term: u64) -> bool {
        if term > self.current_term {
            self.current_term = term;
            true
        } else {
            false
        }
    }

    /// Core admission check at `boundary` for an attempt stamped
    /// `incoming_term`. `Ok(())` admits; `Err(StaleTermRejection)` fences a
    /// term behind the current one.
    pub fn admit(
        &self,
        boundary: FenceBoundary,
        incoming_term: u64,
    ) -> Result<(), StaleTermRejection> {
        if term_is_stale(incoming_term, self.current_term) {
            Err(StaleTermRejection {
                boundary,
                incoming_term,
                current_term: self.current_term,
            })
        } else {
            Ok(())
        }
    }

    /// Admit (or fence) a replication-stream handshake. A stale ex-primary
    /// announcing its old term is refused here, before any stream is opened.
    pub fn admit_handshake(&self, handshake: &StreamHandshake) -> Result<(), StaleTermRejection> {
        self.admit(FenceBoundary::Handshake, handshake.term)
    }

    /// Admit (or fence) a streamed change record stamped `record_term`.
    pub fn admit_record(&self, record_term: u64) -> Result<(), StaleTermRejection> {
        self.admit(FenceBoundary::Apply, record_term)
    }

    /// Admit (or fence) a lease-backed write whose lease was taken under
    /// `lease_term`. A holder whose lease term is behind the current term
    /// fails closed.
    pub fn admit_lease_write(&self, lease_term: u64) -> Result<(), StaleTermRejection> {
        self.admit(FenceBoundary::Lease, lease_term)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_predicate_is_strict_less_than() {
        assert!(term_is_stale(4, 5), "older term is stale");
        assert!(!term_is_stale(5, 5), "same term is not stale");
        assert!(!term_is_stale(6, 5), "newer term is not stale");
    }

    #[test]
    fn current_term_admits_and_stale_is_fenced_at_apply_boundary() {
        let fence = TermFence::new(5);
        assert!(fence.admit_record(5).is_ok(), "same term applies");
        assert!(fence.admit_record(6).is_ok(), "newer term applies");

        let rejection = fence.admit_record(4).expect_err("stale term is fenced");
        assert_eq!(rejection.boundary, FenceBoundary::Apply);
        assert_eq!(rejection.incoming_term, 4);
        assert_eq!(rejection.current_term, 5);
    }

    #[test]
    fn handshake_from_stale_ex_primary_is_refused() {
        let fence = TermFence::new(7);
        let stale = StreamHandshake::new("old-primary", 6);
        let rejection = fence
            .admit_handshake(&stale)
            .expect_err("stale handshake is fenced");
        assert_eq!(rejection.boundary, FenceBoundary::Handshake);
        assert_eq!(rejection.incoming_term, 6);

        let current = StreamHandshake::new("new-primary", 7);
        assert!(
            fence.admit_handshake(&current).is_ok(),
            "current-term handshake is admitted",
        );
    }

    #[test]
    fn stale_lease_holder_fails_closed() {
        let fence = TermFence::new(9);
        assert!(
            fence.admit_lease_write(8).is_err(),
            "a lease taken under an older term fails closed",
        );
        assert!(
            fence.admit_lease_write(9).is_ok(),
            "a lease at the current term may write",
        );
    }

    #[test]
    fn adopt_term_advances_only_forward() {
        let mut fence = TermFence::new(5);
        assert!(!fence.adopt_term(4), "stale term ignored");
        assert!(!fence.adopt_term(5), "equal term ignored");
        assert_eq!(fence.current_term(), 5);
        assert!(fence.adopt_term(6), "newer term adopted");
        assert_eq!(fence.current_term(), 6);
    }

    #[test]
    fn returning_ex_primary_passes_once_it_adopts_the_new_term() {
        // The cluster moved to term 6; the ex-primary is still on 5.
        let mut fence = TermFence::new(6);
        assert!(
            fence.admit_record(5).is_err(),
            "stale-term record is fenced before re-sync",
        );

        // It re-syncs and adopts the new term (a no-op on the *fence's*
        // current term here, but models the node learning term 6). Its
        // subsequent attempts are stamped with the new term and pass.
        fence.adopt_term(6);
        assert!(
            fence.admit_record(6).is_ok(),
            "after adopting the new term, attempts are admitted",
        );
    }
}
