//! B+ Tree Node Structures
//!
//! Internal and leaf nodes for the B+ tree.

use super::version::{Snapshot, Timestamp, TxnId, VersionChain};
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// Node ID type
pub type NodeId = u64;

/// Global node ID counter
static NODE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Get next node ID
pub fn next_node_id() -> NodeId {
    NODE_ID_COUNTER.fetch_add(1, AtomicOrdering::SeqCst)
}

/// Node type discriminator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// Internal node (keys + child pointers)
    Internal,
    /// Leaf node (keys + values)
    Leaf,
}

/// Generic B+ tree node
#[derive(Debug)]
pub enum Node<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Internal node
    Internal(InternalNode<K, V>),
    /// Leaf node
    Leaf(LeafNode<K, V>),
}

impl<K, V> Node<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Get node ID
    pub fn id(&self) -> NodeId {
        match self {
            Node::Internal(n) => n.id,
            Node::Leaf(n) => n.id,
        }
    }

    /// Get node type
    pub fn node_type(&self) -> NodeType {
        match self {
            Node::Internal(_) => NodeType::Internal,
            Node::Leaf(_) => NodeType::Leaf,
        }
    }

    /// Check if node is leaf
    pub fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(_))
    }

    /// Get number of keys
    pub fn len(&self) -> usize {
        match self {
            Node::Internal(n) => n.keys.len(),
            Node::Leaf(n) => n.keys.len(),
        }
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if node is full
    pub fn is_full(&self, order: usize) -> bool {
        self.len() >= order - 1
    }

    /// Check if node has minimum keys
    pub fn has_min_keys(&self, order: usize) -> bool {
        let min = (order - 1) / 2;
        self.len() >= min
    }

    /// Get minimum key
    pub fn min_key(&self) -> Option<&K> {
        match self {
            Node::Internal(n) => n.keys.first(),
            Node::Leaf(n) => n.keys.first(),
        }
    }

    /// Get maximum key
    pub fn max_key(&self) -> Option<&K> {
        match self {
            Node::Internal(n) => n.keys.last(),
            Node::Leaf(n) => n.keys.last(),
        }
    }

    /// Get as internal node
    pub fn as_internal(&self) -> Option<&InternalNode<K, V>> {
        match self {
            Node::Internal(n) => Some(n),
            _ => None,
        }
    }

    /// Get as internal node mutable
    pub fn as_internal_mut(&mut self) -> Option<&mut InternalNode<K, V>> {
        match self {
            Node::Internal(n) => Some(n),
            _ => None,
        }
    }

    /// Get as leaf node
    pub fn as_leaf(&self) -> Option<&LeafNode<K, V>> {
        match self {
            Node::Leaf(n) => Some(n),
            _ => None,
        }
    }

    /// Get as leaf node mutable
    pub fn as_leaf_mut(&mut self) -> Option<&mut LeafNode<K, V>> {
        match self {
            Node::Leaf(n) => Some(n),
            _ => None,
        }
    }
}

/// Internal node (keys + child pointers)
#[derive(Debug)]
pub struct InternalNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Node ID
    pub id: NodeId,
    /// Keys (separator keys)
    pub keys: Vec<K>,
    /// Child node IDs (len = keys.len() + 1)
    pub children: Vec<NodeId>,
    /// Phantom data for V
    _phantom: std::marker::PhantomData<V>,
}

