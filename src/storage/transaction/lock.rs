//! Lock Management for Transactions
//!
//! Provides pessimistic concurrency control with deadlock detection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{
    Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, WaitTimeoutResult,
};
use std::time::{Duration, Instant};

/// Transaction ID type
pub type TxnId = u64;

fn read_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn mutex_unpoisoned<'a, T>(lock: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_timeout_unpoisoned<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
    match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Lock modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockMode {
    /// Shared lock (read)
    Shared,
    /// Exclusive lock (write)
    Exclusive,
    /// Intent shared (for hierarchical locking)
    IntentShared,
    /// Intent exclusive (for hierarchical locking)
    IntentExclusive,
    /// Shared + Intent exclusive
    SharedIntentExclusive,
}

impl LockMode {
    /// Check if this mode is compatible with another
    pub fn is_compatible(&self, other: &LockMode) -> bool {
        use LockMode::*;
        matches!(
            (self, other),
            (Shared, Shared)
                | (Shared, IntentShared)
                | (IntentShared, Shared)
                | (IntentShared, IntentShared)
                | (IntentShared, IntentExclusive)
                | (IntentExclusive, IntentShared)
                | (IntentExclusive, IntentExclusive)
        )
    }

    /// Check if this mode can be upgraded to another
    pub fn can_upgrade_to(&self, target: &LockMode) -> bool {
        use LockMode::*;
        matches!(
            (self, target),
            (Shared, Exclusive)
                | (IntentShared, IntentExclusive)
                | (IntentShared, SharedIntentExclusive)
                | (Shared, SharedIntentExclusive)
        )
    }
}

/// Lock request result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    /// Lock granted immediately
    Granted,
    /// Lock request is waiting
    Waiting,
    /// Deadlock detected, request denied (contains cycle of transaction IDs)
    Deadlock(Vec<TxnId>),
    /// Timeout waiting for lock
    Timeout,
    /// Lock upgrade granted
    Upgraded,
    /// Lock already held
    AlreadyHeld,
    /// Transaction not found
    TxnNotFound,
    /// Lock limit exceeded for transaction
    LockLimitExceeded,
}

/// Lock waiter information
#[derive(Debug, Clone)]
pub struct LockWaiter {
    /// Transaction ID
    pub txn_id: TxnId,
    /// Requested lock mode
    pub mode: LockMode,
    /// When the wait started
    pub start_time: Instant,
    /// Maximum wait time
    pub timeout: Duration,
}

impl LockWaiter {
    /// Create new waiter
    pub fn new(txn_id: TxnId, mode: LockMode, timeout: Duration) -> Self {
        Self {
            txn_id,
            mode,
            start_time: Instant::now(),
            timeout,
        }
    }

    /// Check if wait has timed out
    pub fn is_timed_out(&self) -> bool {
        self.start_time.elapsed() > self.timeout
    }
}

/// A single lock on a resource
#[derive(Debug)]
struct Lock {
    /// Resource being locked
    resource: Vec<u8>,
    /// Current lock holders (txn_id -> mode)
    holders: HashMap<TxnId, LockMode>,
    /// Waiting requests
    waiters: VecDeque<LockWaiter>,
}

impl Lock {
    fn new(resource: Vec<u8>) -> Self {
        Self {
            resource,
            holders: HashMap::new(),
            waiters: VecDeque::new(),
        }
    }

    /// Check if a mode can be granted given current holders
    fn can_grant(&self, txn_id: TxnId, mode: LockMode) -> bool {
        // If we already hold it, check upgrade
        if let Some(held_mode) = self.holders.get(&txn_id) {
            if *held_mode == mode {
                return true; // Already have it
            }
            // Check if upgrade is possible
            if held_mode.can_upgrade_to(&mode) {
                // Can only upgrade if we're the only holder or others are compatible
                return self
                    .holders
                    .iter()
                    .all(|(id, m)| *id == txn_id || mode.is_compatible(m));
            }
            return false;
        }

        // Check compatibility with all holders
        self.holders.values().all(|m| mode.is_compatible(m))
    }

