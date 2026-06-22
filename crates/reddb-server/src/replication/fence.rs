//! Stale-term fencing for a returning ex-primary (issue #835, PRD #819, ADR 0030).
//!
//! After a failover the cluster serves a *new term*. A former primary that
//! rejoins on its old, **stale** term must not be able to corrupt the new
//! timeline — its term-stamped writes and its stream handshakes have to be
//! refused until it re-syncs and adopts the new term. This module is the
//! reusable term-comparison primitive both fencing boundaries share:
//!
//! * **Apply boundary** — a replica rejects a WAL/logical record whose term
//!   is behind its current term. The live replica apply path enforces this
//!   directly in [`super::logical::LogicalChangeApplier::apply`] (it already
//!   tracks the last-applied term); [`TermFence::admit_record`] is the same
//!   rule expressed over a durable term so it survives a restart.
//! * **Handshake boundary** — when a node opens a replication stream it
//!   declares the term it is streaming under. [`TermFence::admit_handshake`]
//!   refuses a handshake whose declared term is behind the current term, so a
//!   stale ex-primary cannot even establish the stream.
//! * **Lease boundary** — a serverless writer lease is stamped with the term
//!   it was taken under; a holder whose term is behind the current term fails
//!   closed before mutating remote artifacts.
//!
//! The decision is deliberately the data-path twin of the election-side
//! [`super::RefusalReason::StaleTerm`]:
//!
//! * `incoming == current` → **admit** at the live term;
//! * `incoming  > current` → a newer timeline supersedes ours, so **adopt**
//!   the new term (persisted durably) and then admit. This is how a replica
//!   moves forward when the legitimate new primary streams to it;
//! * `incoming  < current` → **fenced**: a superseded primary, refused.
//!
//! The current term is held behind a [`TermStore`] so adoption is durable —
//! a replica that crashes after adopting term *N* comes back fencing stale
//! term *N-1* records rather than briefly accepting them. Production wires the
//! file-backed store alongside the node's other durable replication state;
//! tests use the in-memory store.

pub use reddb_file::FileTermStore;

/// The boundary at which a term-stamped message is being admitted. Only
/// affects diagnostics — the term rule is identical at both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceBoundary {
    /// A WAL/logical record being applied on a replica.
    Apply,
    /// A replication stream handshake declaring the streamer's term.
    Handshake,
    /// A serverless writer lease attempting to mutate under its lease term.
    Lease,
}

impl FenceBoundary {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Handshake => "handshake",
            Self::Lease => "lease",
        }
    }
}

/// The one shared stale-term predicate: only a strictly older incoming term is
/// stale. Equal terms are retries; newer terms are adoption candidates.
#[inline]
pub fn term_is_stale(incoming_term: u64, current_term: u64) -> bool {
    incoming_term < current_term
}

/// A replication-stream handshake as seen by the admitting replica.
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

/// Why the term fence refused a message: the incoming term is behind the
/// current term, so the sender is a deposed primary on a superseded timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleTermFenced {
    pub boundary: FenceBoundary,
    pub incoming_term: u64,
    pub current_term: u64,
}

impl std::fmt::Display for StaleTermFenced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "fenced stale-term {} message: incoming term {} is behind current term {}",
            self.boundary.as_str(),
            self.incoming_term,
            self.current_term
        )
    }
}

impl std::error::Error for StaleTermFenced {}

pub type StaleTermRejection = StaleTermFenced;

/// The verdict of the term fence for one incoming term-stamped message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceVerdict {
    /// `incoming == current`: admit at the live term.
    Admit { term: u64 },
    /// `incoming > current`: a newer timeline. The fence adopted `new_term`
    /// (persisting it) and the message is admitted under it.
    Adopt { new_term: u64 },
    /// `incoming < current`: stale — refused.
    Fenced(StaleTermFenced),
}

impl FenceVerdict {
    /// Was the message let through (either at the live term or after adopting
    /// a newer one)?
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admit { .. } | Self::Adopt { .. })
    }

    /// Was the message fenced as stale?
    pub fn is_fenced(&self) -> bool {
        matches!(self, Self::Fenced(_))
    }
}

/// Error reading or persisting the durable current term.
#[derive(Debug)]
pub enum TermStoreError {
    Io(std::io::Error),
    InvalidFormat(String),
}

impl std::fmt::Display for TermStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "term store io error: {err}"),
            Self::InvalidFormat(msg) => write!(f, "invalid term store format: {msg}"),
        }
    }
}

impl std::error::Error for TermStoreError {}

impl From<reddb_file::RdbFileError> for TermStoreError {
    fn from(value: reddb_file::RdbFileError) -> Self {
        match value {
            reddb_file::RdbFileError::Io(err) => Self::Io(err),
            reddb_file::RdbFileError::InvalidOperation(msg) => Self::InvalidFormat(msg),
        }
    }
}