impl<K, V> InternalNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Create new internal node
    pub fn new() -> Self {
        Self {
            id: next_node_id(),
            keys: Vec::new(),
            children: Vec::new(),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create with initial child
    pub fn with_child(child_id: NodeId) -> Self {
        let mut node = Self::new();
        node.children.push(child_id);
        node
    }

    /// Find child index for key
    pub fn find_child_index(&self, key: &K) -> usize {
        match self.keys.binary_search(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    /// Get child for key
    pub fn get_child(&self, key: &K) -> Option<NodeId> {
        let idx = self.find_child_index(key);
        self.children.get(idx).copied()
    }

    /// Insert key and child at position
    pub fn insert_at(&mut self, idx: usize, key: K, right_child: NodeId) {
        self.keys.insert(idx, key);
        self.children.insert(idx + 1, right_child);
    }

    /// Insert key and child in sorted order
    pub fn insert(&mut self, key: K, right_child: NodeId) {
        let idx = match self.keys.binary_search(&key) {
            Ok(i) | Err(i) => i,
        };
        self.insert_at(idx, key, right_child);
    }

    /// Remove key at index
    pub fn remove_at(&mut self, idx: usize) -> (K, NodeId) {
        let key = self.keys.remove(idx);
        let child = self.children.remove(idx + 1);
        (key, child)
    }

    /// Split node at middle
    pub fn split(&mut self) -> (K, Self) {
        let mid = self.keys.len() / 2;

        // Middle key goes up to parent
        let median_key = self.keys.remove(mid);

        // Right half becomes new node
        let right_keys: Vec<K> = self.keys.drain(mid..).collect();
        let right_children: Vec<NodeId> = self.children.drain(mid + 1..).collect();

        let mut right = Self::new();
        right.keys = right_keys;
        right.children = right_children;

        (median_key, right)
    }

    /// Merge with sibling
    pub fn merge(&mut self, separator: K, right: &mut Self) {
        self.keys.push(separator);
        self.keys.append(&mut right.keys);
        self.children.append(&mut right.children);
    }

    /// Borrow from left sibling
    ///
    /// # Invariant
    ///
    /// Caller must have verified that `left.keys.len() > MIN_KEYS` before
    /// invoking this function (see rebalance logic in `tree.rs`). Otherwise
    /// borrowing would violate the B-tree invariant on the sibling.
    pub fn borrow_from_left(&mut self, left: &mut Self, parent_key: &K) -> K {
        // Get rightmost key from left sibling
        let borrowed_key = left
            .keys
            .pop()
            .expect("invariant: borrow_from_left requires left.keys non-empty");
        let borrowed_child = left
            .children
            .pop()
            .expect("invariant: internal node has children.len() == keys.len() + 1");

        // Parent key comes down
        self.keys.insert(0, parent_key.clone());
        self.children.insert(0, borrowed_child);

        // Borrowed key goes up to parent
        borrowed_key
    }

    /// Borrow from right sibling
    pub fn borrow_from_right(&mut self, right: &mut Self, parent_key: &K) -> K {
        // Get leftmost key from right sibling
        let borrowed_key = right.keys.remove(0);
        let borrowed_child = right.children.remove(0);

        // Parent key comes down
        self.keys.push(parent_key.clone());
        self.children.push(borrowed_child);

        // Borrowed key goes up to parent
        borrowed_key
    }
}

impl<K, V> Default for InternalNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Leaf node entry with MVCC versions
#[derive(Debug, Clone)]
pub struct LeafEntry<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Key
    pub key: K,
    /// Version chain for the value
    pub versions: VersionChain<V>,
}

impl<K, V> LeafEntry<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Create new entry
    pub fn new(key: K, value: V, txn_id: TxnId, timestamp: Timestamp) -> Self {
        Self {
            key,
            versions: VersionChain::with_value(value, txn_id, timestamp),
        }
    }

    /// Get value for snapshot
    pub fn get(&self, snapshot: &Snapshot) -> Option<&V> {
        self.versions.get(snapshot)
    }

    /// Update value
    pub fn update(&mut self, value: V, txn_id: TxnId, timestamp: Timestamp) {
        self.versions.update(value, txn_id, timestamp);
    }

    /// Delete entry
    pub fn delete(&mut self, txn_id: TxnId, timestamp: Timestamp) {
        self.versions.delete(txn_id, timestamp);
    }
}

/// Leaf node (keys + versioned values)
#[derive(Debug)]
pub struct LeafNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Node ID
    pub id: NodeId,
    /// Keys
    pub keys: Vec<K>,
    /// Versioned values (parallel to keys)
    pub entries: Vec<LeafEntry<K, V>>,
    /// Next leaf (for range scans)
    pub next: Option<NodeId>,
    /// Previous leaf (for reverse scans)
    pub prev: Option<NodeId>,
}

