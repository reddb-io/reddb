//! WAL resource manager dispatch — Post-MVP credibility item.
//!
//! Mirrors PG's `xlogrecord.h::RmgrId` + `xlog.h::RmgrTable[]`
//! pattern. Each subsystem (heap, btree, vector, graph,
//! timeseries, queue) registers a *resource manager* that owns
//! redo / undo / desc logic for its WAL records. The recovery
//! loop dispatches by `RmgrId` instead of a giant
//! `match record_type` arm.
//!
//! ## Why
//!
//! reddb's `wal/recovery.rs` currently has one match per record
//! type. Adding a new subsystem (e.g. probabilistic data
//! structures) means editing recovery.rs and touching every
//! adjacent arm. With rmgr dispatch, a new subsystem just adds
//! a `ResourceManager` impl and registers it at startup.
//!
//! ## Design
//!
//! - `RmgrId` is a single-byte tag stored at the start of every
//!   WAL record body (after the common header).
//! - `ResourceManager` is a trait with three methods:
//!   - `redo(record)` — apply during forward recovery.
//!   - `undo(record)` — apply during transaction abort.
//!   - `desc(record)` — produce a human-readable string for
//!     diagnostics / pg_waldump-style tooling.
//! - `RmgrRegistry` is a fixed array indexed by `RmgrId` that
//!   the recovery loop consults to dispatch.
//!
//! ## Wiring
//!
//! Phase post-MVP wiring:
//! 1. Each subsystem (heap, btree, vector, …) implements
//!    `ResourceManager` and registers itself at startup via
//!    `rmgr::register(RmgrId::Heap, Box::new(HeapRmgr))`.
//! 2. `wal/recovery.rs::redo_loop` calls `rmgr::dispatch(record)`
//!    instead of pattern-matching on the record kind.
//! 3. Each existing match arm becomes the body of a `redo` impl.
//!
//! Today this module is the framework only — actual subsystem
//! impls live in their respective modules.

use std::sync::OnceLock;

/// Single-byte resource manager identifier. Matches PG's
/// RmgrId space (8-bit) so reddb shares the wire format
/// vocabulary if we ever ship a pg-compat WAL reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RmgrId {
    /// Heap rows (table data inserts/updates/deletes).
    Heap = 10,
    /// B-tree index records.
    Btree = 11,
    /// Vector index updates (HNSW / IVF / flat).
    Vector = 12,
    /// Graph subsystem (nodes, edges, properties).
    Graph = 13,
    /// Timeseries chunks + downsample state.
    Timeseries = 14,
    /// Queue subsystem (push, pop, ack).
    Queue = 15,
    /// Document store (put, delete, schema-less updates).
    Document = 16,
    /// KV store (set, delete, ttl).
    Kv = 17,
    /// Probabilistic data structures (HLL, CMS, Cuckoo, Bloom).
    Probabilistic = 18,
    /// Transaction commit / abort markers.
    Transaction = 50,
    /// Checkpoint metadata.
    Checkpoint = 51,
    /// Cross-cutting CDC / replication markers.
    Replication = 52,
    /// Reserved for future subsystems. Records carrying this
    /// tag are skipped during recovery instead of failing.
    Reserved = 255,
}

impl RmgrId {
    /// Try to convert a raw byte to an `RmgrId`. Returns `None`
    /// for unknown tags so recovery can decide whether to skip
    /// or hard-fail.
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            10 => Some(Self::Heap),
            11 => Some(Self::Btree),
            12 => Some(Self::Vector),
            13 => Some(Self::Graph),
            14 => Some(Self::Timeseries),
            15 => Some(Self::Queue),
            16 => Some(Self::Document),
            17 => Some(Self::Kv),
            18 => Some(Self::Probabilistic),
            50 => Some(Self::Transaction),
            51 => Some(Self::Checkpoint),
            52 => Some(Self::Replication),
            255 => Some(Self::Reserved),
            _ => None,
        }
    }

    /// Single-byte wire encoding.
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// Stable human-readable name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Heap => "heap",
            Self::Btree => "btree",
            Self::Vector => "vector",
            Self::Graph => "graph",
            Self::Timeseries => "timeseries",
            Self::Queue => "queue",
            Self::Document => "document",
            Self::Kv => "kv",
            Self::Probabilistic => "probabilistic",
            Self::Transaction => "transaction",
            Self::Checkpoint => "checkpoint",
            Self::Replication => "replication",
            Self::Reserved => "reserved",
        }
    }
}

/// Errors raised by resource manager dispatch.
#[derive(Debug)]
pub enum RmgrError {
    /// No resource manager registered for this id.
    Unregistered(RmgrId),
    /// Subsystem-specific failure during redo / undo.
    SubsystemFailure { rmgr: RmgrId, message: String },
}

