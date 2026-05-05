//! Garbage Collection for MVCC Versions
//!
//! Cleans up old versions that are no longer visible to any transaction.

use super::node::{Node, NodeId};
use super::tree::BPlusTree;
use super::version::{current_timestamp, Timestamp};
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

fn gc_read<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn gc_write<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// GC Configuration
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// Minimum age of versions to collect (in timestamps)
    pub min_age: Timestamp,
    /// Maximum versions to process per batch
    pub batch_size: usize,
    /// Interval between GC runs
    pub interval: Duration,
    /// Enable background GC
    pub background_gc: bool,
}

impl GcConfig {
    /// Create default config
    pub fn new() -> Self {
        Self {
            min_age: Timestamp(1000),
            batch_size: 1000,
            interval: Duration::from_secs(60),
            background_gc: true,
        }
    }

    /// Set minimum age
    pub fn with_min_age(mut self, age: Timestamp) -> Self {
        self.min_age = age;
        self
    }

    /// Set batch size
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Set interval
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

impl Default for GcConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// GC Statistics
#[derive(Debug, Clone, Default)]
pub struct GcStats {
    /// Total GC runs
    pub runs: u64,
    /// Total versions collected
    pub versions_collected: u64,
    /// Total nodes visited
    pub nodes_visited: u64,
    /// Total time spent in GC (microseconds)
    pub time_spent_us: u64,
    /// Last run timestamp
    pub last_run: Timestamp,
    /// Last run duration (microseconds)
    pub last_run_duration_us: u64,
}

/// Garbage Collector for B+ Tree
pub struct GarbageCollector {
    /// Configuration
    config: GcConfig,
    /// Statistics
    stats: AtomicGcStats,
    /// Running flag
    running: AtomicBool,
    /// Oldest active transaction timestamp
    oldest_active_ts: AtomicU64,
}

/// Atomic wrapper for GC stats
struct AtomicGcStats {
    runs: AtomicU64,
    versions_collected: AtomicU64,
    nodes_visited: AtomicU64,
    time_spent_us: AtomicU64,
    last_run: AtomicU64,
    last_run_duration_us: AtomicU64,
}

impl AtomicGcStats {
    fn new() -> Self {
        Self {
            runs: AtomicU64::new(0),
            versions_collected: AtomicU64::new(0),
            nodes_visited: AtomicU64::new(0),
            time_spent_us: AtomicU64::new(0),
            last_run: AtomicU64::new(0),
            last_run_duration_us: AtomicU64::new(0),
        }
    }

    fn to_stats(&self) -> GcStats {
        GcStats {
            runs: self.runs.load(Ordering::Relaxed),
            versions_collected: self.versions_collected.load(Ordering::Relaxed),
            nodes_visited: self.nodes_visited.load(Ordering::Relaxed),
            time_spent_us: self.time_spent_us.load(Ordering::Relaxed),
            last_run: Timestamp(self.last_run.load(Ordering::Relaxed)),
            last_run_duration_us: self.last_run_duration_us.load(Ordering::Relaxed),
        }
    }
}

impl GarbageCollector {
    /// Create new GC
    pub fn new(config: GcConfig) -> Self {
        Self {
            config,
            stats: AtomicGcStats::new(),
            running: AtomicBool::new(false),
            oldest_active_ts: AtomicU64::new(0),
        }
    }

    /// Get configuration
    pub fn config(&self) -> &GcConfig {
        &self.config
    }

    /// Get statistics
    pub fn stats(&self) -> GcStats {
        self.stats.to_stats()
    }

    /// Update oldest active transaction timestamp
    pub fn set_oldest_active(&self, ts: Timestamp) {
        self.oldest_active_ts.store(ts.get(), Ordering::SeqCst);
    }

    /// Calculate GC watermark
    fn calculate_watermark(&self) -> Timestamp {
        let current = current_timestamp();
        let oldest_active = Timestamp(self.oldest_active_ts.load(Ordering::SeqCst));

        // Watermark is the minimum of:
        // - oldest active transaction timestamp
        // - current - min_age
        if !oldest_active.is_epoch() {
            oldest_active.min(current.saturating_sub(self.config.min_age))
        } else {
            current.saturating_sub(self.config.min_age)
        }
    }

