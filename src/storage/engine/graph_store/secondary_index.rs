//! Secondary indexes on graph nodes for fast non-ID lookups.
//!
//! The primary `node_index` answers "give me node by id". Traversal queries
//! frequently need the *inverse*: "give me every node of type T" or "give
//! me every node whose label equals L". Without a secondary structure these
//! require a full scan over all pages.
//!
//! This module provides [`NodeSecondaryIndex`] — two inverted maps plus a
//! label bloom filter — wired into [`super::GraphStore`] insert/remove
//! paths. All reads are lock-free through `RwLock::read`.
//!
//! It implements [`crate::storage::index::IndexBase`] to participate in the
//! cross-structure planner cost model and bloom-based pruning.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use super::GraphNodeType;
use crate::storage::index::{BloomSegment, HasBloom, IndexBase, IndexKind, IndexStats};
use crate::storage::primitives::BloomFilter;

/// Inverted secondary indexes on graph nodes.
///
/// - `by_type`: `GraphNodeType → set of node ids`
/// - `by_label`: `label string → set of node ids`
/// - `label_bloom`: fast negative filter over distinct labels
pub struct NodeSecondaryIndex {
    by_type: RwLock<HashMap<GraphNodeType, HashSet<String>>>,
    by_label: RwLock<HashMap<String, HashSet<String>>>,
    label_bloom: RwLock<BloomSegment>,
    /// Counter for `IndexStats::entries` — total `(node, index_slot)` pairs.
    entry_count: RwLock<usize>,
}

impl NodeSecondaryIndex {
    /// Create an empty index sized for `expected_labels` distinct label
    /// values (used to size the bloom filter).
    pub fn new(expected_labels: usize) -> Self {
        Self {
            by_type: RwLock::new(HashMap::new()),
            by_label: RwLock::new(HashMap::new()),
            label_bloom: RwLock::new(BloomSegment::with_capacity(expected_labels.max(1024))),
            entry_count: RwLock::new(0),
        }
    }

    /// Record `(node_type, label, node_id)` in both inverted maps.
    ///
    /// Safe to call concurrently — each map takes its own write lock.
    /// Duplicate inserts are idempotent (sets).
    pub fn insert(&self, node_id: &str, node_type: GraphNodeType, label: &str) {
        let mut delta = 0usize;

        if let Ok(mut by_type) = self.by_type.write() {
            if by_type
                .entry(node_type)
                .or_default()
                .insert(node_id.to_string())
            {
                delta += 1;
            }
        }

        if let Ok(mut by_label) = self.by_label.write() {
            if by_label
                .entry(label.to_string())
                .or_default()
                .insert(node_id.to_string())
            {
                delta += 1;
            }
        }

        if let Ok(mut bloom) = self.label_bloom.write() {
            bloom.insert(label.as_bytes());
        }

        if delta > 0 {
            if let Ok(mut c) = self.entry_count.write() {
                *c = c.saturating_add(delta);
            }
        }
    }

    /// Remove a node from both inverted maps. Does not rebuild the bloom
    /// (bloom filters don't support removal — stale positives are harmless).
    pub fn remove(&self, node_id: &str, node_type: GraphNodeType, label: &str) {
        let mut delta = 0usize;

        if let Ok(mut by_type) = self.by_type.write() {
            if let Some(set) = by_type.get_mut(&node_type) {
                if set.remove(node_id) {
                    delta += 1;
                }
                if set.is_empty() {
                    by_type.remove(&node_type);
                }
            }
        }

        if let Ok(mut by_label) = self.by_label.write() {
            if let Some(set) = by_label.get_mut(label) {
                if set.remove(node_id) {
                    delta += 1;
                }
                if set.is_empty() {
                    by_label.remove(label);
                }
            }
        }

        if delta > 0 {
            if let Ok(mut c) = self.entry_count.write() {
                *c = c.saturating_sub(delta);
            }
        }
    }

