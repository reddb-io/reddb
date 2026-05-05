//! B+ Tree with MVCC
//!
//! A concurrent B+ tree implementation with multi-version concurrency control.
//!
//! # Design
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                         B+ Tree                                  │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                     Internal Nodes                               │
//! │   ┌─────┬─────┬─────┐                                           │
//! │   │ K1  │ K2  │ K3  │  Keys only (no values)                    │
//! │   └──┬──┴──┬──┴──┬──┘                                           │
//! │      │     │     │     Pointers to children                     │
//! │      ▼     ▼     ▼                                              │
//! │   ┌─────┐ ┌─────┐ ┌─────┐                                       │
//! │   │Leaf1│ │Leaf2│ │Leaf3│  Leaf nodes (with values)             │
//! │   └──┬──┘ └──┬──┘ └──┬──┘                                       │
//! │      │       │       │     Sibling pointers                     │
//! │      └───────┴───────┘                                          │
//! └─────────────────────────────────────────────────────────────────┘
//!
//! MVCC Version Chain:
//! ┌─────────────────┐     ┌─────────────────┐
//! │ Version (txn=5) │ ──▶ │ Version (txn=3) │ ──▶ null
//! │ value = "new"   │     │ value = "old"   │
//! └─────────────────┘     └─────────────────┘
//! ```
//!
//! # Features
//!
//! - Lock-free reads via MVCC snapshots
//! - Optimistic writes with version validation
//! - Automatic garbage collection of old versions
//! - Support for range scans and prefix queries

pub mod cursor;
pub mod gc;
pub mod node;
pub mod prefetch;
pub mod tree;
pub mod version;
pub mod visibility_map;

pub use cursor::{Cursor, CursorDirection};
pub use gc::{GarbageCollector, GcConfig, GcStats};
pub use node::{InternalNode, LeafNode, Node, NodeId, NodeType};
pub use tree::{BPlusTree, BTreeConfig, BTreeStats};
pub use version::{Timestamp, TxnId, Version, VersionChain, VersionVisibility};