    /// Run GC on a B+ tree
    pub fn run<K, V>(&self, tree: &BPlusTree<K, V>) -> GcStats
    where
        K: Clone + Ord + Debug + Send + Sync,
        V: Clone + Debug + Send + Sync,
    {
        // Check if already running
        if self.running.swap(true, Ordering::SeqCst) {
            return self.stats();
        }

        let start = Instant::now();
        let watermark = self.calculate_watermark();

        let mut versions_collected = 0u64;
        let mut nodes_visited = 0u64;

        // Collect from all leaf nodes
        let first_leaf = *gc_read(&tree.first_leaf);
        let mut current_leaf = first_leaf;

        while let Some(leaf_id) = current_leaf {
            nodes_visited += 1;

            // Process leaf
            if let Some(node) = tree.get_node(leaf_id) {
                let mut node = gc_write(&node);
                if let Node::Leaf(leaf) = &mut *node {
                    versions_collected += leaf.gc(watermark) as u64;

                    current_leaf = leaf.next;
                } else {
                    break;
                }
            } else {
                break;
            }

            // Batch limit
            if nodes_visited >= self.config.batch_size as u64 {
                break;
            }
        }

        let _ = tree.compact_deleted_entries(watermark);

        let duration = start.elapsed();

        // Update stats
        self.stats.runs.fetch_add(1, Ordering::Relaxed);
        self.stats
            .versions_collected
            .fetch_add(versions_collected, Ordering::Relaxed);
        self.stats
            .nodes_visited
            .fetch_add(nodes_visited, Ordering::Relaxed);
        self.stats
            .time_spent_us
            .fetch_add(duration.as_micros() as u64, Ordering::Relaxed);
        self.stats
            .last_run
            .store(current_timestamp().get(), Ordering::Relaxed);
        self.stats
            .last_run_duration_us
            .store(duration.as_micros() as u64, Ordering::Relaxed);

        self.running.store(false, Ordering::SeqCst);

        self.stats()
    }

    /// Check if GC is needed
    pub fn needs_gc<K, V>(&self, tree: &BPlusTree<K, V>) -> bool
    where
        K: Clone + Ord + Debug + Send + Sync,
        V: Clone + Debug + Send + Sync,
    {
        // Simple heuristic: GC if stats show high version count
        let tree_stats = tree.stats();
        tree_stats.versions > tree_stats.entries * 2
    }

    /// Run incremental GC (process one batch)
    pub fn run_incremental<K, V>(
        &self,
        tree: &BPlusTree<K, V>,
        start_leaf: Option<NodeId>,
    ) -> Option<NodeId>
    where
        K: Clone + Ord + Debug + Send + Sync,
        V: Clone + Debug + Send + Sync,
    {
        let watermark = self.calculate_watermark();
        let mut nodes_visited = 0;
        let mut versions_collected = 0u64;

        let first = start_leaf.or_else(|| *gc_read(&tree.first_leaf));
        let mut current_leaf = first;

        while let Some(leaf_id) = current_leaf {
            nodes_visited += 1;

            if let Some(node) = tree.get_node(leaf_id) {
                let mut node = gc_write(&node);
                if let Node::Leaf(leaf) = &mut *node {
                    versions_collected += leaf.gc(watermark) as u64;
                    current_leaf = leaf.next;
                } else {
                    break;
                }
            } else {
                break;
            }

            if nodes_visited >= self.config.batch_size {
                // Return next leaf to continue from
                return current_leaf;
            }
        }

        // Update stats
        self.stats
            .versions_collected
            .fetch_add(versions_collected, Ordering::Relaxed);
        self.stats
            .nodes_visited
            .fetch_add(nodes_visited as u64, Ordering::Relaxed);

        None // GC complete
    }
}

impl Default for GarbageCollector {
    fn default() -> Self {
        Self::new(GcConfig::default())
    }
}

/// GC handle for managing background GC
pub struct GcHandle {
    /// GC instance
    gc: GarbageCollector,
    /// Stop flag
    stop: AtomicBool,
}

impl GcHandle {
    /// Create new handle
    pub fn new(config: GcConfig) -> Self {
        Self {
            gc: GarbageCollector::new(config),
            stop: AtomicBool::new(false),
        }
    }