impl<K, V> LeafNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    /// Create new leaf node
    pub fn new() -> Self {
        Self {
            id: next_node_id(),
            keys: Vec::new(),
            entries: Vec::new(),
            next: None,
            prev: None,
        }
    }

    /// Find index for key
    pub fn find_index(&self, key: &K) -> Result<usize, usize> {
        self.keys.binary_search(key)
    }

    /// Get value for key
    pub fn get(&self, key: &K, snapshot: &Snapshot) -> Option<&V> {
        match self.find_index(key) {
            Ok(idx) => self.entries[idx].get(snapshot),
            Err(_) => None,
        }
    }

    /// Check if key exists
    pub fn contains(&self, key: &K, snapshot: &Snapshot) -> bool {
        self.get(key, snapshot).is_some()
    }

    /// Insert key-value pair
    pub fn insert(&mut self, key: K, value: V, txn_id: TxnId, timestamp: Timestamp) -> bool {
        match self.find_index(&key) {
            Ok(idx) => {
                // Key exists, update
                self.entries[idx].update(value, txn_id, timestamp);
                false // No new key added
            }
            Err(idx) => {
                // New key
                let entry = LeafEntry::new(key.clone(), value, txn_id, timestamp);
                self.keys.insert(idx, key);
                self.entries.insert(idx, entry);
                true // New key added
            }
        }
    }

    /// Delete key
    pub fn delete(&mut self, key: &K, txn_id: TxnId, timestamp: Timestamp) -> bool {
        match self.find_index(key) {
            Ok(idx) => {
                self.entries[idx].delete(txn_id, timestamp);
                true
            }
            Err(_) => false,
        }
    }

    /// Split node at middle
    pub fn split(&mut self) -> (K, Self) {
        let mid = self.keys.len() / 2;

        // Right half becomes new node
        let right_keys: Vec<K> = self.keys.drain(mid..).collect();
        let right_entries: Vec<LeafEntry<K, V>> = self.entries.drain(mid..).collect();

        let mut right = Self::new();
        right.keys = right_keys.clone();
        right.entries = right_entries;

        // First key of right node is the separator
        let separator = right.keys[0].clone();

        // Link siblings
        right.next = self.next;
        right.prev = Some(self.id);
        self.next = Some(right.id);

        (separator, right)
    }

    /// Merge with right sibling
    pub fn merge(&mut self, right: &mut Self) {
        self.keys.append(&mut right.keys);
        self.entries.append(&mut right.entries);
        self.next = right.next;
    }

    /// Borrow from left sibling
    ///
    /// # Invariant
    ///
    /// Caller must have verified that `left.keys.len() > MIN_KEYS` before
    /// invoking this function. In leaf nodes `keys.len() == entries.len()`
    /// always, so one invariant check covers both pops.
    pub fn borrow_from_left(&mut self, left: &mut Self) -> K {
        let borrowed_key = left
            .keys
            .pop()
            .expect("invariant: borrow_from_left requires left.keys non-empty");
        let borrowed_entry = left
            .entries
            .pop()
            .expect("invariant: leaf node has keys.len() == entries.len()");

        self.keys.insert(0, borrowed_key.clone());
        self.entries.insert(0, borrowed_entry);

        self.keys[0].clone()
    }

    /// Borrow from right sibling
    pub fn borrow_from_right(&mut self, right: &mut Self) -> K {
        let borrowed_key = right.keys.remove(0);
        let borrowed_entry = right.entries.remove(0);

        self.keys.push(borrowed_key);
        self.entries.push(borrowed_entry);

        right.keys[0].clone()
    }

    /// Get all keys in range
    pub fn range<'a>(
        &'a self,
        start: Option<&K>,
        end: Option<&K>,
        snapshot: &'a Snapshot,
    ) -> impl Iterator<Item = (&'a K, &'a V)> {
        let start_idx = match start {
            Some(k) => match self.find_index(k) {
                Ok(i) => i,
                Err(i) => i,
            },
            None => 0,
        };

        let end_idx = match end {
            Some(k) => match self.find_index(k) {
                Ok(i) => i + 1,
                Err(i) => i,
            },
            None => self.keys.len(),
        };

        self.keys[start_idx..end_idx]
            .iter()
            .zip(self.entries[start_idx..end_idx].iter())
            .filter_map(move |(key, entry)| entry.get(snapshot).map(|v| (key, v)))
    }

    /// Garbage collect old versions
    pub fn gc(&mut self, watermark: Timestamp) -> usize {
        let mut removed = 0;
        for entry in &mut self.entries {
            removed += entry.versions.gc(watermark);
        }
        removed
    }
}

