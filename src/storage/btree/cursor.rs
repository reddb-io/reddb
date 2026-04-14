//! B+ Tree Cursor
//!
//! Iterator for traversing B+ tree entries.

use super::node::{Node, NodeId};
use super::tree::BPlusTree;
use super::version::Snapshot;
use std::fmt::Debug;
use std::sync::{RwLock, RwLockReadGuard};

fn recover_read_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Cursor direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirection {
    /// Forward (ascending keys)
    Forward,
    /// Backward (descending keys)
    Backward,
}

/// Cursor state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorState {
    /// Before first entry
    BeforeFirst,
    /// At valid entry
    Valid,
    /// After last entry
    AfterLast,
    /// Invalid/closed
    Invalid,
}

/// Cursor for iterating over B+ tree entries
pub struct Cursor<'a, K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    /// Reference to tree
    tree: &'a BPlusTree<K, V>,
    /// Snapshot for consistent reads
    snapshot: Snapshot,
    /// Current leaf node ID
    current_leaf: Option<NodeId>,
    /// Current index in leaf
    current_index: usize,
    /// Direction
    direction: CursorDirection,
    /// State
    state: CursorState,
    /// Cached current key-value
    current_entry: Option<(K, V)>,
    /// Pre-fetched visible entries from the current leaf.
    ///
    /// Populated once when the cursor lands on a new leaf (via `fill_leaf_cache`).
    /// Subsequent `next()` calls drain this vec without re-acquiring `nodes.read()`.
    /// One lock acquisition per leaf instead of one per visible entry.
    leaf_cache: Vec<(K, V)>,
    /// Next position to consume from `leaf_cache`.
    leaf_cache_idx: usize,
}

