//! Central index catalog — glues [`super::IndexBase`] implementations to
//! the cost-based planner's [`crate::storage::query::planner::StatsProvider`]
//! surface.
//!
//! Before this module, fase 1 defined the generic [`super::IndexBase`] trait
//! and fase 3.1 added a `StatsProvider` that the planner consults for real
//! statistics. But nothing *published* index stats into the provider — every
//! storage component kept its indexes private. The registry is the missing
//! glue: storages register any [`IndexBase`] trait object keyed by scope,
//! and the registry exposes those stats through the `StatsProvider` surface.
//!
//! # Scopes
//!
//! Indexes live in three scopes so the registry can match planner queries
//! without leaking storage-specific paths:
//!
//! - `Table { name, column }` — secondary indexes on table columns
//! - `Graph { collection }`   — adjacency / property indexes on graph nodes
//! - `Timeseries { series }`  — temporal indexes for a named series
//!
//! # Thread safety
//!
//! The registry is `Send + Sync` via `RwLock`. Reads are the hot path
//! (planner consults per query), writes are rare (startup, schema
//! migrations). Callers share an `Arc<IndexRegistry>`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::{IndexBase, IndexStats};

/// Where an index lives in the logical namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IndexScope {
    /// Secondary index on a table column.
    Table { table: String, column: String },
    /// Index attached to a graph collection (adjacency, label, type).
    Graph { collection: String },
    /// Temporal / property index on a named timeseries.
    Timeseries { series: String },
}

impl IndexScope {
    /// Build a table-scoped key.
    pub fn table(table: impl Into<String>, column: impl Into<String>) -> Self {
        Self::Table {
            table: table.into(),
            column: column.into(),
        }
    }

    /// Build a graph-scoped key.
    pub fn graph(collection: impl Into<String>) -> Self {
        Self::Graph {
            collection: collection.into(),
        }
    }

    /// Build a timeseries-scoped key.
    pub fn timeseries(series: impl Into<String>) -> Self {
        Self::Timeseries {
            series: series.into(),
        }
    }
}

/// A trait-object-friendly snapshot of an index. Stored in the registry so
/// readers get `Arc<dyn IndexBase>` without worrying about concrete types.
pub type SharedIndex = Arc<dyn IndexBase>;

/// Central index catalog.
pub struct IndexRegistry {
    entries: RwLock<HashMap<IndexScope, SharedIndex>>,
}

