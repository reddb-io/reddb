//! Dynamic label registry for graph nodes and edges.
//!
//! Replaces the hardcoded `GraphNodeType` / `GraphEdgeType` enums with a
//! per-database catalog mapping arbitrary label strings to dense `u32`
//! identifiers. This unhooks the storage engine from any specific domain
//! (the prior enums were pentest-flavoured: `Host`, `Vulnerability`, …).
//!
//! # Design
//!
//! - Two independent namespaces: [`Namespace::Node`] and [`Namespace::Edge`].
//!   A label `"host"` registered as a node label is distinct from `"host"` as
//!   an edge label. Mirrors Neo4j semantics where node labels and relationship
//!   types live in different ID spaces.
//! - IDs are dense `u32`, allocated monotonically starting at `1`.
//!   `LabelId(0)` is reserved as the sentinel "unset/unknown" value so callers
//!   can use it as a default without clashing with a real label.
//! - Reserved ID range `1..=63` carries the legacy `GraphNodeType` /
//!   `GraphEdgeType` variants so on-disk format v1 records can be decoded
//!   into the new format without losing semantics. New labels start at
//!   [`FIRST_USER_LABEL_ID`] = 64.
//!
//! # Persistence
//!
//! The persisted byte frame lives in `reddb-file`; this module owns allocation,
//! lookup, legacy seeds, and concurrency.
//!
//! Persisting the registry is the caller's job — `GraphStore` writes it as
//! a sidecar page; embedded users can call [`LabelRegistry::encode`] /
//! [`LabelRegistry::decode`] directly.

use std::collections::HashMap;
use std::sync::RwLock;

/// Sentinel value for "no label assigned". Never returned by `intern`.
pub const UNSET_LABEL_ID: LabelId = LabelId(0);

/// First ID handed out to labels registered at runtime. IDs below this are
/// reserved for legacy `GraphNodeType` / `GraphEdgeType` variants so v1
/// page-format files can be migrated in place.
pub const FIRST_USER_LABEL_ID: u32 = 64;

/// Densely-numbered identifier for a label string within a [`LabelRegistry`].
///
/// Cheap to copy/hash/compare. Persisted as a 4-byte little-endian value in
/// the v2 graph page format (replaces the v1 `u8` enum discriminant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LabelId(pub u32);

impl LabelId {
    /// Construct from raw u32.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Raw u32 value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// True for `UNSET_LABEL_ID`.
    #[inline]
    pub const fn is_unset(self) -> bool {
        self.0 == 0
    }
}

/// Which catalog a label belongs to.
///
/// Node labels and edge labels live in separate ID spaces so the same
/// string (`"host"`) can be reused with different semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Namespace {
    Node = 0,
    Edge = 1,
}

impl Namespace {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Node),
            1 => Some(Self::Edge),
            _ => None,
        }
    }
}

/// Errors returned by [`LabelRegistry`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelRegistryError {
    /// Label exceeds [`MAX_LABEL_LEN`] bytes.
    LabelTooLong { len: usize, max: usize },
    /// Decoding hit malformed bytes at the given offset.
    Malformed { offset: usize, reason: &'static str },
    /// Internal lock was poisoned (process should treat this as fatal).
    LockPoisoned,
}

impl std::fmt::Display for LabelRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LabelTooLong { len, max } => {
                write!(f, "label too long: {} bytes (max {})", len, max)
            }
            Self::Malformed { offset, reason } => {
                write!(f, "malformed registry at offset {}: {}", offset, reason)
            }
            Self::LockPoisoned => write!(f, "label registry lock poisoned"),
        }
    }
}

impl std::error::Error for LabelRegistryError {}

/// Hard cap on label string length. Matches `MAX_LABEL_SIZE` in `graph_store.rs`.
pub const MAX_LABEL_LEN: usize = reddb_file::GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN;

