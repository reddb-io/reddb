//! Unified ID Types for Storage Layer
//!
//! This module provides type-safe ID wrappers for all storage concepts.
//! Using newtype wrappers prevents accidental mixing of semantically
//! different IDs (e.g., using a NodeId where a VectorId was expected).
//!
//! # Design Rationale
//!
//! Previously, IDs were scattered across modules as simple type aliases:
//! - `NodeId` was defined in both btree/node.rs and engine/hnsw.rs
//! - `TxnId` was duplicated 5 times across transaction modules
//! - `VectorId` existed in both deprecated vector/ and engine/vector_store.rs
//!
//! This centralization provides:
//! 1. Single source of truth
//! 2. Type safety (cannot accidentally mix ID types)
//! 3. Easy to add traits uniformly (Display, Hash, etc.)
//!
//! The wrappers support arithmetic operations for practical usage while
//! maintaining type distinction between different ID domains.

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// B+ Tree IDs
// ============================================================================

/// Node ID for B+ tree nodes (internal and leaf nodes).
///
/// B+ tree nodes form a hierarchical structure with internal nodes
/// containing keys and child pointers, and leaf nodes containing
/// the actual key-value pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct BTreeNodeId(pub u64);

impl BTreeNodeId {
    /// Create a new BTreeNodeId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BTreeNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "btree:{}", self.0)
    }
}

impl From<u64> for BTreeNodeId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<BTreeNodeId> for u64 {
    fn from(id: BTreeNodeId) -> Self {
        id.0
    }
}

/// Global counter for B+ tree node IDs
static BTREE_NODE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate next B+ tree node ID
pub fn next_btree_node_id() -> BTreeNodeId {
    BTreeNodeId(BTREE_NODE_ID_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ============================================================================
// HNSW Graph IDs
// ============================================================================

/// Node ID for HNSW (Hierarchical Navigable Small World) graph nodes.
///
/// HNSW nodes represent vectors in the approximate nearest neighbor
/// search structure. Each node exists in one or more layers of the
/// hierarchical graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct HnswNodeId(pub u64);

impl HnswNodeId {
    /// Create a new HnswNodeId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for HnswNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hnsw:{}", self.0)
    }
}

impl From<u64> for HnswNodeId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<HnswNodeId> for u64 {
    fn from(id: HnswNodeId) -> Self {
        id.0
    }
}

// ============================================================================
// Transaction IDs
// ============================================================================

/// Transaction ID for MVCC (Multi-Version Concurrency Control).
///
/// Transaction IDs are monotonically increasing and used to:
/// - Identify which transaction created/modified a version
/// - Determine version visibility during reads
/// - Coordinate distributed transactions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct TxnId(pub u64);

impl TxnId {
    /// Create a new TxnId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The null/initial transaction ID
    pub const ZERO: TxnId = TxnId(0);

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Check if this is the null/initial transaction
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for TxnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "txn:{}", self.0)
    }
}

impl From<u64> for TxnId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<TxnId> for u64 {
    fn from(id: TxnId) -> Self {
        id.0
    }
}

// Arithmetic operations for TxnId
impl Add<u64> for TxnId {
    type Output = TxnId;
    fn add(self, rhs: u64) -> TxnId {
        TxnId(self.0 + rhs)
    }
}

impl AddAssign<u64> for TxnId {
    fn add_assign(&mut self, rhs: u64) {
        self.0 += rhs;
    }
}

impl Sub<u64> for TxnId {
    type Output = TxnId;
    fn sub(self, rhs: u64) -> TxnId {
        TxnId(self.0 - rhs)
    }
}

/// Global counter for transaction IDs
static TXN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate next transaction ID
pub fn next_txn_id() -> TxnId {
    TxnId(TXN_ID_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ============================================================================
// Vector IDs
// ============================================================================

/// Vector ID for vector storage and similarity search.
///
/// Each vector in the storage engine has a unique VectorId that
/// persists across index rebuilds and can be used to retrieve
/// the original vector data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct VectorId(pub u64);

impl VectorId {
    /// Create a new VectorId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for VectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vec:{}", self.0)
    }
}

impl From<u64> for VectorId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<VectorId> for u64 {
    fn from(id: VectorId) -> Self {
        id.0
    }
}

// ============================================================================
// Segment IDs
// ============================================================================

/// Segment ID for data segment identification.
///
/// Segments are logical partitions of data used for:
/// - Organizing related records together
/// - Enabling targeted queries
/// - Supporting segment-level operations (compaction, gc)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct SegmentId(pub u64);

impl SegmentId {
    /// Create a new SegmentId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seg:{}", self.0)
    }
}

impl From<u64> for SegmentId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<SegmentId> for u64 {
    fn from(id: SegmentId) -> Self {
        id.0
    }
}

// ============================================================================
// Page IDs
// ============================================================================

/// Page ID for storage page identification.
///
/// Pages are the fundamental unit of storage I/O. Each page has
/// a fixed size (typically 4KB or 8KB) and contains records or
/// index data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct PageId(pub u64);

impl PageId {
    /// Create a new PageId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The null page ID (no page)
    pub const NULL: PageId = PageId(0);

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Check if this is the null page
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page:{}", self.0)
    }
}

impl From<u64> for PageId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<PageId> for u64 {
    fn from(id: PageId) -> Self {
        id.0
    }
}

// ============================================================================
// Entity IDs (graph/relational)
// ============================================================================

/// Entity ID for graph nodes and relational entities.
///
/// Used in the graph storage layer to identify vertices
/// and their relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct EntityId(pub u64);