impl<'a, K, V> Cursor<'a, K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    /// Create new forward cursor at beginning
    pub fn new(tree: &'a BPlusTree<K, V>, snapshot: Snapshot) -> Self {
        Self {
            tree,
            snapshot,
            current_leaf: None,
            current_index: 0,
            direction: CursorDirection::Forward,
            state: CursorState::BeforeFirst,
            current_entry: None,
            leaf_cache: Vec::new(),
            leaf_cache_idx: 0,
        }
    }

    /// Create cursor at specific key
    pub fn at_key(tree: &'a BPlusTree<K, V>, snapshot: Snapshot, key: &K) -> Self {
        let mut cursor = Self::new(tree, snapshot);
        cursor.seek(key);
        cursor
    }

    /// Create reverse cursor
    pub fn reverse(tree: &'a BPlusTree<K, V>, snapshot: Snapshot) -> Self {
        Self {
            tree,
            snapshot,
            current_leaf: None,
            current_index: 0,
            direction: CursorDirection::Backward,
            state: CursorState::AfterLast,
            current_entry: None,
            leaf_cache: Vec::new(),
            leaf_cache_idx: 0,
        }
    }

    /// Get current state
    pub fn state(&self) -> CursorState {
        self.state
    }

    /// Check if at valid entry
    pub fn is_valid(&self) -> bool {
        self.state == CursorState::Valid
    }

    /// Get current key
    pub fn key(&self) -> Option<&K> {
        self.current_entry.as_ref().map(|(k, _)| k)
    }

    /// Get current value
    pub fn value(&self) -> Option<&V> {
        self.current_entry.as_ref().map(|(_, v)| v)
    }

    /// Get current key-value pair
    pub fn entry(&self) -> Option<(&K, &V)> {
        self.current_entry.as_ref().map(|(k, v)| (k, v))
    }

    /// Move to first entry
    pub fn first(&mut self) -> bool {
        // Find first leaf
        let first_leaf = match *recover_read_guard(&self.tree.first_leaf) {
            Some(id) => id,
            None => {
                self.state = CursorState::AfterLast;
                return false;
            }
        };

        self.current_leaf = Some(first_leaf);
        self.current_index = 0;
        self.direction = CursorDirection::Forward;
        // Reset cache so fill_leaf_cache() is called for the new leaf
        self.leaf_cache.clear();
        self.leaf_cache_idx = 0;

        self.load_current()
    }

    /// Move to last entry
    pub fn last(&mut self) -> bool {
        // Find last leaf by traversing from first
        let mut leaf_id = match *recover_read_guard(&self.tree.first_leaf) {
            Some(id) => id,
            None => {
                self.state = CursorState::BeforeFirst;
                return false;
            }
        };

        // Walk to last leaf
        while let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                match leaf.next {
                    Some(next_id) => {
                        leaf_id = next_id;
                    }
                    None => break,
                }
            } else {
                break;
            }
        }

        self.current_leaf = Some(leaf_id);

        // Set to last entry in leaf
        if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                self.current_index = leaf.keys.len().saturating_sub(1);
            }
        }

        self.direction = CursorDirection::Backward;
        // Reset cache — last() positions at a specific index; subsequent prev()
        // calls use load_current_at() which bypasses the forward cache.
        self.leaf_cache.clear();
        self.leaf_cache_idx = 0;
        self.load_current_at(self.current_index)
    }

    /// Seek to key (or first key >= key)
    pub fn seek(&mut self, key: &K) -> bool {
        // Find leaf containing key
        let leaf_id = match self.find_leaf(key) {
            Some(id) => id,
            None => {
                self.state = CursorState::AfterLast;
                return false;
            }
        };

        self.current_leaf = Some(leaf_id);

        // Find index in leaf
        if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                match leaf.keys.binary_search(key) {
                    Ok(i) => self.current_index = i,
                    Err(i) => self.current_index = i,
                }
            }
        }

        // Reset cache — fill_leaf_cache respects current_index for seek offset
        self.leaf_cache.clear();
        self.leaf_cache_idx = 0;

        self.load_current()
    }

    /// Move to next entry
    pub fn next(&mut self) -> bool {
        match self.state {
            CursorState::BeforeFirst => self.first(),
            CursorState::AfterLast | CursorState::Invalid => false,
            CursorState::Valid => {
                // Fast path: consume next entry from the pre-fetched leaf cache
                self.leaf_cache_idx += 1;
                if self.leaf_cache_idx < self.leaf_cache.len() {
                    let entry = self.leaf_cache[self.leaf_cache_idx].clone();
                    self.current_entry = Some(entry);
                    return true;
                }
                // Cache exhausted — move to next leaf
                self.move_to_next_leaf()
            }
        }
    }

    /// Move to previous entry
    pub fn prev(&mut self) -> bool {
        match self.state {
            CursorState::AfterLast => self.last(),
            CursorState::BeforeFirst | CursorState::Invalid => false,
            CursorState::Valid => {
                if self.current_index == 0 {
                    // Move to previous leaf
                    self.move_to_prev_leaf()
                } else {
                    self.current_index -= 1;
                    // Backward traversal bypasses the forward cache — load directly
                    // from the node at current_index.
                    self.leaf_cache.clear();
                    self.leaf_cache_idx = 0;
                    self.load_current_at(self.current_index)
                }
            }
        }
    }

    /// Load a single visible entry at `index` in `current_leaf`, without using
    /// or populating `leaf_cache`.  Used by backward traversal where the cache
    /// (filled in forward order from seek/first position) doesn't cover earlier
    /// positions.
    fn load_current_at(&mut self, index: usize) -> bool {
        let leaf_id = match self.current_leaf {
            Some(id) => id,
            None => {
                self.state = CursorState::Invalid;
                self.current_entry = None;
                return false;
            }
        };

        if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                if index < leaf.keys.len() {
                    if let Some(value) = leaf.entries[index].get(&self.snapshot) {
                        self.current_entry = Some((leaf.keys[index].clone(), value.clone()));
                        self.state = CursorState::Valid;
                        return true;
                    }
                }
            }
        }

        self.state = CursorState::BeforeFirst;
        self.current_entry = None;
        false
    }

    /// Find leaf for key
    fn find_leaf(&self, key: &K) -> Option<NodeId> {
        let root_id = (*recover_read_guard(&self.tree.root))?;
        self.find_leaf_from(root_id, key)
    }

    fn find_leaf_from(&self, node_id: NodeId, key: &K) -> Option<NodeId> {
        let node = self.tree.get_node(node_id)?;
        let node = recover_read_guard(&node);

        match &*node {
            Node::Internal(internal) => {
                let child_id = internal.get_child(key)?;
                drop(node);
                self.find_leaf_from(child_id, key)
            }
            Node::Leaf(_) => Some(node_id),
        }
    }

    /// Check if current index is within bounds
    fn check_bounds(&self) -> bool {
        if let Some(leaf_id) = self.current_leaf {
            if let Some(node) = self.tree.get_node(leaf_id) {
                let node = recover_read_guard(&node);
                if let Node::Leaf(leaf) = &*node {
                    return self.current_index < leaf.keys.len();
                }
            }
        }
        false
    }

    /// Populate `leaf_cache` with all visible entries from `current_leaf`.
    ///
    /// Acquires `nodes.read()` exactly once for the entire leaf, then releases
    /// the lock. Subsequent `next()` calls drain the cache without re-locking.
    /// Returns true if at least one visible entry was found.
    fn fill_leaf_cache(&mut self) -> bool {
        self.leaf_cache.clear();
        self.leaf_cache_idx = 0;

        let leaf_id = match self.current_leaf {
            Some(id) => id,
            None => return false,
        };

        if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                // Skip entries before current_index (used by seek())
                for i in self.current_index..leaf.keys.len() {
                    if let Some(value) = leaf.entries[i].get(&self.snapshot) {
                        self.leaf_cache.push((leaf.keys[i].clone(), value.clone()));
                    }
                }
            }
        }

        !self.leaf_cache.is_empty()
    }

    /// Load current entry from leaf — uses `leaf_cache` when available,
    /// falling back to `fill_leaf_cache()` on first entry of a new leaf.
    fn load_current(&mut self) -> bool {
        // If cache is warm and has entries, serve directly (no lock needed)
        if self.leaf_cache_idx < self.leaf_cache.len() {
            let entry = self.leaf_cache[self.leaf_cache_idx].clone();
            self.current_entry = Some(entry);
            self.state = CursorState::Valid;
            return true;
        }

        // Cache empty — fill from current leaf (one lock acquisition)
        if self.fill_leaf_cache() {
            let entry = self.leaf_cache[0].clone();
            self.current_entry = Some(entry);
            self.state = CursorState::Valid;
            true
        } else {
            // No visible entries in this leaf — move to next
            self.move_to_next_leaf()
        }
    }

    /// Move to next leaf
    fn move_to_next_leaf(&mut self) -> bool {
        let leaf_id = match self.current_leaf {
            Some(id) => id,
            None => {
                self.state = CursorState::AfterLast;
                return false;
            }
        };

        // Read next sibling pointer — single lock acquisition, also resets cache
        let next_leaf = if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                leaf.next
            } else {
                None
            }
        } else {
            None
        };

        match next_leaf {
            Some(next_id) => {
                self.current_leaf = Some(next_id);
                self.current_index = 0;
                // Clear cache — fill_leaf_cache() will populate it from the new leaf
                self.leaf_cache.clear();
                self.leaf_cache_idx = 0;
                self.load_current()
            }
            None => {
                self.state = CursorState::AfterLast;
                self.current_entry = None;
                false
            }
        }
    }

    /// Move to previous leaf
    fn move_to_prev_leaf(&mut self) -> bool {
        let leaf_id = match self.current_leaf {
            Some(id) => id,
            None => {
                self.state = CursorState::BeforeFirst;
                return false;
            }
        };

        let prev_leaf = if let Some(node) = self.tree.get_node(leaf_id) {
            let node = recover_read_guard(&node);
            if let Node::Leaf(leaf) = &*node {
                leaf.prev
            } else {
                None
            }
        } else {
            None
        };

        match prev_leaf {
            Some(prev_id) => {
                self.current_leaf = Some(prev_id);
                // Set to last entry in previous leaf
                if let Some(node) = self.tree.get_node(prev_id) {
                    let node = recover_read_guard(&node);
                    if let Node::Leaf(leaf) = &*node {
                        self.current_index = leaf.keys.len().saturating_sub(1);
                    }
                }
                self.leaf_cache.clear();
                self.leaf_cache_idx = 0;
                self.load_current_at(self.current_index)
            }
            None => {
                self.state = CursorState::BeforeFirst;
                self.current_entry = None;
                false
            }
        }
    }
}