impl IndexRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) an index in the given scope. Returns the
    /// previous entry if one existed, so callers can drop it deterministically.
    pub fn register(&self, scope: IndexScope, index: SharedIndex) -> Option<SharedIndex> {
        self.entries
            .write()
            .ok()
            .and_then(|mut map| map.insert(scope, index))
    }

    /// Remove an index from the registry. Returns it if it existed.
    pub fn unregister(&self, scope: &IndexScope) -> Option<SharedIndex> {
        self.entries
            .write()
            .ok()
            .and_then(|mut map| map.remove(scope))
    }

    /// Look up an index by scope.
    pub fn get(&self, scope: &IndexScope) -> Option<SharedIndex> {
        self.entries
            .read()
            .ok()
            .and_then(|map| map.get(scope).cloned())
    }

    /// Convenience: fetch stats for a `(table, column)` index.
    pub fn table_index_stats(&self, table: &str, column: &str) -> Option<IndexStats> {
        self.get(&IndexScope::table(table, column))
            .map(|idx| idx.stats())
    }

    /// Convenience: fetch stats for a graph collection index.
    pub fn graph_index_stats(&self, collection: &str) -> Option<IndexStats> {
        self.get(&IndexScope::graph(collection))
            .map(|idx| idx.stats())
    }

    /// Convenience: fetch stats for a named timeseries index.
    pub fn timeseries_index_stats(&self, series: &str) -> Option<IndexStats> {
        self.get(&IndexScope::timeseries(series))
            .map(|idx| idx.stats())
    }

    /// Total number of registered indexes across every scope.
    pub fn len(&self) -> usize {
        self.entries.read().map(|m| m.len()).unwrap_or(0)
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over every `(scope, stats)` pair currently registered.
    /// Clones stats so callers don't hold the read lock.
    pub fn snapshot(&self) -> Vec<(IndexScope, IndexStats)> {
        self.entries
            .read()
            .map(|map| {
                map.iter()
                    .map(|(scope, idx)| (scope.clone(), idx.stats()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drop every registered index.
    pub fn clear(&self) {
        if let Ok(mut map) = self.entries.write() {
            map.clear();
        }
    }
}

impl Default for IndexRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index::{IndexBase, IndexKind, IndexStats};

    struct FakeIndex {
        name: String,
        kind: IndexKind,
        stats: IndexStats,
    }

    impl FakeIndex {
        fn new(name: &str, kind: IndexKind, distinct: usize) -> Self {
            Self {
                name: name.to_string(),
                kind,
                stats: IndexStats {
                    entries: distinct,
                    distinct_keys: distinct,
                    approx_bytes: 0,
                    kind,
                    has_bloom: false,
                    index_correlation: 0.0,
                },
            }
        }
    }

    impl IndexBase for FakeIndex {
        fn name(&self) -> &str {
            &self.name
        }
        fn kind(&self) -> IndexKind {
            self.kind
        }
        fn stats(&self) -> IndexStats {
            self.stats.clone()
        }
    }

    fn shared(name: &str, kind: IndexKind, distinct: usize) -> SharedIndex {
        Arc::new(FakeIndex::new(name, kind, distinct))
    }

    #[test]
    fn register_and_lookup_table_scope() {
        let reg = IndexRegistry::new();
        let prev = reg.register(
            IndexScope::table("users", "email"),
            shared("users.email", IndexKind::Hash, 1_000_000),
        );
        assert!(prev.is_none());

        let stats = reg.table_index_stats("users", "email").unwrap();
        assert_eq!(stats.distinct_keys, 1_000_000);
        assert_eq!(stats.kind, IndexKind::Hash);
    }

    #[test]
    fn register_replaces_existing() {
        let reg = IndexRegistry::new();
        reg.register(
            IndexScope::table("t", "c"),
            shared("old", IndexKind::BTree, 10),
        );
        let replaced = reg.register(
            IndexScope::table("t", "c"),
            shared("new", IndexKind::Hash, 100),
        );
        assert!(replaced.is_some());
        let stats = reg.table_index_stats("t", "c").unwrap();
        assert_eq!(stats.distinct_keys, 100);
        assert_eq!(stats.kind, IndexKind::Hash);
    }

    #[test]
    fn unregister_removes_entry() {
        let reg = IndexRegistry::new();
        reg.register(
            IndexScope::graph("hosts"),
            shared("adjacency", IndexKind::GraphAdjacency, 50),
        );
        assert_eq!(reg.len(), 1);

        let removed = reg.unregister(&IndexScope::graph("hosts"));
        assert!(removed.is_some());
        assert_eq!(reg.len(), 0);
        assert!(reg.graph_index_stats("hosts").is_none());
    }

    #[test]
    fn multi_scope_registration() {
        let reg = IndexRegistry::new();
        reg.register(
            IndexScope::table("users", "id"),
            shared("u.id", IndexKind::BTree, 10_000),
        );
        reg.register(
            IndexScope::graph("social"),
            shared("social.adj", IndexKind::GraphAdjacency, 5_000),
        );
        reg.register(
            IndexScope::timeseries("cpu.idle"),
            shared("cpu.temporal", IndexKind::Temporal, 2_000),
        );

        assert_eq!(reg.len(), 3);
        assert!(reg.table_index_stats("users", "id").is_some());
        assert!(reg.graph_index_stats("social").is_some());
        assert!(reg.timeseries_index_stats("cpu.idle").is_some());
    }

    #[test]
    fn snapshot_returns_all_entries() {
        let reg = IndexRegistry::new();
        reg.register(
            IndexScope::table("a", "x"),
            shared("a.x", IndexKind::Hash, 5),
        );
        reg.register(
            IndexScope::table("a", "y"),
            shared("a.y", IndexKind::Hash, 6),
        );

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        let kinds: Vec<IndexKind> = snap.iter().map(|(_, s)| s.kind).collect();
        assert!(kinds.iter().all(|k| *k == IndexKind::Hash));
    }

    #[test]
    fn clear_drops_everything() {
        let reg = IndexRegistry::new();
        reg.register(
            IndexScope::table("a", "x"),
            shared("a.x", IndexKind::Hash, 5),
        );
        reg.clear();
        assert!(reg.is_empty());
    }

    #[test]
    fn concurrent_registration_is_safe() {
        use std::thread;

        let reg = Arc::new(IndexRegistry::new());
        let mut handles = vec![];
        for t in 0..4u32 {
            let reg_c = Arc::clone(&reg);
            handles.push(thread::spawn(move || {
                for i in 0..50u32 {
                    let scope = IndexScope::table(format!("t{t}"), format!("c{i}"));
                    reg_c.register(
                        scope,
                        shared(&format!("t{t}.c{i}"), IndexKind::BTree, i as usize + 1),
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.len(), 200);
    }
}