impl<K, V> Default for LeafNode<K, V>
where
    K: Clone + Ord + Debug,
    V: Clone + Debug,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_internal_node_basic() {
        let mut node: InternalNode<i32, String> = InternalNode::new();
        assert!(node.keys.is_empty());
        assert!(node.children.is_empty());

        // Add a child first
        node.children.push(100);

        // Insert separator keys with children
        node.insert(5, 101);
        node.insert(10, 102);
        node.insert(15, 103);

        assert_eq!(node.keys, vec![5, 10, 15]);
        assert_eq!(node.children, vec![100, 101, 102, 103]);
    }

    #[test]
    fn test_internal_node_find_child() {
        let mut node: InternalNode<i32, String> = InternalNode::new();
        node.children.push(100);
        node.insert(10, 101);
        node.insert(20, 102);
        node.insert(30, 103);

        // Key < 10 goes to child 0
        assert_eq!(node.find_child_index(&5), 0);
        // Key = 10 goes to child 1
        assert_eq!(node.find_child_index(&10), 1);
        // Key 15 (10 < x < 20) goes to child 1
        assert_eq!(node.find_child_index(&15), 1);
        // Key >= 30 goes to child 3
        assert_eq!(node.find_child_index(&35), 3);
    }

    #[test]
    fn test_internal_node_split() {
        let mut node: InternalNode<i32, String> = InternalNode::new();
        node.children.push(100);
        for i in 1..=6 {
            node.insert(i * 10, 100 + i as u64);
        }

        let (median, right) = node.split();

        // Median goes up
        assert!(node.keys.len() < 6);
        assert!(!right.keys.is_empty());
    }

    #[test]
    fn test_leaf_node_basic() {
        let mut node: LeafNode<i32, String> = LeafNode::new();
        let snapshot = Snapshot::new(TxnId::ZERO, Timestamp(100));

        assert!(node.keys.is_empty());

        // Insert values
        node.insert(10, "ten".to_string(), TxnId(1), Timestamp(1));
        node.insert(20, "twenty".to_string(), TxnId(1), Timestamp(2));
        node.insert(5, "five".to_string(), TxnId(1), Timestamp(3));

        assert_eq!(node.keys, vec![5, 10, 20]);
        assert_eq!(node.get(&10, &snapshot), Some(&"ten".to_string()));
        assert_eq!(node.get(&15, &snapshot), None);
    }

    #[test]
    fn test_leaf_node_update() {
        let mut node: LeafNode<i32, String> = LeafNode::new();
        let snapshot = Snapshot::new(TxnId::ZERO, Timestamp(100));

        node.insert(10, "v1".to_string(), TxnId(1), Timestamp(1));
        assert_eq!(node.get(&10, &snapshot), Some(&"v1".to_string()));

        // Update same key
        node.insert(10, "v2".to_string(), TxnId(2), Timestamp(2));
        assert_eq!(node.get(&10, &snapshot), Some(&"v2".to_string()));

        // Only one key
        assert_eq!(node.keys.len(), 1);
    }

    #[test]
    fn test_leaf_node_delete() {
        let mut node: LeafNode<i32, String> = LeafNode::new();
        let snapshot = Snapshot::new(TxnId::ZERO, Timestamp(100));

        node.insert(10, "ten".to_string(), TxnId(1), Timestamp(1));
        assert!(node.contains(&10, &snapshot));

        node.delete(&10, TxnId(2), Timestamp(2));
        assert!(!node.contains(&10, &snapshot));
    }

    #[test]
    fn test_leaf_node_split() {
        let mut node: LeafNode<i32, String> = LeafNode::new();

        for i in 1..=6 {
            node.insert(i * 10, format!("v{}", i), TxnId(1), Timestamp(i as u64));
        }

        let (separator, right) = node.split();

        // Keys are split
        assert!(node.keys.len() < 6);
        assert!(!right.keys.is_empty());

        // Separator is first key of right
        assert_eq!(separator, right.keys[0]);

        // Siblings are linked
        assert_eq!(node.next, Some(right.id));
        assert_eq!(right.prev, Some(node.id));
    }

    #[test]
    fn test_leaf_node_range() {
        let mut node: LeafNode<i32, String> = LeafNode::new();
        let snapshot = Snapshot::new(TxnId::ZERO, Timestamp(100));

        for i in 1..=10 {
            node.insert(i * 10, format!("v{}", i), TxnId(1), Timestamp(i as u64));
        }

        // Range 25..75
        let results: Vec<_> = node.range(Some(&25), Some(&75), &snapshot).collect();
        assert_eq!(results.len(), 5); // 30, 40, 50, 60, 70
    }

    #[test]
    fn test_node_enum() {
        let leaf: Node<i32, String> = Node::Leaf(LeafNode::new());
        assert!(leaf.is_leaf());
        assert!(leaf.as_leaf().is_some());
        assert!(leaf.as_internal().is_none());

        let internal: Node<i32, String> = Node::Internal(InternalNode::new());
        assert!(!internal.is_leaf());
        assert!(internal.as_internal().is_some());
        assert!(internal.as_leaf().is_none());
    }
}