/// Bidirectional `label ↔ LabelId` catalog with concurrent access.
///
/// Cloned cheaply via `Arc<LabelRegistry>` from `GraphStore`. Internally
/// uses a single `RwLock` covering both directions of the map plus the
/// next-id counter so that allocation + insertion is atomic.
#[derive(Debug)]
pub struct LabelRegistry {
    inner: RwLock<RegistryInner>,
}

#[derive(Debug)]
struct RegistryInner {
    /// `(namespace, label) → id`
    by_label: HashMap<(Namespace, String), LabelId>,
    /// `id → (namespace, label)`. `id` is the index, with `Vec[0]` reserved
    /// as the sentinel for `UNSET_LABEL_ID`.
    by_id: Vec<Option<(Namespace, String)>>,
    /// Next unused ID. Starts at [`FIRST_USER_LABEL_ID`] after legacy seed.
    next_id: u32,
}

impl LabelRegistry {
    /// New empty registry pre-seeded with legacy `GraphNodeType` and
    /// `GraphEdgeType` variant names so v1-format records decode into stable
    /// IDs without a separate migration step.
    pub fn with_legacy_seed() -> Self {
        let reg = Self::empty();
        // SAFETY of unwraps: labels are short literals well under MAX_LABEL_LEN
        // and the empty registry has no allocation pressure.
        for (raw, name) in LEGACY_NODE_LABELS {
            reg.intern_with_id(Namespace::Node, name, LabelId(legacy_node_id(*raw)))
                .expect("legacy node label seed");
        }
        for (raw, name) in LEGACY_EDGE_LABELS {
            reg.intern_with_id(Namespace::Edge, name, LabelId(legacy_edge_id(*raw)))
                .expect("legacy edge label seed");
        }
        // Bump next_id past the reserved range.
        if let Ok(mut g) = reg.inner.write() {
            g.next_id = FIRST_USER_LABEL_ID;
        }
        reg
    }