impl std::fmt::Display for RmgrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unregistered(id) => write!(f, "no resource manager for {}", id.name()),
            Self::SubsystemFailure { rmgr, message } => {
                write!(f, "{} rmgr failed: {message}", rmgr.name())
            }
        }
    }
}

impl std::error::Error for RmgrError {}

/// Trait implemented by each subsystem that ships WAL records.
/// Every method receives the raw record bytes (including the
/// 1-byte rmgr id prefix) so the impl can decode in its
/// preferred format.
pub trait ResourceManager: Send + Sync {
    /// Apply `record` to the live state during forward recovery.
    /// Called by `wal/recovery.rs::redo_loop` for every record
    /// in the WAL whose LSN is past the last checkpoint.
    fn redo(&self, record: &[u8]) -> Result<(), RmgrError>;

    /// Apply `record`'s inverse to the live state during
    /// transaction abort. Most subsystems can leave this as
    /// the default no-op when their state is rebuilt
    /// idempotently from the WAL during recovery — only
    /// subsystems that carry side-effects across transactions
    /// (queue ack, replication ship) need to override.
    fn undo(&self, _record: &[u8]) -> Result<(), RmgrError> {
        Ok(())
    }

    /// Format a single record for diagnostic display
    /// (`reddb-waldump`-style tooling). Default returns a
    /// generic placeholder when the impl doesn't bother.
    fn desc(&self, record: &[u8]) -> String {
        format!("({} bytes opaque)", record.len())
    }

    /// Stable identifier for logging. Returns the rmgr's own
    /// `RmgrId::name()`. Trait method so trait objects can
    /// answer without an extra `dyn Any` cast.
    fn name(&self) -> &'static str;
}

/// Process-wide resource manager registry. Indexed by
/// `RmgrId.to_u8()` so dispatch is a single array lookup.
/// `OnceLock` ensures registration happens once at startup
/// and is then read-only — no runtime locking on the hot path.
static REGISTRY: OnceLock<RmgrRegistry> = OnceLock::new();

/// Read-only table of resource managers indexed by RmgrId.
pub struct RmgrRegistry {
    table: Vec<Option<Box<dyn ResourceManager>>>,
}

impl RmgrRegistry {
    /// Build a registry of `capacity` slots. 256 is the natural
    /// choice since `RmgrId` is u8.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut table = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            table.push(None);
        }
        Self { table }
    }

    /// Insert a resource manager at the given id. Replaces any
    /// existing entry. Builders typically chain `.register()`
    /// calls for each subsystem before sealing into a static.
    pub fn register(mut self, id: RmgrId, rmgr: Box<dyn ResourceManager>) -> Self {
        let idx = id.to_u8() as usize;
        if idx >= self.table.len() {
            self.table.resize_with(idx + 1, || None);
        }
        self.table[idx] = Some(rmgr);
        self
    }

    /// Look up a resource manager by id.
    pub fn get(&self, id: RmgrId) -> Option<&dyn ResourceManager> {
        self.table
            .get(id.to_u8() as usize)
            .and_then(|slot| slot.as_deref())
    }

    /// Dispatch a record to its registered manager's `redo`.
    /// Reads the first byte of `record` as the `RmgrId` tag.
    pub fn dispatch_redo(&self, record: &[u8]) -> Result<(), RmgrError> {
        let id_byte = record.first().copied().unwrap_or(0);
        let id = RmgrId::from_u8(id_byte).ok_or(RmgrError::Unregistered(RmgrId::Reserved))?;
        match self.get(id) {
            Some(rmgr) => rmgr.redo(record),
            None => Err(RmgrError::Unregistered(id)),
        }
    }
}

/// Install the process-wide registry. Call once during
/// `Database::open` after every subsystem has had a chance to
/// register its rmgr. Subsequent calls are no-ops (OnceLock).
pub fn install(registry: RmgrRegistry) -> Result<(), RmgrError> {
    REGISTRY
        .set(registry)
        .map_err(|_| RmgrError::SubsystemFailure {
            rmgr: RmgrId::Reserved,
            message: "rmgr registry already installed".to_string(),
        })
}

/// Look up the installed registry. Returns `None` until
/// `install()` is called — recovery code should panic in that
/// case because no recovery is possible without rmgrs.
pub fn registry() -> Option<&'static RmgrRegistry> {
    REGISTRY.get()
}

/// Dispatch a record to its registered manager. Convenience
/// wrapper used by the recovery loop.
pub fn dispatch_redo(record: &[u8]) -> Result<(), RmgrError> {
    match registry() {
        Some(reg) => reg.dispatch_redo(record),
        None => Err(RmgrError::SubsystemFailure {
            rmgr: RmgrId::Reserved,
            message: "rmgr registry not installed".to_string(),
        }),
    }
}
