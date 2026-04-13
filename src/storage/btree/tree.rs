//! B+ Tree Implementation
//!
//! Concurrent B+ tree with MVCC support.

use super::node::{InternalNode, LeafEntry, LeafNode, Node, NodeId, NodeType};
use super::version::{next_timestamp, ActiveTransaction, Snapshot, Timestamp, TxnId};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

fn recover_read_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn recover_write_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// B+ Tree configuration
#[derive(Debug, Clone)]
pub struct BTreeConfig {
    /// Order (maximum children per node)
    pub order: usize,
    /// Enable MVCC
    pub mvcc_enabled: bool,
    /// GC watermark age (timestamps older than this are eligible for GC)
    pub gc_watermark_age: Timestamp,
}

impl BTreeConfig {
    /// Create default config
    pub fn new() -> Self {
        Self {
            order: 128,
            mvcc_enabled: true,
            gc_watermark_age: Timestamp(1000),
        }
    }

    /// Set order
    pub fn with_order(mut self, order: usize) -> Self {
        self.order = order.max(4); // Minimum order 4
        self
    }

    /// Enable/disable MVCC
    pub fn with_mvcc(mut self, enabled: bool) -> Self {
        self.mvcc_enabled = enabled;
        self
    }
}

impl Default for BTreeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// B+ Tree statistics
#[derive(Debug, Clone, Default)]
pub struct BTreeStats {
    /// Number of entries
    pub entries: u64,
    /// Number of nodes
    pub nodes: u64,
    /// Number of internal nodes
    pub internal_nodes: u64,
    /// Number of leaf nodes
    pub leaf_nodes: u64,
    /// Tree height
    pub height: u32,
    /// Total versions (MVCC)
    pub versions: u64,
    /// Insert count
    pub inserts: u64,
    /// Update count
    pub updates: u64,
    /// Delete count
    pub deletes: u64,
    /// Get count
    pub gets: u64,
    /// Range scan count
    pub range_scans: u64,
}

/// B+ Tree with MVCC
pub struct BPlusTree<K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    /// Configuration
    config: BTreeConfig,
    /// Root node ID
    pub(crate) root: RwLock<Option<NodeId>>,
    /// Node storage
    nodes: RwLock<HashMap<NodeId, Arc<RwLock<Node<K, V>>>>>,
    /// First leaf (for full scans)
    pub(crate) first_leaf: RwLock<Option<NodeId>>,
    /// Statistics
    stats: RwLock<BTreeStats>,
    /// Active transactions
    active_txns: RwLock<HashMap<TxnId, ActiveTransaction>>,
    /// Next transaction ID
    next_txn_id: RwLock<TxnId>,
}