impl EntityId {
    /// Create a new EntityId
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entity:{}", self.0)
    }
}

impl From<u64> for EntityId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<EntityId> for u64 {
    fn from(id: EntityId) -> Self {
        id.0
    }
}

// ============================================================================
// Timestamp (logical clock)
// ============================================================================

/// Logical timestamp for MVCC versioning.
///
/// Timestamps are monotonically increasing values used to
/// order events and determine version visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Create a new Timestamp
    pub const fn new(ts: u64) -> Self {
        Self(ts)
    }

    /// The epoch (time zero)
    pub const EPOCH: Timestamp = Timestamp(0);

    /// Maximum timestamp
    pub const MAX: Timestamp = Timestamp(u64::MAX);

    /// Get the raw u64 value
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Check if this is the epoch
    pub const fn is_epoch(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ts:{}", self.0)
    }
}

impl From<u64> for Timestamp {
    fn from(ts: u64) -> Self {
        Self(ts)
    }
}

impl From<Timestamp> for u64 {
    fn from(ts: Timestamp) -> Self {
        ts.0
    }
}

// Arithmetic operations for Timestamp
impl Add<u64> for Timestamp {
    type Output = Timestamp;
    fn add(self, rhs: u64) -> Timestamp {
        Timestamp(self.0 + rhs)
    }
}

impl Add<Timestamp> for Timestamp {
    type Output = Timestamp;
    fn add(self, rhs: Timestamp) -> Timestamp {
        Timestamp(self.0 + rhs.0)
    }
}

impl AddAssign<u64> for Timestamp {
    fn add_assign(&mut self, rhs: u64) {
        self.0 += rhs;
    }
}

impl Sub<u64> for Timestamp {
    type Output = Timestamp;
    fn sub(self, rhs: u64) -> Timestamp {
        Timestamp(self.0 - rhs)
    }
}

impl Sub<Timestamp> for Timestamp {
    type Output = Timestamp;
    fn sub(self, rhs: Timestamp) -> Timestamp {
        Timestamp(self.0 - rhs.0)
    }
}

impl SubAssign<u64> for Timestamp {
    fn sub_assign(&mut self, rhs: u64) {
        self.0 -= rhs;
    }
}

impl Timestamp {
    /// Saturating subtraction
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Timestamp(self.0.saturating_sub(rhs.0))
    }

    /// Minimum of two timestamps
    pub fn min(self, other: Self) -> Self {
        Timestamp(self.0.min(other.0))
    }

    /// Maximum of two timestamps
    pub fn max(self, other: Self) -> Self {
        Timestamp(self.0.max(other.0))
    }
}

/// Global logical timestamp counter
static GLOBAL_TIMESTAMP: AtomicU64 = AtomicU64::new(1);

/// Get next timestamp
pub fn next_timestamp() -> Timestamp {
    Timestamp(GLOBAL_TIMESTAMP.fetch_add(1, Ordering::SeqCst))
}

/// Get current timestamp without incrementing
pub fn current_timestamp() -> Timestamp {
    Timestamp(GLOBAL_TIMESTAMP.load(Ordering::SeqCst))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_btree_node_id() {
        let id1 = BTreeNodeId::new(42);
        let id2 = BTreeNodeId::from(42u64);
        assert_eq!(id1, id2);
        assert_eq!(id1.get(), 42);
        assert_eq!(format!("{}", id1), "btree:42");
    }

    #[test]
    fn test_hnsw_node_id() {
        let id = HnswNodeId::new(100);
        assert_eq!(id.get(), 100);
        assert_eq!(format!("{}", id), "hnsw:100");
    }

    #[test]
    fn test_txn_id() {
        assert!(TxnId::ZERO.is_zero());
        let id = TxnId::new(5);
        assert!(!id.is_zero());
        assert_eq!(format!("{}", id), "txn:5");
    }

    #[test]
    fn test_vector_id() {
        let id = VectorId::new(999);
        assert_eq!(id.get(), 999);
        assert_eq!(u64::from(id), 999);
    }

    #[test]
    fn test_page_id() {
        assert!(PageId::NULL.is_null());
        let id = PageId::new(1);
        assert!(!id.is_null());
    }

    #[test]
    fn test_timestamp() {
        assert!(Timestamp::EPOCH.is_epoch());
        let ts = Timestamp::new(100);
        assert!(!ts.is_epoch());
        assert!(ts < Timestamp::MAX);
    }

    #[test]
    fn test_id_generation() {
        let id1 = next_btree_node_id();
        let id2 = next_btree_node_id();
        assert!(id2.get() > id1.get());

        let txn1 = next_txn_id();
        let txn2 = next_txn_id();
        assert!(txn2.get() > txn1.get());

        let ts1 = next_timestamp();
        let ts2 = next_timestamp();
        assert!(ts2.get() > ts1.get());
    }

    #[test]
    fn test_type_safety() {
        // These types should NOT be interchangeable at compile time
        // (This is a conceptual test - actual type errors are compile-time)
        let btree_id = BTreeNodeId::new(1);
        let hnsw_id = HnswNodeId::new(1);
        let txn_id = TxnId::new(1);

        // They have the same underlying value but are different types
        assert_eq!(btree_id.get(), hnsw_id.get());
        assert_eq!(btree_id.get(), txn_id.get());

        // But they format differently, showing their semantic meaning
        assert_ne!(format!("{}", btree_id), format!("{}", hnsw_id));
        assert_ne!(format!("{}", btree_id), format!("{}", txn_id));
    }
}