/// Durable store for a node's current replication term. The default (when
/// nothing was ever written) is
/// [`DEFAULT_REPLICATION_TERM`](crate::replication::DEFAULT_REPLICATION_TERM),
/// matching the term records carry before any failover.
pub trait TermStore {
    fn load(&self) -> Result<u64, TermStoreError>;
    fn persist(&self, term: u64) -> Result<(), TermStoreError>;
}

/// In-memory term store for tests and ephemeral nodes.
#[derive(Debug)]
pub struct MemoryTermStore {
    inner: std::sync::Mutex<u64>,
}

impl Default for MemoryTermStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryTermStore {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(crate::replication::DEFAULT_REPLICATION_TERM),
        }
    }

    /// Seed an initial term — used by tests to simulate a node that already
    /// adopted a term before a restart.
    pub fn seeded(term: u64) -> Self {
        Self {
            inner: std::sync::Mutex::new(term),
        }
    }
}

impl TermStore for MemoryTermStore {
    fn load(&self) -> Result<u64, TermStoreError> {
        Ok(*self.inner.lock().expect("term store mutex"))
    }

    fn persist(&self, term: u64) -> Result<(), TermStoreError> {
        *self.inner.lock().expect("term store mutex") = term;
        Ok(())
    }
}

impl TermStore for FileTermStore {
    fn load(&self) -> Result<u64, TermStoreError> {
        self.load_file().map_err(TermStoreError::from)
    }

    fn persist(&self, term: u64) -> Result<(), TermStoreError> {
        self.persist_file(term).map_err(TermStoreError::from)
    }
}

/// The stale-term fence. Wraps a durable [`TermStore`] and applies the term
/// rule at the apply and handshake boundaries.
pub struct TermFence<S: TermStore> {
    store: S,
}