impl<K, V> BPlusTree<K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    /// Create new empty tree
    pub fn new(config: BTreeConfig) -> Self {
        Self {
            config,
            root: RwLock::new(None),
            nodes: RwLock::new(HashMap::new()),
            first_leaf: RwLock::new(None),
            stats: RwLock::new(BTreeStats::default()),
            active_txns: RwLock::new(HashMap::new()),
            next_txn_id: RwLock::new(TxnId(1)),
        }
    }

    /// Create with default config
    pub fn with_default_config() -> Self {
        Self::new(BTreeConfig::default())
    }

    /// Get configuration
    pub fn config(&self) -> &BTreeConfig {
        &self.config
    }

    /// Get statistics
    pub fn stats(&self) -> BTreeStats {
        recover_read_guard(&self.stats).clone()
    }

    /// Check if tree is empty
    pub fn is_empty(&self) -> bool {
        recover_read_guard(&self.root).is_none()
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        recover_read_guard(&self.stats).entries as usize
    }

    // =========================================
    // Transaction Management
    // =========================================

    /// Begin a new transaction
    pub fn begin_transaction(&self) -> TxnId {
        let mut next_id = recover_write_guard(&self.next_txn_id);
        let txn_id = *next_id;
        *next_id += 1;

        // Get list of active transactions
        let active_txns = recover_read_guard(&self.active_txns);
        let active_list: Vec<TxnId> = active_txns.keys().copied().collect();
        drop(active_txns);

        // Create new transaction
        let txn = ActiveTransaction::new(txn_id, active_list);

        recover_write_guard(&self.active_txns).insert(txn_id, txn);

        txn_id
    }

    /// Commit a transaction
    pub fn commit_transaction(&self, txn_id: TxnId) -> bool {
        let mut active = recover_write_guard(&self.active_txns);
        if let Some(mut txn) = active.remove(&txn_id) {
            txn.commit();
            true
        } else {
            false
        }
    }

    /// Abort a transaction
    pub fn abort_transaction(&self, txn_id: TxnId) -> bool {
        let mut active = recover_write_guard(&self.active_txns);
        if let Some(mut txn) = active.remove(&txn_id) {
            txn.abort();
            true
        } else {
            false
        }
    }

    /// Get snapshot for transaction
    pub fn get_snapshot(&self, txn_id: TxnId) -> Option<Snapshot> {
        let active = recover_read_guard(&self.active_txns);
        active.get(&txn_id).map(|txn| txn.snapshot.clone())
    }

    /// Create a read-only snapshot at current time
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(TxnId::ZERO, next_timestamp())
    }

    // =========================================
    // Node Management
    // =========================================

    /// Get node by ID
    pub(crate) fn get_node(&self, id: NodeId) -> Option<Arc<RwLock<Node<K, V>>>> {
        recover_read_guard(&self.nodes).get(&id).cloned()
    }

    /// Store node
    fn store_node(&self, node: Node<K, V>) -> NodeId {
        let id = node.id();
        let arc = Arc::new(RwLock::new(node));
        let node_type = recover_read_guard(&arc).node_type();
        recover_write_guard(&self.nodes).insert(id, Arc::clone(&arc));

        let mut stats = recover_write_guard(&self.stats);
        stats.nodes += 1;
        match node_type {
            NodeType::Internal => stats.internal_nodes += 1,
            NodeType::Leaf => stats.leaf_nodes += 1,
        }

        id
    }

    /// Remove node
    fn remove_node(&self, id: NodeId) {
        if let Some(node) = recover_write_guard(&self.nodes).remove(&id) {
            let mut stats = recover_write_guard(&self.stats);
            stats.nodes -= 1;
            match recover_read_guard(&node).node_type() {
                NodeType::Internal => stats.internal_nodes -= 1,
                NodeType::Leaf => stats.leaf_nodes -= 1,
            }
        }
    }

    // =========================================
    // Read Operations
    // =========================================

    /// Get value for key
    pub fn get(&self, key: &K, snapshot: &Snapshot) -> Option<V> {
        recover_write_guard(&self.stats).gets += 1;

        let root_id = *recover_read_guard(&self.root);
        let root_id = root_id?;

        self.get_from_node(root_id, key, snapshot)
    }

    /// Get value starting from node
    fn get_from_node(&self, node_id: NodeId, key: &K, snapshot: &Snapshot) -> Option<V> {
        let node = self.get_node(node_id)?;
        let node = recover_read_guard(&node);

        match &*node {
            Node::Internal(internal) => {
                let child_id = internal.get_child(key)?;
                drop(node);
                self.get_from_node(child_id, key, snapshot)
            }
            Node::Leaf(leaf) => leaf.get(key, snapshot).cloned(),
        }
    }

    /// Check if key exists
    pub fn contains(&self, key: &K, snapshot: &Snapshot) -> bool {
        self.get(key, snapshot).is_some()
    }

    /// Get range of values
    pub fn range(&self, start: Option<&K>, end: Option<&K>, snapshot: &Snapshot) -> Vec<(K, V)> {
        recover_write_guard(&self.stats).range_scans += 1;

        let mut results = Vec::new();

        // Find starting leaf
        let start_leaf_id = if let Some(start_key) = start {
            self.find_leaf(start_key)
        } else {
            *recover_read_guard(&self.first_leaf)
        };

        let Some(mut leaf_id) = start_leaf_id else {
            return results;
        };

        // Traverse leaves
        loop {
            let node = match self.get_node(leaf_id) {
                Some(n) => n,
                None => break,
            };

            let node = recover_read_guard(&node);
            let leaf = match &*node {
                Node::Leaf(l) => l,
                _ => break,
            };

            // Collect entries from this leaf
            for (key, value) in leaf.range(start, end, snapshot) {
                // Check end condition
                if let Some(end_key) = end {
                    if key >= end_key {
                        return results;
                    }
                }
                results.push((key.clone(), value.clone()));
            }

            // Move to next leaf
            leaf_id = match leaf.next {
                Some(id) => id,
                None => break,
            };
        }

        results
    }

    /// Find leaf node for key
    fn find_leaf(&self, key: &K) -> Option<NodeId> {
        let root_id = *recover_read_guard(&self.root);
        let root_id = root_id?;

        self.find_leaf_from_node(root_id, key)
    }

    /// Find leaf starting from node
    fn find_leaf_from_node(&self, node_id: NodeId, key: &K) -> Option<NodeId> {
        let node = self.get_node(node_id)?;
        let node = recover_read_guard(&node);

        match &*node {
            Node::Internal(internal) => {
                let child_id = internal.get_child(key)?;
                drop(node);
                self.find_leaf_from_node(child_id, key)
            }
            Node::Leaf(_) => Some(node_id),
        }
    }

    // =========================================
    // Write Operations
    // =========================================

    /// Insert key-value pair
    pub fn insert(&self, key: K, value: V, txn_id: TxnId) -> bool {
        let timestamp = next_timestamp();
        self.insert_with_timestamp(key, value, txn_id, timestamp)
    }

    /// Insert with explicit timestamp
    fn insert_with_timestamp(&self, key: K, value: V, txn_id: TxnId, timestamp: Timestamp) -> bool {
        let mut root_lock = recover_write_guard(&self.root);

        if root_lock.is_none() {
            // Create first leaf
            let mut leaf = LeafNode::new();
            leaf.insert(key, value, txn_id, timestamp);
            let leaf_id = self.store_node(Node::Leaf(leaf));
            *root_lock = Some(leaf_id);
            *recover_write_guard(&self.first_leaf) = Some(leaf_id);

            let mut stats = recover_write_guard(&self.stats);
            stats.entries += 1;
            stats.inserts += 1;
            stats.height = 1;

            return true;
        }

        let Some(root_id) = *root_lock else {
            return false;
        };
        drop(root_lock);

        // Insert into tree
        match self.insert_recursive(root_id, key.clone(), value, txn_id, timestamp) {
            InsertResult::Done(is_new) => {
                let mut stats = recover_write_guard(&self.stats);
                if is_new {
                    stats.entries += 1;
                    stats.inserts += 1;
                } else {
                    stats.updates += 1;
                }
                is_new
            }
            InsertResult::Split(median, right_id) => {
                // Root split - create new root
                let mut new_root = InternalNode::new();
                new_root.children.push(root_id);
                new_root.insert(median, right_id);

                let new_root_id = self.store_node(Node::Internal(new_root));
                *recover_write_guard(&self.root) = Some(new_root_id);

                let mut stats = recover_write_guard(&self.stats);
                stats.entries += 1;
                stats.inserts += 1;
                stats.height += 1;

                true
            }
        }
    }

    /// Recursive insert
    fn insert_recursive(
        &self,
        node_id: NodeId,
        key: K,
        value: V,
        txn_id: TxnId,
        timestamp: Timestamp,
    ) -> InsertResult<K> {
        let Some(node) = self.get_node(node_id) else {
            return InsertResult::Done(false);
        };
        let mut node = recover_write_guard(&node);

        match &mut *node {
            Node::Internal(internal) => {
                let child_idx = internal.find_child_index(&key);
                let child_id = internal.children[child_idx];
                drop(node);

                match self.insert_recursive(child_id, key, value, txn_id, timestamp) {
                    InsertResult::Done(is_new) => InsertResult::Done(is_new),
                    InsertResult::Split(median, right_child) => {
                        let Some(node) = self.get_node(node_id) else {
                            return InsertResult::Done(false);
                        };
                        let mut node = recover_write_guard(&node);
                        let internal = match &mut *node {
                            Node::Internal(internal) => internal,
                            Node::Leaf(_) => return InsertResult::Done(false),
                        };

                        internal.insert(median, right_child);

                        if internal.keys.len() >= self.config.order - 1 {
                            // Need to split
                            let (new_median, right) = internal.split();
                            let right_id = self.store_node(Node::Internal(right));
                            InsertResult::Split(new_median, right_id)
                        } else {
                            InsertResult::Done(true)
                        }
                    }
                }
            }
            Node::Leaf(leaf) => {
                let is_new = leaf.insert(key.clone(), value, txn_id, timestamp);

                if leaf.keys.len() >= self.config.order - 1 {
                    // Need to split
                    let (median, right) = leaf.split();
                    let right_id = self.store_node(Node::Leaf(right));
                    InsertResult::Split(median, right_id)
                } else {
                    InsertResult::Done(is_new)
                }
            }
        }
    }

    /// Delete key
    pub fn delete(&self, key: &K, txn_id: TxnId) -> bool {
        let timestamp = next_timestamp();
        self.delete_with_timestamp(key, txn_id, timestamp)
    }

    /// Delete with explicit timestamp
    fn delete_with_timestamp(&self, key: &K, txn_id: TxnId, timestamp: Timestamp) -> bool {
        let root_id = match *recover_read_guard(&self.root) {
            Some(id) => id,
            None => return false,
        };

        // Find leaf with key
        let leaf_id = match self.find_leaf(key) {
            Some(id) => id,
            None => return false,
        };

        // Mark as deleted (MVCC soft delete)
        let Some(node) = self.get_node(leaf_id) else {
            return false;
        };
        let mut node = recover_write_guard(&node);

        if let Node::Leaf(leaf) = &mut *node {
            if leaf.delete(key, txn_id, timestamp) {
                recover_write_guard(&self.stats).deletes += 1;
                return true;
            }
        }

        false
    }

    // =========================================
    // Utility Operations
    // =========================================

    pub(crate) fn compact_deleted_entries(&self, watermark: Timestamp) -> usize {
        let mut kept_entries: Vec<LeafEntry<K, V>> = Vec::new();
        let mut removed = 0usize;

        let mut leaf_id = *recover_read_guard(&self.first_leaf);
        while let Some(id) = leaf_id {
            let node = match self.get_node(id) {
                Some(node) => node,
                None => break,
            };
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                for entry in &leaf.entries {
                    if Self::entry_purgeable(entry, watermark) {
                        removed += 1;
                    } else {
                        kept_entries.push(entry.clone());
                    }
                }
                leaf_id = leaf.next;
            } else {
                break;
            }
        }

        if removed == 0 {
            return 0;
        }

        self.rebuild_from_entries(kept_entries);
        removed
    }

    fn entry_purgeable(entry: &LeafEntry<K, V>, watermark: Timestamp) -> bool {
        if !entry.versions.is_all_deleted() {
            return false;
        }

        entry
            .versions
            .head()
            .map(|version| version.created_at <= watermark)
            .unwrap_or(false)
    }

    fn rebuild_from_entries(&self, entries: Vec<LeafEntry<K, V>>) {
        let mut new_nodes: HashMap<NodeId, Arc<RwLock<Node<K, V>>>> = HashMap::new();
        let mut leaf_nodes = Vec::new();

        let max_leaf_keys = self.config.order.saturating_sub(1).max(1);

        for chunk in entries.chunks(max_leaf_keys) {
            let mut leaf = LeafNode::new();
            for entry in chunk {
                leaf.keys.push(entry.key.clone());
                leaf.entries.push(entry.clone());
            }
            leaf_nodes.push(leaf);
        }

        for i in 0..leaf_nodes.len() {
            if i > 0 {
                let prev_id = leaf_nodes[i - 1].id;
                leaf_nodes[i].prev = Some(prev_id);
            }
            if i + 1 < leaf_nodes.len() {
                let next_id = leaf_nodes[i + 1].id;
                leaf_nodes[i].next = Some(next_id);
            }
        }

        let mut current_level: Vec<NodeId> = Vec::new();
        for leaf in leaf_nodes {
            let id = leaf.id;
            new_nodes.insert(id, Arc::new(RwLock::new(Node::Leaf(leaf))));
            current_level.push(id);
        }

        let mut height = if current_level.is_empty() { 0 } else { 1 };
        let max_children = self.config.order.max(2);

        while current_level.len() > 1 {
            let mut next_level = Vec::new();

            for chunk in current_level.chunks(max_children) {
                let mut internal = InternalNode::new();

                for (idx, child_id) in chunk.iter().enumerate() {
                    internal.children.push(*child_id);
                    if idx > 0 {
                        let key = Self::node_min_key(&new_nodes, *child_id);
                        internal.keys.push(key);
                    }
                }

                let id = internal.id;
                new_nodes.insert(id, Arc::new(RwLock::new(Node::Internal(internal))));
                next_level.push(id);
            }

            current_level = next_level;
            height += 1;
        }

        let root_id = current_level.first().copied();

        *recover_write_guard(&self.root) = root_id;
        *recover_write_guard(&self.first_leaf) = root_id.and_then(|id| {
            let node = new_nodes.get(&id)?;
            let node = recover_read_guard(node);
            match &*node {
                Node::Leaf(_) => Some(id),
                Node::Internal(_) => {
                    let mut current = id;
                    loop {
                        let node = new_nodes.get(&current)?;
                        let node = recover_read_guard(node);
                        match &*node {
                            Node::Leaf(_) => return Some(current),
                            Node::Internal(internal) => {
                                if let Some(&child) = internal.children.first() {
                                    current = child;
                                } else {
                                    return None;
                                }
                            }
                        }
                    }
                }
            }
        });

        *recover_write_guard(&self.nodes) = new_nodes;

        let leaf_count = if let Some(first_leaf) = *recover_read_guard(&self.first_leaf) {
            let nodes = recover_read_guard(&self.nodes);
            let mut count = 0u64;
            let mut leaf_id = Some(first_leaf);
            while let Some(id) = leaf_id {
                let node = nodes.get(&id).cloned();
                if let Some(node) = node {
                    let node = recover_read_guard(&node);
                    if let Node::Leaf(leaf) = &*node {
                        count += 1;
                        leaf_id = leaf.next;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            count
        } else {
            0
        };

        let mut stats = recover_write_guard(&self.stats);
        stats.entries = entries.len() as u64;
        stats.nodes = recover_read_guard(&self.nodes).len() as u64;
        stats.leaf_nodes = leaf_count;
        stats.internal_nodes = stats.nodes.saturating_sub(leaf_count);
        stats.height = height as u32;
    }

    fn node_min_key(nodes: &HashMap<NodeId, Arc<RwLock<Node<K, V>>>>, node_id: NodeId) -> K {
        let node = nodes.get(&node_id).expect("node missing");
        let node = node.read().expect("node lock failed");
        match &*node {
            Node::Leaf(leaf) => leaf.keys.first().expect("leaf empty").clone(),
            Node::Internal(internal) => {
                let child = *internal.children.first().expect("internal empty");
                drop(node);
                Self::node_min_key(nodes, child)
            }
        }
    }

    /// Clear all entries
    pub fn clear(&self) {
        *recover_write_guard(&self.root) = None;
        *recover_write_guard(&self.first_leaf) = None;
        recover_write_guard(&self.nodes).clear();
        *recover_write_guard(&self.stats) = BTreeStats::default();
    }

    /// Get tree height
    pub fn height(&self) -> u32 {
        let root_id = match *recover_read_guard(&self.root) {
            Some(id) => id,
            None => return 0,
        };

        self.height_from_node(root_id)
    }

    /// Calculate height from node
    fn height_from_node(&self, node_id: NodeId) -> u32 {
        let node = match self.get_node(node_id) {
            Some(n) => n,
            None => return 0,
        };

        let node = recover_read_guard(&node);

        match &*node {
            Node::Leaf(_) => 1,
            Node::Internal(internal) => {
                if let Some(&first_child) = internal.children.first() {
                    drop(node);
                    1 + self.height_from_node(first_child)
                } else {
                    1
                }
            }
        }
    }

    /// Collect all keys (for debugging)
    pub fn all_keys(&self, snapshot: &Snapshot) -> Vec<K> {
        self.range(None, None, snapshot)
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }
}

impl<K, V> Default for BPlusTree<K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    fn default() -> Self {
        Self::with_default_config()
    }
}

/// Result of insert operation
enum InsertResult<K> {
    /// Insert completed
    Done(bool),
    /// Node split occurred (median key, right node ID)
    Split(K, NodeId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_tree() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert_eq!(tree.height(), 0);
    }

    #[test]
    fn test_insert_single() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        tree.insert(10, "ten".to_string(), TxnId(1));

        // Snapshot taken AFTER insert to see the data (MVCC semantics)
        let snapshot = tree.snapshot();
        assert!(!tree.is_empty());
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.get(&10, &snapshot), Some("ten".to_string()));
    }

    #[test]
    fn test_insert_multiple() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in 1..=10 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Snapshot taken AFTER inserts to see all data (MVCC semantics)
        let snapshot = tree.snapshot();
        assert_eq!(tree.len(), 10);

        for i in 1..=10 {
            assert_eq!(tree.get(&i, &snapshot), Some(format!("v{}", i)));
        }
    }

    #[test]
    fn test_insert_causes_split() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        // Insert enough to cause splits
        for i in 1..=20 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Snapshot taken AFTER inserts to see all data (MVCC semantics)
        let snapshot = tree.snapshot();
        assert_eq!(tree.len(), 20);
        assert!(tree.height() > 1);

        // All values still accessible
        for i in 1..=20 {
            assert_eq!(tree.get(&i, &snapshot), Some(format!("v{}", i)));
        }
    }

    #[test]
    fn test_update() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        tree.insert(10, "v1".to_string(), TxnId(1));
        // Snapshot AFTER first insert
        let snapshot1 = tree.snapshot();
        assert_eq!(tree.get(&10, &snapshot1), Some("v1".to_string()));

        tree.insert(10, "v2".to_string(), TxnId(2));
        // Snapshot AFTER update
        let snapshot2 = tree.snapshot();
        assert_eq!(tree.get(&10, &snapshot2), Some("v2".to_string()));

        // Still only one entry
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn test_delete() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        tree.insert(10, "ten".to_string(), TxnId(1));
        // Snapshot AFTER insert
        let snapshot1 = tree.snapshot();
        assert!(tree.contains(&10, &snapshot1));

        tree.delete(&10, TxnId(2));
        // Snapshot AFTER delete
        let snapshot2 = tree.snapshot();
        assert!(!tree.contains(&10, &snapshot2));
    }

    #[test]
    fn test_range() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in 1..=100 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Snapshot AFTER inserts (MVCC semantics)
        let snapshot = tree.snapshot();

        // Range 25..75
        let results = tree.range(Some(&25), Some(&75), &snapshot);
        assert_eq!(results.len(), 50); // 25..74 inclusive

        // First and last
        assert_eq!(results.first().unwrap().0, 25);
        assert_eq!(results.last().unwrap().0, 74);
    }

    #[test]
    fn test_range_full() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in 1..=10 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Snapshot AFTER inserts (MVCC semantics)
        let snapshot = tree.snapshot();
        let results = tree.range(None, None, &snapshot);
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn test_transactions() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        // Begin transaction
        let txn1 = tree.begin_transaction();
        assert!(tree.get_snapshot(txn1).is_some());

        // Insert in transaction
        tree.insert(10, "ten".to_string(), txn1);

        // Commit
        assert!(tree.commit_transaction(txn1));
        assert!(tree.get_snapshot(txn1).is_none());
    }

    #[test]
    fn test_abort_transaction() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        let txn1 = tree.begin_transaction();
        tree.insert(10, "ten".to_string(), txn1);

        // Abort
        assert!(tree.abort_transaction(txn1));
        assert!(tree.get_snapshot(txn1).is_none());
    }

    #[test]
    fn test_clear() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        for i in 1..=10 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        assert!(!tree.is_empty());

        tree.clear();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn test_all_keys() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in [5, 2, 8, 1, 9, 3, 7, 4, 6, 10] {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Snapshot AFTER inserts (MVCC semantics)
        let snapshot = tree.snapshot();
        let keys = tree.all_keys(&snapshot);
        assert_eq!(keys, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn test_stats() {
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));
        let snapshot = tree.snapshot();

        for i in 1..=10 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        tree.get(&5, &snapshot);
        tree.delete(&3, TxnId(2));

        let stats = tree.stats();
        assert_eq!(stats.inserts, 10);
        assert_eq!(stats.gets, 1);
        assert_eq!(stats.deletes, 1);
    }
}