    /// Grant lock to transaction
    fn grant(&mut self, txn_id: TxnId, mode: LockMode) {
        self.holders.insert(txn_id, mode);
    }

    /// Release lock from transaction
    fn release(&mut self, txn_id: TxnId) -> Option<LockMode> {
        self.holders.remove(&txn_id)
    }

    /// Add waiter
    fn add_waiter(&mut self, waiter: LockWaiter) {
        self.waiters.push_back(waiter);
    }

    /// Process waiters after a release
    fn process_waiters(&mut self) -> Vec<TxnId> {
        let mut granted = Vec::new();

        // Remove timed out waiters
        self.waiters.retain(|w| !w.is_timed_out());

        // Try to grant waiting requests
        let mut i = 0;
        while i < self.waiters.len() {
            let waiter = &self.waiters[i];
            if self.can_grant(waiter.txn_id, waiter.mode) {
                if let Some(waiter) = self.waiters.remove(i) {
                    self.grant(waiter.txn_id, waiter.mode);
                    granted.push(waiter.txn_id);
                }
            } else {
                i += 1;
            }
        }

        granted
    }
}

/// Lock manager configuration
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// Default lock timeout
    pub default_timeout: Duration,
    /// Enable deadlock detection
    pub deadlock_detection: bool,
    /// Deadlock detection interval
    pub detection_interval: Duration,
    /// Maximum locks per transaction
    pub max_locks_per_txn: usize,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(30),
            deadlock_detection: true,
            detection_interval: Duration::from_millis(100),
            max_locks_per_txn: 10000,
        }
    }
}

/// Lock manager statistics
#[derive(Debug, Clone, Default)]
pub struct LockStats {
    /// Total lock requests
    pub requests: u64,
    /// Immediately granted
    pub granted: u64,
    /// Had to wait
    pub waited: u64,
    /// Deadlocks detected
    pub deadlocks: u64,
    /// Timeouts
    pub timeouts: u64,
    /// Current active locks
    pub active_locks: u64,
    /// Current waiting requests
    pub waiting: u64,
}

/// Lock manager for coordinating transaction locks
pub struct LockManager {
    /// Configuration
    config: LockConfig,
    /// Lock table: resource -> Lock
    locks: RwLock<HashMap<Vec<u8>, Lock>>,
    /// Transaction locks: txn_id -> resources held
    txn_locks: RwLock<HashMap<TxnId, HashSet<Vec<u8>>>>,
    /// Wait-for graph: txn_id -> txns it's waiting for
    wait_graph: RwLock<HashMap<TxnId, HashSet<TxnId>>>,
    /// Condition variable for waiters
    waiter_cv: Condvar,
    /// Mutex for condition variable
    waiter_mutex: Mutex<()>,
    /// Statistics
    stats: RwLock<LockStats>,
}

impl LockManager {
    /// Create new lock manager
    pub fn new(config: LockConfig) -> Self {
        Self {
            config,
            locks: RwLock::new(HashMap::new()),
            txn_locks: RwLock::new(HashMap::new()),
            wait_graph: RwLock::new(HashMap::new()),
            waiter_cv: Condvar::new(),
            waiter_mutex: Mutex::new(()),
            stats: RwLock::new(LockStats::default()),
        }
    }

    /// Create with default config
    pub fn with_defaults() -> Self {
        Self::new(LockConfig::default())
    }

    /// Acquire a lock
    pub fn acquire(&self, txn_id: TxnId, resource: &[u8], mode: LockMode) -> LockResult {
        self.acquire_with_timeout(txn_id, resource, mode, self.config.default_timeout)
    }