impl<S: TermStore> TermFence<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// The node's current (highest adopted) term.
    pub fn current_term(&self) -> Result<u64, TermStoreError> {
        self.store.load()
    }

    /// Classify `incoming_term` against the current term **without** mutating
    /// anything. Pure read — useful for probing a decision before committing
    /// to the adoption side-effect.
    pub fn classify(
        &self,
        boundary: FenceBoundary,
        incoming_term: u64,
    ) -> Result<FenceVerdict, TermStoreError> {
        let current = self.store.load()?;
        Ok(if term_is_stale(incoming_term, current) {
            FenceVerdict::Fenced(StaleTermFenced {
                boundary,
                incoming_term,
                current_term: current,
            })
        } else if incoming_term > current {
            FenceVerdict::Adopt {
                new_term: incoming_term,
            }
        } else {
            FenceVerdict::Admit { term: current }
        })
    }

    /// Admit (or fence) a term-stamped **record** at the apply boundary. On a
    /// newer term the fence adopts it durably before returning `Adopt`.
    pub fn admit_record(&self, incoming_term: u64) -> Result<FenceVerdict, TermStoreError> {
        self.admit(FenceBoundary::Apply, incoming_term)
    }

    /// Admit (or fence) a stream **handshake** declaring `incoming_term`. On a
    /// newer term the fence adopts it durably before returning `Adopt`.
    pub fn admit_handshake(&self, incoming_term: u64) -> Result<FenceVerdict, TermStoreError> {
        self.admit(FenceBoundary::Handshake, incoming_term)
    }

    /// Admit (or fence) a stream handshake carrying peer metadata.
    pub fn admit_stream_handshake(
        &self,
        handshake: &StreamHandshake,
    ) -> Result<FenceVerdict, TermStoreError> {
        self.admit_handshake(handshake.term)
    }

    /// Admit (or fence) a lease-backed write whose lease was taken under
    /// `lease_term`.
    pub fn admit_lease_write(&self, lease_term: u64) -> Result<FenceVerdict, TermStoreError> {
        self.admit(FenceBoundary::Lease, lease_term)
    }

    fn admit(
        &self,
        boundary: FenceBoundary,
        incoming_term: u64,
    ) -> Result<FenceVerdict, TermStoreError> {
        let verdict = self.classify(boundary, incoming_term)?;
        if let FenceVerdict::Adopt { new_term } = verdict {
            // Persist the adopted term before admitting so the advance is
            // durable: a crash right after this cannot un-adopt the new term.
            self.store.persist(new_term)?;
        }
        Ok(verdict)
    }

    /// Force the current term to `new_term`, persisting it. Used after a node
    /// re-syncs under a known term (e.g. a deposed primary rejoining as a
    /// replica) so it stops fencing the timeline it has now adopted. Never
    /// moves the term backwards.
    pub fn adopt(&self, new_term: u64) -> Result<(), TermStoreError> {
        let current = self.store.load()?;
        if new_term > current {
            self.store.persist(new_term)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(term: u64) -> TermFence<MemoryTermStore> {
        TermFence::new(MemoryTermStore::seeded(term))
    }

    // ---- Apply boundary ----

    #[test]
    fn apply_boundary_fences_stale_term() {
        let f = fence(5);
        let verdict = f.admit_record(4).unwrap();
        assert_eq!(
            verdict,
            FenceVerdict::Fenced(StaleTermFenced {
                boundary: FenceBoundary::Apply,
                incoming_term: 4,
                current_term: 5,
            })
        );
        assert!(verdict.is_fenced());
        // A fenced record must not move the current term.
        assert_eq!(f.current_term().unwrap(), 5);
    }

    #[test]
    fn apply_boundary_admits_current_term() {
        let f = fence(5);
        assert_eq!(f.admit_record(5).unwrap(), FenceVerdict::Admit { term: 5 });
        assert_eq!(f.current_term().unwrap(), 5);
    }

    #[test]
    fn apply_boundary_adopts_higher_term_durably() {
        let f = fence(5);
        assert_eq!(
            f.admit_record(8).unwrap(),
            FenceVerdict::Adopt { new_term: 8 }
        );
        // Adoption persisted: the old term is now fenced.
        assert_eq!(f.current_term().unwrap(), 8);
        assert!(f.admit_record(5).unwrap().is_fenced());
    }

    // ---- Handshake boundary ----

    #[test]
    fn handshake_boundary_fences_stale_term() {
        let f = fence(7);
        let verdict = f.admit_handshake(6).unwrap();
        assert_eq!(
            verdict,
            FenceVerdict::Fenced(StaleTermFenced {
                boundary: FenceBoundary::Handshake,
                incoming_term: 6,
                current_term: 7,
            })
        );
        assert!(verdict.is_fenced());
    }

    #[test]
    fn handshake_boundary_admits_current_and_adopts_higher() {
        let f = fence(7);
        assert_eq!(
            f.admit_handshake(7).unwrap(),
            FenceVerdict::Admit { term: 7 }
        );
        assert_eq!(
            f.admit_handshake(9).unwrap(),
            FenceVerdict::Adopt { new_term: 9 }
        );
        assert_eq!(f.current_term().unwrap(), 9);
    }

    // ---- End-to-end: returning ex-primary on a stale term ----

    #[test]
    fn returning_ex_primary_is_fenced_until_it_adopts_new_term() {
        // The replica adopted the new term 6 (handshake from the new primary).
        let f = fence(5);
        assert!(matches!(
            f.admit_handshake(6).unwrap(),
            FenceVerdict::Adopt { new_term: 6 }
        ));

        // The ex-primary returns on its stale term 5: both its handshake and
        // its records are fenced.
        assert!(f.admit_handshake(5).unwrap().is_fenced());
        assert!(f.admit_record(5).unwrap().is_fenced());

        // Only after it re-syncs and adopts the new term can it participate —
        // here it rejoins as a replica that has caught up to term 6.
        f.adopt(6).unwrap();
        assert!(f.admit_record(6).unwrap().is_admitted());
    }

    #[test]
    fn classify_is_pure_and_does_not_adopt() {
        let f = fence(3);
        // Classifying a higher term reports Adopt but must NOT persist it.
        assert_eq!(
            f.classify(FenceBoundary::Apply, 9).unwrap(),
            FenceVerdict::Adopt { new_term: 9 }
        );
        assert_eq!(f.current_term().unwrap(), 3, "classify must not mutate");
    }

    #[test]
    fn adopt_never_moves_term_backwards() {
        let f = fence(10);
        f.adopt(4).unwrap();
        assert_eq!(f.current_term().unwrap(), 10);
        f.adopt(12).unwrap();
        assert_eq!(f.current_term().unwrap(), 12);
    }

    // ---- File-backed durability ----

    #[test]
    fn file_term_store_round_trips_and_defaults() {
        let path = std::env::temp_dir().join(format!(
            "reddb-term-fence-{}-{}.json",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        let _ = std::fs::remove_file(&path);

        // Missing file → default base term.
        let store = FileTermStore::new(&path);
        assert_eq!(
            store.load().unwrap(),
            crate::replication::DEFAULT_REPLICATION_TERM
        );

        // Adopt across a "restart": a fresh store at the same path still
        // fences the old term.
        {
            let fence = TermFence::new(FileTermStore::new(&path));
            assert!(matches!(
                fence.admit_handshake(6).unwrap(),
                FenceVerdict::Adopt { new_term: 6 }
            ));
        }
        let reopened = TermFence::new(FileTermStore::new(&path));
        assert_eq!(reopened.current_term().unwrap(), 6);
        assert!(reopened.admit_record(5).unwrap().is_fenced());

        let _ = std::fs::remove_file(&path);
    }
}