    /// New empty registry with no legacy seeding. Use only when callers do
    /// not need to decode v1 graph pages.
    pub fn empty() -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                by_label: HashMap::new(),
                // Index 0 is the sentinel — leave it None.
                by_id: vec![None],
                next_id: 1,
            }),
        }
    }

    /// Look up an existing label or allocate a new ID. Idempotent.
    pub fn intern(&self, ns: Namespace, label: &str) -> Result<LabelId, LabelRegistryError> {
        if label.len() > MAX_LABEL_LEN {
            return Err(LabelRegistryError::LabelTooLong {
                len: label.len(),
                max: MAX_LABEL_LEN,
            });
        }
        let mut g = self
            .inner
            .write()
            .map_err(|_| LabelRegistryError::LockPoisoned)?;
        if let Some(&id) = g.by_label.get(&(ns, label.to_string())) {
            return Ok(id);
        }
        let id = LabelId(g.next_id);
        g.next_id = g
            .next_id
            .checked_add(1)
            .expect("LabelId u32 space exhausted (>4B labels)");
        let key = (ns, label.to_string());
        g.by_label.insert(key.clone(), id);
        let idx = id.0 as usize;
        if g.by_id.len() <= idx {
            g.by_id.resize(idx + 1, None);
        }
        g.by_id[idx] = Some(key);
        Ok(id)
    }

    /// Look up the ID for an existing label. `None` if not interned.
    pub fn lookup(&self, ns: Namespace, label: &str) -> Option<LabelId> {
        let g = self.inner.read().ok()?;
        g.by_label.get(&(ns, label.to_string())).copied()
    }

    /// Resolve an ID back to its `(namespace, label)`. `None` for unknown
    /// or sentinel IDs.
    pub fn resolve(&self, id: LabelId) -> Option<(Namespace, String)> {
        if id.is_unset() {
            return None;
        }
        let g = self.inner.read().ok()?;
        g.by_id.get(id.0 as usize).cloned().flatten()
    }

    /// Resolve an ID to just its label string, scoped to a namespace.
    /// Returns `None` if the ID belongs to a different namespace.
    pub fn label_of(&self, ns: Namespace, id: LabelId) -> Option<String> {
        self.resolve(id)
            .filter(|(found_ns, _)| *found_ns == ns)
            .map(|(_, l)| l)
    }

    /// Total interned labels (across both namespaces, excludes sentinel).
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.by_label.len()).unwrap_or(0)
    }

    /// True if no labels have been interned.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Translate a v1 `GraphNodeType` discriminant (`0..=8`) into the
    /// reserved `LabelId` it was seeded with. Returns `UNSET_LABEL_ID` for
    /// unknown discriminants (forward-compat).
    pub fn legacy_node_label_id(disc: u8) -> LabelId {
        if (disc as usize) < LEGACY_NODE_LABELS.len() {
            LabelId(legacy_node_id(disc))
        } else {
            UNSET_LABEL_ID
        }
    }

    /// Translate a v1 `GraphEdgeType` discriminant (`0..=9`) into the
    /// reserved `LabelId` it was seeded with.
    pub fn legacy_edge_label_id(disc: u8) -> LabelId {
        if (disc as usize) < LEGACY_EDGE_LABELS.len() {
            LabelId(legacy_edge_id(disc))
        } else {
            UNSET_LABEL_ID
        }
    }

    /// Serialize the catalog to the canonical `reddb-file` label-registry frame.
    pub fn encode(&self) -> Result<Vec<u8>, LabelRegistryError> {
        let g = self
            .inner
            .read()
            .map_err(|_| LabelRegistryError::LockPoisoned)?;
        // Count only populated slots.
        let entries: Vec<reddb_file::GraphLabelRegistryEntry> = g
            .by_id
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                slot.as_ref()
                    .map(|(ns, label)| reddb_file::GraphLabelRegistryEntry {
                        id: i as u32,
                        namespace: *ns as u8,
                        label: label.clone(),
                    })
            })
            .collect();
        reddb_file::encode_graph_label_registry_frame(&entries).map_err(Self::map_frame_error)
    }

    /// Inverse of [`encode`]. Returns a fresh registry; the legacy seed is
    /// *not* re-applied (caller decides whether incoming bytes already
    /// contain the legacy entries).
    pub fn decode(data: &[u8]) -> Result<Self, LabelRegistryError> {
        let reg = Self::empty();
        let entries =
            reddb_file::decode_graph_label_registry_frame(data).map_err(Self::map_frame_error)?;
        for entry in entries {
            let ns = Namespace::from_u8(entry.namespace).ok_or(LabelRegistryError::Malformed {
                offset: 0,
                reason: "unknown namespace",
            })?;
            reg.intern_with_id(ns, &entry.label, LabelId(entry.id))?;
        }
        // Bump next_id to one past the highest ID seen so subsequent
        // intern() calls do not collide with restored entries.
        if let Ok(mut g) = reg.inner.write() {
            let max_id = g
                .by_id
                .iter()
                .enumerate()
                .filter_map(|(i, slot)| slot.as_ref().map(|_| i as u32))
                .max()
                .unwrap_or(0);
            g.next_id = max_id.saturating_add(1).max(FIRST_USER_LABEL_ID);
        }
        Ok(reg)
    }

    fn map_frame_error(e: reddb_file::GraphLabelRegistryFrameError) -> LabelRegistryError {
        match e {
            reddb_file::GraphLabelRegistryFrameError::LabelTooLong { len, max } => {
                LabelRegistryError::LabelTooLong { len, max }
            }
            reddb_file::GraphLabelRegistryFrameError::Malformed { offset, reason } => {
                LabelRegistryError::Malformed { offset, reason }
            }
        }
    }

    /// Insert at a specific ID. Used by [`with_legacy_seed`] and [`decode`].
    /// Errors if the ID is already taken with a different `(ns, label)`.
    fn intern_with_id(
        &self,
        ns: Namespace,
        label: &str,
        id: LabelId,
    ) -> Result<(), LabelRegistryError> {
        if label.len() > MAX_LABEL_LEN {
            return Err(LabelRegistryError::LabelTooLong {
                len: label.len(),
                max: MAX_LABEL_LEN,
            });
        }
        let mut g = self
            .inner
            .write()
            .map_err(|_| LabelRegistryError::LockPoisoned)?;
        let idx = id.0 as usize;
        if g.by_id.len() <= idx {
            g.by_id.resize(idx + 1, None);
        }
        let key = (ns, label.to_string());
        if let Some(existing) = &g.by_id[idx] {
            if existing != &key {
                return Err(LabelRegistryError::Malformed {
                    offset: 0,
                    reason: "id collision with different label",
                });
            }
            return Ok(());
        }
        g.by_id[idx] = Some(key.clone());
        g.by_label.insert(key, id);
        Ok(())
    }
}