    /// Acquire lock with custom timeout
    pub fn acquire_with_timeout(
        &self,
        txn_id: TxnId,
        resource: &[u8],
        mode: LockMode,
        timeout: Duration,
    ) -> LockResult {
        // Update stats
        {
            let mut stats = write_unpoisoned(&self.stats);
            stats.requests += 1;
        }

        let resource_key = resource.to_vec();

        // Check lock limit
        {
            let txn_locks = read_unpoisoned(&self.txn_locks);
            if let Some(locks) = txn_locks.get(&txn_id) {
                if locks.len() >= self.config.max_locks_per_txn && !locks.contains(&resource_key) {
                    return LockResult::LockLimitExceeded;
                }
            }
        }

        // Try to acquire immediately
        {
            let mut locks = write_unpoisoned(&self.locks);
            let lock = locks
                .entry(resource_key.clone())
                .or_insert_with(|| Lock::new(resource_key.clone()));

            if lock.can_grant(txn_id, mode) {
                let already_held = lock.holders.contains_key(&txn_id);
                lock.grant(txn_id, mode);

                // Track in txn_locks
                let mut txn_locks = write_unpoisoned(&self.txn_locks);
                txn_locks.entry(txn_id).or_default().insert(resource_key);

                let mut stats = write_unpoisoned(&self.stats);
                stats.granted += 1;
                stats.active_locks = locks.values().map(|l| l.holders.len() as u64).sum();

                return if already_held {
                    LockResult::Upgraded
                } else {
                    LockResult::Granted
                };
            }

            // Can't grant immediately, need to wait
            let waiting_for: HashSet<TxnId> = lock.holders.keys().copied().collect();

            if self.config.deadlock_detection {
                let mut wait_graph = Self::build_wait_graph_from_locks(&locks);
                wait_graph
                    .entry(txn_id)
                    .or_default()
                    .extend(waiting_for.iter().copied());

                if self.detect_deadlock_inner(txn_id, &wait_graph) {
                    let cycle: Vec<TxnId> = waiting_for.iter().copied().collect();
                    let mut stats = write_unpoisoned(&self.stats);
                    stats.deadlocks += 1;
                    return LockResult::Deadlock(cycle);
                }

                *write_unpoisoned(&self.wait_graph) = wait_graph;
            }

            // Add to wait queue
            if let Some(lock) = locks.get_mut(&resource_key) {
                lock.add_waiter(LockWaiter::new(txn_id, mode, timeout));
            }

            let mut stats = write_unpoisoned(&self.stats);
            stats.waited += 1;
            stats.waiting += 1;
        }

        // Wait for lock
        let start = Instant::now();
        loop {
            // Wait on condition variable
            let guard = mutex_unpoisoned(&self.waiter_mutex);
            let (_guard, _wait_result) =
                wait_timeout_unpoisoned(&self.waiter_cv, guard, Duration::from_millis(10));

            // Check if we got the lock
            let holders: Option<HashSet<TxnId>> = {
                let locks = read_unpoisoned(&self.locks);
                if let Some(lock) = locks.get(&resource_key) {
                    if lock.holders.contains_key(&txn_id) {
                        // Remove from wait graph
                        if self.config.deadlock_detection {
                            let mut wait_graph = write_unpoisoned(&self.wait_graph);
                            wait_graph.remove(&txn_id);
                        }

                        let mut stats = write_unpoisoned(&self.stats);
                        stats.waiting -= 1;

                        return LockResult::Granted;
                    }

                    Some(lock.holders.keys().copied().collect())
                } else {
                    None
                }
            };

            if self.config.deadlock_detection {
                let locks = read_unpoisoned(&self.locks);
                let wait_graph = Self::build_wait_graph_from_locks(&locks);
                drop(locks);

                if self.detect_deadlock_inner(txn_id, &wait_graph) {
                    let mut stats = write_unpoisoned(&self.stats);
                    stats.deadlocks += 1;
                    stats.waiting -= 1;
                    return LockResult::Deadlock(holders.unwrap_or_default().into_iter().collect());
                }

                *write_unpoisoned(&self.wait_graph) = wait_graph;
            }

            // Check timeout
            if start.elapsed() > timeout {
                // Remove from wait queue
                {
                    let mut locks = write_unpoisoned(&self.locks);
                    if let Some(lock) = locks.get_mut(&resource_key) {
                        lock.waiters.retain(|w| w.txn_id != txn_id);
                    }
                }

                // Remove from wait graph
                if self.config.deadlock_detection {
                    let mut wait_graph = write_unpoisoned(&self.wait_graph);
                    wait_graph.remove(&txn_id);
                }

                let mut stats = write_unpoisoned(&self.stats);
                stats.timeouts += 1;
                stats.waiting -= 1;

                return LockResult::Timeout;
            }
        }
    }