impl<'a, K, V> Iterator for Cursor<'a, K, V>
where
    K: Clone + Ord + Debug + Send + Sync,
    V: Clone + Debug + Send + Sync,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.state == CursorState::BeforeFirst {
            if !self.first() {
                return None;
            }
            self.current_entry.clone()
        } else if self.next() {
            self.current_entry.clone()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::btree::{BPlusTree, BTreeConfig};

    fn create_test_tree() -> BPlusTree<i32, String> {
        use crate::storage::primitives::ids::TxnId;
        let tree = BPlusTree::new(BTreeConfig::new().with_order(4));
        for i in 1..=10 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }
        tree
    }

    #[test]
    fn test_cursor_forward() {
        let tree = create_test_tree();
        let snapshot = tree.snapshot();
        let cursor = Cursor::new(&tree, snapshot);

        let results: Vec<_> = cursor.collect();
        assert_eq!(results.len(), 10);
        assert_eq!(results[0], (1, "v1".to_string()));
        assert_eq!(results[9], (10, "v10".to_string()));
    }

    #[test]
    fn test_cursor_first_last() {
        let tree = create_test_tree();
        let snapshot = tree.snapshot();
        let mut cursor = Cursor::new(&tree, snapshot);

        // First - use UFCS to avoid Iterator::first() conflict
        assert!(Cursor::first(&mut cursor));
        assert_eq!(cursor.key(), Some(&1));

        // Last - use UFCS to avoid Iterator::last() conflict
        let mut cursor2 = Cursor::new(&tree, tree.snapshot());
        assert!(Cursor::last(&mut cursor2));
        assert_eq!(cursor2.key(), Some(&10));
    }

    #[test]
    fn test_cursor_seek() {
        let tree = create_test_tree();
        let snapshot = tree.snapshot();
        let mut cursor = Cursor::new(&tree, snapshot);

        // Seek to existing key
        assert!(cursor.seek(&5));
        assert_eq!(cursor.key(), Some(&5));

        // Seek to non-existing key (should find next)
        assert!(cursor.seek(&7));
        assert_eq!(cursor.key(), Some(&7));
    }

    #[test]
    fn test_cursor_prev() {
        let tree = create_test_tree();
        let snapshot = tree.snapshot();
        let mut cursor = Cursor::new(&tree, snapshot);

        // Start at end - use UFCS to avoid Iterator::last() conflict
        Cursor::last(&mut cursor);
        assert_eq!(cursor.key(), Some(&10));

        // Move backwards
        cursor.prev();
        assert_eq!(cursor.key(), Some(&9));

        cursor.prev();
        assert_eq!(cursor.key(), Some(&8));
    }

    #[test]
    fn test_cursor_empty_tree() {
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();
        let snapshot = tree.snapshot();
        let mut cursor = Cursor::new(&tree, snapshot);

        assert!(!cursor.first());
        assert!(!cursor.is_valid());
    }
}