impl Default for LabelRegistry {
    fn default() -> Self {
        Self::with_legacy_seed()
    }
}

// ===== Legacy seed tables ===================================================
//
// These mirror the v1 `GraphNodeType` and `GraphEdgeType` enums *only* so
// that v1-format graph pages on disk can be decoded into the new label-id
// world without a separate migration tool. They are intentionally NOT
// referenced anywhere else — new code should call `intern()` with whatever
// label string the user picks.

const LEGACY_NODE_LABELS: &[(u8, &str)] = &[
    (0, "host"),
    (1, "service"),
    (2, "credential"),
    (3, "vulnerability"),
    (4, "endpoint"),
    (5, "technology"),
    (6, "user"),
    (7, "domain"),
    (8, "certificate"),
];

const LEGACY_EDGE_LABELS: &[(u8, &str)] = &[
    (0, "has_service"),
    (1, "has_endpoint"),
    (2, "uses_tech"),
    (3, "auth_access"),
    (4, "affected_by"),
    (5, "contains"),
    (6, "connects_to"),
    (7, "related_to"),
    (8, "has_user"),
    (9, "has_cert"),
];

// Legacy IDs are packed 1..=9 (nodes) and 10..=19 (edges), leaving 20..=63
// as headroom in the reserved range. `FIRST_USER_LABEL_ID = 64` is where
// runtime allocations begin.
fn legacy_node_id(disc: u8) -> u32 {
    1 + disc as u32
}