    /// Return all node ids of a given type. O(1) lookup + clone.
    pub fn nodes_by_type(&self, node_type: GraphNodeType) -> Vec<String> {
        self.by_type
            .read()
            .map(|map| {
                map.get(&node_type)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Return all node ids with a given label. Uses the bloom as a pre-check
    /// — callers get an immediate empty `Vec` for definitely-absent labels.
    pub fn nodes_by_label(&self, label: &str) -> Vec<String> {
        if let Ok(bloom) = self.label_bloom.read() {
            if bloom.definitely_absent(label.as_bytes()) {
                return Vec::new();
            }
        }
        self.by_label
            .read()
            .map(|map| {
                map.get(label)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Cardinality of a type bucket (fast stat for the planner).
    pub fn count_by_type(&self, node_type: GraphNodeType) -> usize {
        self.by_type
            .read()
            .map(|m| m.get(&node_type).map(|s| s.len()).unwrap_or(0))
            .unwrap_or(0)
    }

    /// Number of distinct labels tracked.
    pub fn distinct_labels(&self) -> usize {
        self.by_label.read().map(|m| m.len()).unwrap_or(0)
    }

    /// Number of distinct node types tracked.
    pub fn distinct_types(&self) -> usize {
        self.by_type.read().map(|m| m.len()).unwrap_or(0)
    }

    /// Reset everything. Used by `rebuild_indexes`.
    pub fn clear(&self) {
        if let Ok(mut m) = self.by_type.write() {
            m.clear();
        }
        if let Ok(mut m) = self.by_label.write() {
            m.clear();
        }
        if let Ok(mut b) = self.label_bloom.write() {
            *b = BloomSegment::with_capacity(1024);
        }
        if let Ok(mut c) = self.entry_count.write() {
            *c = 0;
        }
    }
}

impl Default for NodeSecondaryIndex {
    fn default() -> Self {
        Self::new(1024)
    }
}

/// `HasBloom` impl pushes the label bloom through the shared trait so the
/// unified query planner can consult it uniformly.
///
/// Note: this returns `None` because the underlying bloom is behind a
/// `RwLock`. See [`NodeSecondaryIndex::may_contain_label`] for the actual
/// fast-path. The impl exists so call-sites that only know `dyn HasBloom`
/// can still reach the index via `IndexBase::definitely_absent`.
impl HasBloom for NodeSecondaryIndex {
    fn bloom_segment(&self) -> Option<&BloomSegment> {
        None
    }

    fn definitely_absent(&self, key: &[u8]) -> bool {
        self.label_bloom
            .read()
            .map(|b| b.definitely_absent(key))
            .unwrap_or(false)
    }
}

impl NodeSecondaryIndex {
    /// Public fast-path for label membership. Returns `false` iff the bloom
    /// proves the label was never inserted.
    pub fn may_contain_label(&self, label: &str) -> bool {
        !HasBloom::definitely_absent(self, label.as_bytes())
    }
}

impl IndexBase for NodeSecondaryIndex {
    fn name(&self) -> &str {
        "graph.node_secondary"
    }

    fn kind(&self) -> IndexKind {
        IndexKind::Inverted
    }

    fn stats(&self) -> IndexStats {
        let entries = self.entry_count.read().map(|c| *c).unwrap_or(0);
        let distinct_keys = self.distinct_labels() + self.distinct_types();
        IndexStats {
            entries,
            distinct_keys,
            approx_bytes: 0,
            kind: IndexKind::Inverted,
            has_bloom: true,
            index_correlation: 0.0,
        }
    }

    fn bloom(&self) -> Option<&BloomFilter> {
        // RwLock precludes handing out a raw reference to the inner bloom.
        // `definitely_absent` above routes around it.
        None
    }

    fn definitely_absent(&self, key_bytes: &[u8]) -> bool {
        <Self as HasBloom>::definitely_absent(self, key_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup_by_type() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("host:1", GraphNodeType::Host, "Web Server");
        idx.insert("host:2", GraphNodeType::Host, "DB Server");
        idx.insert("svc:1", GraphNodeType::Service, "HTTP");

        let hosts = idx.nodes_by_type(GraphNodeType::Host);
        assert_eq!(hosts.len(), 2);
        assert!(hosts.contains(&"host:1".to_string()));
        assert!(hosts.contains(&"host:2".to_string()));

        let services = idx.nodes_by_type(GraphNodeType::Service);
        assert_eq!(services, vec!["svc:1".to_string()]);

        assert!(idx.nodes_by_type(GraphNodeType::Vulnerability).is_empty());
    }

    #[test]
    fn lookup_by_label() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("host:1", GraphNodeType::Host, "Web Server");
        idx.insert("host:2", GraphNodeType::Host, "Web Server");
        idx.insert("host:3", GraphNodeType::Host, "DB Server");

        let web = idx.nodes_by_label("Web Server");
        assert_eq!(web.len(), 2);

        let db = idx.nodes_by_label("DB Server");
        assert_eq!(db, vec!["host:3".to_string()]);
    }

    #[test]
    fn bloom_prunes_absent_label() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("host:1", GraphNodeType::Host, "Web Server");

        // Existing label must never be reported absent.
        assert!(idx.may_contain_label("Web Server"));
        // Looking up an absent label returns empty (possibly via bloom).
        assert!(idx.nodes_by_label("DefinitelyNotThere").is_empty());
    }

    #[test]
    fn remove_shrinks_buckets() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("host:1", GraphNodeType::Host, "A");
        idx.insert("host:2", GraphNodeType::Host, "A");

        idx.remove("host:1", GraphNodeType::Host, "A");
        let remaining = idx.nodes_by_label("A");
        assert_eq!(remaining, vec!["host:2".to_string()]);
        assert_eq!(idx.count_by_type(GraphNodeType::Host), 1);

        idx.remove("host:2", GraphNodeType::Host, "A");
        assert!(idx.nodes_by_label("A").is_empty());
        assert_eq!(idx.count_by_type(GraphNodeType::Host), 0);
    }

    #[test]
    fn clear_resets_everything() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("a", GraphNodeType::Host, "L");
        idx.clear();
        assert_eq!(idx.count_by_type(GraphNodeType::Host), 0);
        assert!(idx.nodes_by_label("L").is_empty());
    }

    #[test]
    fn stats_reflect_insertions() {
        let idx = NodeSecondaryIndex::new(64);
        idx.insert("a", GraphNodeType::Host, "x");
        idx.insert("b", GraphNodeType::Service, "y");
        let s = idx.stats();
        // 2 inserts × (by_type + by_label) = 4 entries
        assert_eq!(s.entries, 4);
        assert!(s.has_bloom);
        assert_eq!(s.kind, IndexKind::Inverted);
    }

    #[test]
    fn concurrent_inserts_are_consistent() {
        use std::sync::Arc;
        use std::thread;

        let idx = Arc::new(NodeSecondaryIndex::new(1024));
        let mut handles = vec![];
        for t in 0..4 {
            let idx_c = Arc::clone(&idx);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let id = format!("node:{}:{}", t, i);
                    idx_c.insert(&id, GraphNodeType::Host, "bulk");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(idx.count_by_type(GraphNodeType::Host), 400);
        assert_eq!(idx.nodes_by_label("bulk").len(), 400);
    }
}