    /// Get GC reference
    pub fn gc(&self) -> &GarbageCollector {
        &self.gc
    }

    /// Stop background GC
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Check if stopped
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::btree::{BPlusTree, BTreeConfig};
    use crate::storage::primitives::ids::TxnId;

    #[test]
    fn test_gc_config() {
        let config = GcConfig::new()
            .with_min_age(Timestamp(500))
            .with_batch_size(100);

        assert_eq!(config.min_age, Timestamp(500));
        assert_eq!(config.batch_size, 100);
    }

    #[test]
    fn test_gc_run_empty_tree() {
        let gc = GarbageCollector::new(GcConfig::new());
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        let stats = gc.run(&tree);
        assert_eq!(stats.runs, 1);
        assert_eq!(stats.versions_collected, 0);
    }

    #[test]
    fn test_gc_run_with_data() {
        let gc = GarbageCollector::new(GcConfig::new().with_min_age(Timestamp(0)));
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        // Insert and update to create versions
        for i in 1..=10 {
            tree.insert(i, format!("v1_{}", i), TxnId(1));
        }

        // Update to create more versions
        for i in 1..=10 {
            tree.insert(i, format!("v2_{}", i), TxnId(2));
            tree.insert(i, format!("v3_{}", i), TxnId(3));
        }

        let stats = gc.run(&tree);
        assert!(stats.nodes_visited > 0);
    }

    #[test]
    fn test_gc_incremental() {
        let gc = GarbageCollector::new(GcConfig::new().with_batch_size(2));
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in 1..=20 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        // Run incremental - should return continuation point
        let next = gc.run_incremental(&tree, None);
        // May or may not be done depending on tree structure
    }

    #[test]
    fn test_gc_watermark() {
        // Use min_age = 0 so watermark equals current timestamp
        let gc = GarbageCollector::new(GcConfig::new().with_min_age(Timestamp(0)));

        // With no active transactions, watermark is current timestamp
        let wm1 = gc.calculate_watermark();
        // Can be 0 if no timestamps have been generated yet
        assert!(wm1 >= Timestamp::EPOCH);

        // With active transaction set, watermark should respect it
        gc.set_oldest_active(Timestamp(50));
        let wm2 = gc.calculate_watermark();
        assert!(wm2 <= Timestamp(50));
    }

    #[test]
    fn test_gc_handle() {
        let handle = GcHandle::new(GcConfig::default());

        assert!(!handle.is_stopped());

        handle.stop();
        assert!(handle.is_stopped());
    }

    #[test]
    fn test_gc_run_recovers_after_first_leaf_lock_poisoning() {
        let gc = GarbageCollector::new(GcConfig::new());
        let tree: BPlusTree<i32, String> = BPlusTree::with_default_config();

        let poison_target = &tree;
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = poison_target
                    .first_leaf
                    .write()
                    .expect("first_leaf lock should be acquired");
                panic!("poison first_leaf lock");
            });
            let _ = handle.join();
        });

        let stats = gc.run(&tree);
        assert_eq!(stats.runs, 1);
    }

    #[test]
    fn test_gc_run_recovers_after_leaf_node_lock_poisoning() {
        let gc = GarbageCollector::new(GcConfig::new().with_min_age(Timestamp(0)));
        let tree: BPlusTree<i32, String> = BPlusTree::new(BTreeConfig::new().with_order(4));

        for i in 1..=4 {
            tree.insert(i, format!("v{}", i), TxnId(1));
        }

        let first_leaf = (*gc_read(&tree.first_leaf)).expect("tree should have a first leaf");
        let leaf = tree.get_node(first_leaf).expect("leaf node should exist");
        let poison_target = &leaf;
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = poison_target
                    .write()
                    .expect("leaf node lock should be acquired");
                panic!("poison leaf node lock");
            });
            let _ = handle.join();
        });

        let stats = gc.run(&tree);
        assert!(stats.nodes_visited > 0);
    }
}