    /// Release a lock
    pub fn release(&self, txn_id: TxnId, resource: &[u8]) -> bool {
        let resource_key = resource.to_vec();

        let granted = {
            let mut locks = write_unpoisoned(&self.locks);

            if let Some(lock) = locks.get_mut(&resource_key) {
                if lock.release(txn_id).is_some() {
                    // Remove from txn_locks
                    let mut txn_locks = write_unpoisoned(&self.txn_locks);
                    if let Some(resources) = txn_locks.get_mut(&txn_id) {
                        resources.remove(&resource_key);
                    }

                    // Process waiters
                    let granted = lock.process_waiters();

                    // Update wait graph for granted transactions
                    if self.config.deadlock_detection && !granted.is_empty() {
                        let mut wait_graph = write_unpoisoned(&self.wait_graph);
                        for txn in &granted {
                            wait_graph.remove(txn);
                        }
                    }

                    // Clean up empty lock
                    if lock.holders.is_empty() && lock.waiters.is_empty() {
                        locks.remove(&resource_key);
                    }

                    // Notify waiters
                    self.waiter_cv.notify_all();

                    return true;
                }
            }

            false
        };

        granted
    }

    /// Release all locks for a transaction
    pub fn release_all(&self, txn_id: TxnId) -> usize {
        let resources: Vec<Vec<u8>> = {
            let txn_locks = read_unpoisoned(&self.txn_locks);
            txn_locks
                .get(&txn_id)
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default()
        };

        let count = resources.len();

        for resource in resources {
            self.release(txn_id, &resource);
        }

        // Clean up txn_locks entry
        {
            let mut txn_locks = write_unpoisoned(&self.txn_locks);
            txn_locks.remove(&txn_id);
        }

        // Clean up wait graph
        if self.config.deadlock_detection {
            let mut wait_graph = write_unpoisoned(&self.wait_graph);
            wait_graph.remove(&txn_id);
        }

        count
    }

    /// Check if transaction holds lock on resource
    pub fn holds_lock(&self, txn_id: TxnId, resource: &[u8]) -> Option<LockMode> {
        let locks = read_unpoisoned(&self.locks);
        locks
            .get(resource)
            .and_then(|lock| lock.holders.get(&txn_id).copied())
    }