fn legacy_edge_id(disc: u8) -> u32 {
    10 + disc as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_entries_but_sentinel_resolves_to_none() {
        let r = LabelRegistry::empty();
        assert!(r.is_empty());
        assert_eq!(r.resolve(UNSET_LABEL_ID), None);
        assert_eq!(r.lookup(Namespace::Node, "anything"), None);
    }

    #[test]
    fn intern_is_idempotent() {
        let r = LabelRegistry::empty();
        let a = r.intern(Namespace::Node, "order").unwrap();
        let b = r.intern(Namespace::Node, "order").unwrap();
        assert_eq!(a, b);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn namespaces_are_independent() {
        let r = LabelRegistry::empty();
        let n = r.intern(Namespace::Node, "host").unwrap();
        let e = r.intern(Namespace::Edge, "host").unwrap();
        assert_ne!(
            n, e,
            "same label in different namespaces must get distinct ids"
        );
        assert_eq!(r.label_of(Namespace::Node, n).as_deref(), Some("host"));
        assert_eq!(r.label_of(Namespace::Edge, e).as_deref(), Some("host"));
        assert_eq!(r.label_of(Namespace::Node, e), None);
    }

    #[test]
    fn legacy_seed_populates_reserved_range() {
        let r = LabelRegistry::with_legacy_seed();
        // Old GraphNodeType::Host (disc=0) → "host" at LabelId(1).
        let host_id = r.lookup(Namespace::Node, "host").unwrap();
        assert_eq!(host_id, LabelId(1));
        assert_eq!(LabelRegistry::legacy_node_label_id(0), host_id);
        // Old GraphEdgeType::HasService (disc=0) → "has_service" at LabelId(10).
        let edge_id = r.lookup(Namespace::Edge, "has_service").unwrap();
        assert_eq!(edge_id, LabelId(10));
        assert_eq!(LabelRegistry::legacy_edge_label_id(0), edge_id);
    }

    #[test]
    fn user_labels_start_at_first_user_id() {
        let r = LabelRegistry::with_legacy_seed();
        let id = r.intern(Namespace::Node, "order").unwrap();
        assert_eq!(id, LabelId(FIRST_USER_LABEL_ID));
        let id2 = r.intern(Namespace::Node, "product").unwrap();
        assert_eq!(id2, LabelId(FIRST_USER_LABEL_ID + 1));
    }

    #[test]
    fn round_trip_encode_decode() {
        let r = LabelRegistry::with_legacy_seed();
        r.intern(Namespace::Node, "order").unwrap();
        r.intern(Namespace::Node, "product").unwrap();
        r.intern(Namespace::Edge, "purchased").unwrap();
        let bytes = r.encode().unwrap();
        let restored = LabelRegistry::decode(&bytes).unwrap();
        assert_eq!(restored.len(), r.len());
        assert_eq!(
            restored.lookup(Namespace::Node, "order"),
            r.lookup(Namespace::Node, "order")
        );
        assert_eq!(
            restored.lookup(Namespace::Edge, "purchased"),
            r.lookup(Namespace::Edge, "purchased")
        );
        // After decode, next intern must NOT reuse a restored ID.
        let new_id = restored.intern(Namespace::Node, "shipment").unwrap();
        let prior_max = r.lookup(Namespace::Node, "product").unwrap();
        assert!(new_id.0 > prior_max.0);
    }

    #[test]
    fn decode_rejects_truncated_input() {
        let bad = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            LabelRegistry::decode(&bad),
            Err(LabelRegistryError::Malformed { .. })
        ));
    }

    #[test]
    fn decode_rejects_invalid_namespace() {
        // count=1, id=64, ns=99 (invalid), len=4, "test"
        let mut bad = Vec::new();
        bad.extend_from_slice(&1u32.to_le_bytes());
        bad.extend_from_slice(&64u32.to_le_bytes());
        bad.push(99);
        bad.extend_from_slice(&4u16.to_le_bytes());
        bad.extend_from_slice(b"test");
        let err = LabelRegistry::decode(&bad).unwrap_err();
        assert!(matches!(err, LabelRegistryError::Malformed { .. }));
    }

    #[test]
    fn label_too_long_is_rejected() {
        let r = LabelRegistry::empty();
        let big = "x".repeat(MAX_LABEL_LEN + 1);
        assert!(matches!(
            r.intern(Namespace::Node, &big),
            Err(LabelRegistryError::LabelTooLong { .. })
        ));
    }

    #[test]
    fn concurrent_intern_yields_consistent_ids() {
        use std::sync::Arc;
        use std::thread;

        let r = Arc::new(LabelRegistry::empty());
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let r = Arc::clone(&r);
                thread::spawn(move || {
                    let mut ids = Vec::new();
                    for j in 0..50 {
                        let label = format!("label_{}_{}", i % 4, j);
                        ids.push(r.intern(Namespace::Node, &label).unwrap());
                    }
                    ids
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // Every distinct label should map to a unique LabelId. With i % 4
        // we have 4 distinct prefixes × 50 suffixes = 200 distinct labels.
        let mut seen_ids = std::collections::HashSet::new();
        for i in 0..4 {
            for j in 0..50 {
                let label = format!("label_{}_{}", i, j);
                let id = r.lookup(Namespace::Node, &label).unwrap();
                assert!(seen_ids.insert(id), "duplicate id {:?} for {}", id, label);
            }
        }
        assert_eq!(seen_ids.len(), 200);
    }

    #[test]
    fn unset_id_never_resolves() {
        let r = LabelRegistry::with_legacy_seed();
        assert!(UNSET_LABEL_ID.is_unset());
        assert_eq!(r.resolve(UNSET_LABEL_ID), None);
        assert_eq!(r.label_of(Namespace::Node, UNSET_LABEL_ID), None);
    }
}