    /// Get all locks held by transaction
    pub fn get_locks(&self, txn_id: TxnId) -> Vec<(Vec<u8>, LockMode)> {
        let txn_locks = read_unpoisoned(&self.txn_locks);
        let locks = read_unpoisoned(&self.locks);

        txn_locks
            .get(&txn_id)
            .map(|resources| {
                resources
                    .iter()
                    .filter_map(|r| {
                        locks
                            .get(r)
                            .and_then(|l| l.holders.get(&txn_id).map(|m| (r.clone(), *m)))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Detect deadlock using DFS on wait-for graph
    fn detect_deadlock_inner(
        &self,
        start: TxnId,
        wait_graph: &HashMap<TxnId, HashSet<TxnId>>,
    ) -> bool {
        let mut visited = HashSet::new();
        let mut stack = HashSet::new();

        Self::dfs_cycle(start, &mut visited, &mut stack, wait_graph)
    }

    fn build_wait_graph_from_locks(
        locks: &HashMap<Vec<u8>, Lock>,
    ) -> HashMap<TxnId, HashSet<TxnId>> {
        let mut graph: HashMap<TxnId, HashSet<TxnId>> = HashMap::new();

        for lock in locks.values() {
            if lock.holders.is_empty() {
                continue;
            }
            let holders: HashSet<TxnId> = lock.holders.keys().copied().collect();
            for waiter in &lock.waiters {
                graph
                    .entry(waiter.txn_id)
                    .or_default()
                    .extend(holders.iter().copied());
            }
        }

        graph
    }

    fn dfs_cycle(
        node: TxnId,
        visited: &mut HashSet<TxnId>,
        stack: &mut HashSet<TxnId>,
        wait_graph: &HashMap<TxnId, HashSet<TxnId>>,
    ) -> bool {
        if stack.contains(&node) {
            return true; // Cycle found
        }
        if visited.contains(&node) {
            return false; // Already processed
        }

        visited.insert(node);
        stack.insert(node);

        if let Some(waiting_for) = wait_graph.get(&node) {
            for &next in waiting_for {
                if Self::dfs_cycle(next, visited, stack, wait_graph) {
                    return true;
                }
            }
        }

        stack.remove(&node);
        false
    }

    /// Get statistics
    pub fn stats(&self) -> LockStats {
        read_unpoisoned(&self.stats).clone()
    }

    /// Get configuration
    pub fn config(&self) -> &LockConfig {
        &self.config
    }
}

impl Default for LockManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_mode_compatibility() {
        assert!(LockMode::Shared.is_compatible(&LockMode::Shared));
        assert!(!LockMode::Shared.is_compatible(&LockMode::Exclusive));
        assert!(!LockMode::Exclusive.is_compatible(&LockMode::Exclusive));
        assert!(LockMode::IntentShared.is_compatible(&LockMode::IntentShared));
    }

    #[test]
    fn test_lock_acquire_release() {
        let lm = LockManager::with_defaults();

        // Acquire shared lock
        let result = lm.acquire(1, b"resource1", LockMode::Shared);
        assert_eq!(result, LockResult::Granted);

        // Another transaction can get shared lock
        let result = lm.acquire(2, b"resource1", LockMode::Shared);
        assert_eq!(result, LockResult::Granted);

        // Release locks
        assert!(lm.release(1, b"resource1"));
        assert!(lm.release(2, b"resource1"));
    }

    #[test]
    fn test_exclusive_lock() {
        let lm = LockManager::with_defaults();

        // Acquire exclusive lock
        let result = lm.acquire(1, b"resource1", LockMode::Exclusive);
        assert_eq!(result, LockResult::Granted);

        // Check held
        assert_eq!(lm.holds_lock(1, b"resource1"), Some(LockMode::Exclusive));

        // Release
        lm.release_all(1);
        assert_eq!(lm.holds_lock(1, b"resource1"), None);
    }

    #[test]
    fn test_release_all() {
        let lm = LockManager::with_defaults();

        // Acquire multiple locks
        lm.acquire(1, b"r1", LockMode::Shared);
        lm.acquire(1, b"r2", LockMode::Exclusive);
        lm.acquire(1, b"r3", LockMode::Shared);

        // Release all
        let count = lm.release_all(1);
        assert_eq!(count, 3);
    }

    #[test]
    fn test_lock_limit_exceeded() {
        let config = LockConfig {
            max_locks_per_txn: 1,
            ..LockConfig::default()
        };
        let lm = LockManager::new(config);

        let result = lm.acquire(1, b"r1", LockMode::Shared);
        assert_eq!(result, LockResult::Granted);

        let result = lm.acquire(1, b"r2", LockMode::Shared);
        assert_eq!(result, LockResult::LockLimitExceeded);
    }

    #[test]
    fn test_lock_limit_allows_upgrade() {
        let config = LockConfig {
            max_locks_per_txn: 1,
            ..LockConfig::default()
        };
        let lm = LockManager::new(config);

        let result = lm.acquire(1, b"r1", LockMode::Shared);
        assert_eq!(result, LockResult::Granted);

        let result = lm.acquire(1, b"r1", LockMode::Exclusive);
        assert_eq!(result, LockResult::Upgraded);
    }

    #[test]
    fn test_get_locks() {
        let lm = LockManager::with_defaults();

        lm.acquire(1, b"r1", LockMode::Shared);
        lm.acquire(1, b"r2", LockMode::Exclusive);

        let locks = lm.get_locks(1);
        assert_eq!(locks.len(), 2);
    }

    #[test]
    fn test_waiter_timeout() {
        let waiter = LockWaiter::new(1, LockMode::Shared, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(waiter.is_timed_out());
    }
}
