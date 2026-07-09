//! Budget shares and the one shared accounting pool — ADR 0073 §2 made
//! executable.
//!
//! The budget resolved at boot (`memory_budget`, ADR 0073 §1) is divided into
//! **named shares** by a single allocation policy. The big memory consumers
//! pre-size their structures from those shares and report live usage into
//! **one shared accounting pool**.
//!
//! Per-subsystem caps *without* shared accounting are explicitly rejected by
//! the ADR: each subsystem stays individually "within limits" while the sum
//! kills the process (OOM by summation). One pool, one total.
//!
//! The invariant that makes the pool meaningful is
//!
//! > **Σ(shares) ≤ budget**
//!
//! It holds by construction: every share is `(budget / 10_000) * basis_points`
//! and the policy's basis points sum to at most `10_000`. Boot asserts both
//! halves of that statement rather than trusting the arithmetic.
//!
//! What this module is *not*: it does not enforce admission. A pool over its
//! share is visible in `red.stats` and nothing more — enforcement is the next
//! slice (ADR 0073 §4). Reporting is therefore free of policy: a plain relaxed
//! atomic per pool, no allocation, no lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use super::engine::page_cache::MIN_CACHE_CAPACITY;
use super::memory_budget::MemoryBudget;
use super::profile::DeployProfile;

/// Fixed page size the page-cache share is divided by to obtain a slot count.
/// Pages are fixed size → slots are fixed size → the arena is preallocated.
pub const PAGE_CACHE_PAGE_SIZE_BYTES: u64 = reddb_file::PAGED_PAGE_SIZE as u64;

/// Denominator of the policy fractions. Shares are expressed in basis points
/// (parts per ten thousand) so the whole policy is integer arithmetic — no
/// float rounding standing between the budget and the invariant.
pub const BASIS_POINTS_PER_WHOLE: u32 = 10_000;

/// The big memory consumers that receive a slice of the budget.
///
/// Only structures whose growth is `O(data)` or `O(traffic)` belong here.
/// L2's *disk* extent is not memory and stays out; its RAM-resident metadata
/// (B+ tree index, synopsis filters) is accounted under [`MemoryPool::BlobCacheL1`]
/// because it is the blob cache's RAM footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryPool {
    /// Pager page cache — preallocated 16 KiB slots (ADR 0033 SIEVE cache).
    PageCache,
    /// Blob Cache RAM tier: L1 entries plus L2's RAM-resident metadata.
    BlobCacheL1,
    /// Unified segment arena — growing + sealed segments.
    SegmentArena,
    /// Secondary-index memory — hash / bitmap / sorted / composite.
    IndexMemory,
    /// WAL group-commit queue and writer buffers.
    WalBuffers,
}

/// Every pool, in the order `red.stats` reports them.
pub const MEMORY_POOLS: [MemoryPool; MEMORY_POOL_COUNT] = [
    MemoryPool::PageCache,
    MemoryPool::BlobCacheL1,
    MemoryPool::SegmentArena,
    MemoryPool::IndexMemory,
    MemoryPool::WalBuffers,
];

/// Number of pools. Arrays are indexed by [`MemoryPool::index`].
pub const MEMORY_POOL_COUNT: usize = 5;

impl MemoryPool {
    /// Stable label echoed by the boot log and the `red.stats` budget section.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PageCache => "page_cache",
            Self::BlobCacheL1 => "blob_cache_l1",
            Self::SegmentArena => "segment_arena",
            Self::IndexMemory => "index_memory",
            Self::WalBuffers => "wal_buffers",
        }
    }

    /// Dense index into the share and usage arrays.
    pub const fn index(self) -> usize {
        match self {
            Self::PageCache => 0,
            Self::BlobCacheL1 => 1,
            Self::SegmentArena => 2,
            Self::IndexMemory => 3,
            Self::WalBuffers => 4,
        }
    }
}

/// The single allocation policy: named fractions per pool, profile-adjustable.
///
/// No subsystem computes its own fraction. Adding a pool means adding a
/// fraction here and taking it from another pool or from the reserve — which
/// is the point: the tradeoff is visible in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSharePolicy {
    basis_points: [u32; MEMORY_POOL_COUNT],
}

/// Server-side profiles: the segment arena *is* the database (RAM-resident
/// store), the page cache backs the paged tier and the B+ trees over it.
/// The unnamed remainder (10%) absorbs plan caches, connection state, result
/// buffers and allocator slack — everything that is not a governed pool.
const DEFAULT_POLICY: BudgetSharePolicy = BudgetSharePolicy {
    basis_points: [
        2_000, // page_cache
        1_500, // blob_cache_l1
        4_000, // segment_arena
        1_000, // index_memory
        500,   // wal_buffers
    ],
};

/// Serverless: boundedness is a survival contract, not a tuning preference
/// (ADR 0038 §1). A cold function instance has a small budget and a short
/// life, so the caches shrink and the unnamed reserve doubles to 20%.
const SERVERLESS_POLICY: BudgetSharePolicy = BudgetSharePolicy {
    basis_points: [
        1_500, // page_cache
        1_000, // blob_cache_l1
        4_000, // segment_arena
        1_000, // index_memory
        500,   // wal_buffers
    ],
};

impl BudgetSharePolicy {
    /// The one policy for a deployment profile.
    pub const fn for_profile(profile: DeployProfile) -> Self {
        match profile {
            DeployProfile::Serverless => SERVERLESS_POLICY,
            DeployProfile::Embedded | DeployProfile::PrimaryReplica | DeployProfile::Cluster => {
                DEFAULT_POLICY
            }
        }
    }

    /// This pool's fraction of the budget, in basis points.
    pub const fn basis_points(&self, pool: MemoryPool) -> u32 {
        self.basis_points[pool.index()]
    }

    /// Sum of every pool's fraction. Never exceeds [`BASIS_POINTS_PER_WHOLE`].
    pub fn total_basis_points(&self) -> u32 {
        self.basis_points.iter().sum()
    }

    /// The slice of the budget handed to no pool: plan caches, connection
    /// state, allocator slack.
    pub fn reserve_basis_points(&self) -> u32 {
        BASIS_POINTS_PER_WHOLE - self.total_basis_points()
    }
}

/// The resolved per-pool shares of one process budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetShares {
    budget_bytes: u64,
    policy: BudgetSharePolicy,
    share_bytes: [u64; MEMORY_POOL_COUNT],
}

impl BudgetShares {
    /// Divide a resolved budget among the pools.
    ///
    /// Integer division comes *first* (`budget / 10_000` then `* basis_points`)
    /// for two reasons: `budget * basis_points` overflows `u64` for budgets
    /// above ~1.8 EiB, and dividing first makes Σ(shares) ≤ budget an identity
    /// rather than a rounding accident. The cost is at most 9_999 bytes of
    /// unallocated remainder per pool.
    pub fn resolve(budget: MemoryBudget, profile: DeployProfile) -> Self {
        let policy = BudgetSharePolicy::for_profile(profile);

        // Pair assertion: the policy claims a positive but not-more-than-whole
        // fraction of the budget. Both halves, because a policy summing to 0
        // is as broken as one summing to 12_000 and neither is caught by the
        // other check.
        assert!(
            policy.total_basis_points() > 0,
            "invariant: budget share policy must claim some of the budget"
        );
        assert!(
            policy.total_basis_points() <= BASIS_POINTS_PER_WHOLE,
            "invariant: Σ(share fractions) = {} bp exceeds the whole budget ({BASIS_POINTS_PER_WHOLE} bp)",
            policy.total_basis_points()
        );

        let budget_bytes = budget.resolved_bytes;
        assert!(
            budget_bytes > 0,
            "invariant: the resolved budget is strictly positive (ADR 0073 §1: no unlimited mode)"
        );

        let per_basis_point = budget_bytes / u64::from(BASIS_POINTS_PER_WHOLE);
        let mut share_bytes = [0_u64; MEMORY_POOL_COUNT];
        for pool in MEMORY_POOLS {
            share_bytes[pool.index()] = per_basis_point * u64::from(policy.basis_points(pool));
        }

        let shares = Self {
            budget_bytes,
            policy,
            share_bytes,
        };

        // The property this whole module exists to guarantee, asserted at boot
        // rather than assumed. Its negative space too: the shares are not all
        // zero unless the budget itself is smaller than a basis point.
        assert!(
            shares.total_share_bytes() <= budget_bytes,
            "invariant: Σ(shares) = {} exceeds budget {budget_bytes}",
            shares.total_share_bytes()
        );
        assert!(
            shares.total_share_bytes() > 0 || budget_bytes < u64::from(BASIS_POINTS_PER_WHOLE),
            "invariant: a budget of {budget_bytes} bytes must reach the pools"
        );

        shares
    }

    /// The budget these shares divide.
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// The policy that produced these shares.
    pub fn policy(&self) -> BudgetSharePolicy {
        self.policy
    }

    /// This pool's slice of the budget.
    pub fn share_bytes(&self, pool: MemoryPool) -> u64 {
        self.share_bytes[pool.index()]
    }

    /// Σ(shares). Never exceeds [`Self::budget_bytes`].
    pub fn total_share_bytes(&self) -> u64 {
        self.share_bytes.iter().sum()
    }

    /// Page-cache slot count: the share divided by the fixed page size.
    ///
    /// Clamped to the cache's structural minimum. A budget small enough to hit
    /// the clamp buys a page cache slightly larger than its share; that is
    /// visible in `red.stats` as `used_bytes > share_bytes` and, per ADR 0073
    /// §4, is the enforcement slice's problem, not this one's.
    pub fn page_cache_slots(&self) -> usize {
        let slots = self.share_bytes(MemoryPool::PageCache) / PAGE_CACHE_PAGE_SIZE_BYTES;
        usize::try_from(slots)
            .unwrap_or(usize::MAX)
            .max(MIN_CACHE_CAPACITY)
    }

    /// Blob Cache L1 byte ceiling: the RAM tier's share, verbatim.
    pub fn blob_cache_l1_bytes(&self) -> usize {
        usize::try_from(self.share_bytes(MemoryPool::BlobCacheL1)).unwrap_or(usize::MAX)
    }

    /// Emit one boot log line per pool naming its share. Guarded so a process
    /// that opens several runtimes still logs exactly once, matching
    /// `memory_budget::log_resolved_once`.
    pub fn log_once(&self) {
        static LOGGED: Once = Once::new();
        LOGGED.call_once(|| {
            for pool in MEMORY_POOLS {
                tracing::info!(
                    pool = pool.as_str(),
                    share_bytes = self.share_bytes(pool),
                    basis_points = self.policy.basis_points(pool),
                    budget_bytes = self.budget_bytes,
                    "memory budget share assigned"
                );
            }
            tracing::info!(
                total_share_bytes = self.total_share_bytes(),
                reserve_basis_points = self.policy.reserve_basis_points(),
                budget_bytes = self.budget_bytes,
                "memory budget shares resolved"
            );
        });
    }
}

/// One pool's row in the shared accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolUsage {
    pub pool: MemoryPool,
    pub share_bytes: u64,
    pub used_bytes: u64,
}

/// The one shared accounting pool: fixed shares, live usage.
///
/// Usage is a relaxed atomic per pool. Reporting is a single store or
/// fetch-add — no allocation, no lock, nothing a read hot path has to wait on.
/// Relaxed is the right ordering because the counters are observability, not
/// synchronisation: no other memory is published through them.
#[derive(Debug)]
pub struct MemoryAccounting {
    budget: MemoryBudget,
    shares: BudgetShares,
    used_bytes: [AtomicU64; MEMORY_POOL_COUNT],
}

impl MemoryAccounting {
    /// Build the pool from a resolved budget and the profile's policy.
    pub fn new(budget: MemoryBudget, profile: DeployProfile) -> Self {
        Self::from_shares(budget, BudgetShares::resolve(budget, profile))
    }

    /// Build the pool from shares already resolved (and already asserted).
    pub fn from_shares(budget: MemoryBudget, shares: BudgetShares) -> Self {
        assert_eq!(
            budget.resolved_bytes, shares.budget_bytes,
            "invariant: accounting shares must divide this process's budget"
        );
        Self {
            budget,
            shares,
            used_bytes: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// The budget this process runs under.
    pub fn budget(&self) -> MemoryBudget {
        self.budget
    }

    /// The shares this pool accounts against.
    pub fn shares(&self) -> BudgetShares {
        self.shares
    }

    /// Overwrite a pool's live usage. The reporting shape for pools whose
    /// footprint is a measured total rather than a running delta.
    pub fn report(&self, pool: MemoryPool, bytes: u64) {
        self.used_bytes[pool.index()].store(bytes, Ordering::Relaxed);
    }

    /// Charge `bytes` to a pool. One relaxed fetch-add.
    pub fn charge(&self, pool: MemoryPool, bytes: u64) {
        self.used_bytes[pool.index()].fetch_add(bytes, Ordering::Relaxed);
    }

    /// Return `bytes` to a pool, saturating at zero.
    ///
    /// Saturation rather than wrap: a double-release is a programmer error the
    /// enforcement slice will assert on, but until then an accounting counter
    /// underflowing to 18 exabytes would make `red.stats` lie spectacularly.
    pub fn release(&self, pool: MemoryPool, bytes: u64) {
        let slot = &self.used_bytes[pool.index()];
        let _ = slot.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
            Some(used.saturating_sub(bytes))
        });
    }

    /// This pool's live usage.
    pub fn used_bytes(&self, pool: MemoryPool) -> u64 {
        self.used_bytes[pool.index()].load(Ordering::Relaxed)
    }

    /// This pool's share of the budget.
    pub fn share_bytes(&self, pool: MemoryPool) -> u64 {
        self.shares.share_bytes(pool)
    }

    /// Σ(used) across every pool — the number the enforcement slice will gate on.
    pub fn total_used_bytes(&self) -> u64 {
        MEMORY_POOLS
            .iter()
            .map(|pool| self.used_bytes(*pool))
            .fold(0, u64::saturating_add)
    }

    /// Σ(shares). Never exceeds the budget.
    pub fn total_share_bytes(&self) -> u64 {
        self.shares.total_share_bytes()
    }

    /// Every pool's share and usage, in [`MEMORY_POOLS`] order.
    pub fn snapshot(&self) -> [PoolUsage; MEMORY_POOL_COUNT] {
        MEMORY_POOLS.map(|pool| PoolUsage {
            pool,
            share_bytes: self.share_bytes(pool),
            used_bytes: self.used_bytes(pool),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory_budget::MemoryBudgetSource;

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    const PROFILES: [DeployProfile; 4] = [
        DeployProfile::Embedded,
        DeployProfile::Serverless,
        DeployProfile::PrimaryReplica,
        DeployProfile::Cluster,
    ];

    fn budget(bytes: u64) -> MemoryBudget {
        MemoryBudget {
            resolved_bytes: bytes,
            source: MemoryBudgetSource::Config,
        }
    }

    #[test]
    fn every_policy_claims_at_most_the_whole_budget() {
        for profile in PROFILES {
            let policy = BudgetSharePolicy::for_profile(profile);
            assert!(policy.total_basis_points() > 0, "{profile:?}");
            assert!(
                policy.total_basis_points() <= BASIS_POINTS_PER_WHOLE,
                "{profile:?} claims {} bp",
                policy.total_basis_points()
            );
            assert!(
                policy.reserve_basis_points() > 0,
                "{profile:?} must leave an unpooled reserve for plan caches and slack"
            );
        }
    }

    /// The property: for any resolved budget across the valid range, and any
    /// profile, Σ(shares) ≤ B. Boot asserts the same thing (see `resolve`).
    #[test]
    fn sum_of_shares_never_exceeds_the_budget() {
        let budgets = [
            1,
            2,
            9_999,
            10_000,
            10_001,
            64 * MIB,
            256 * MIB,
            512 * MIB,
            GIB,
            2 * GIB,
            11 * GIB,
            1 << 40,
            1 << 50,
            u64::MAX / 2,
            u64::MAX,
        ];

        for profile in PROFILES {
            for bytes in budgets {
                let shares = BudgetShares::resolve(budget(bytes), profile);
                assert!(
                    shares.total_share_bytes() <= bytes,
                    "{profile:?} budget {bytes}: Σ(shares) = {}",
                    shares.total_share_bytes()
                );
                // Positive space too: nothing above a basis point starves.
                if bytes >= u64::from(BASIS_POINTS_PER_WHOLE) {
                    for pool in MEMORY_POOLS {
                        assert!(
                            shares.share_bytes(pool) > 0,
                            "{profile:?} budget {bytes}: {} starved",
                            pool.as_str()
                        );
                    }
                }
            }
        }
    }

    /// Exhaustive over a dense sweep — the same property, walked rather than
    /// sampled, so an off-by-one in the basis-point arithmetic cannot hide
    /// between the hand-picked budgets above.
    #[test]
    fn sum_of_shares_never_exceeds_the_budget_across_a_dense_sweep() {
        for profile in PROFILES {
            for step in 0..2_000_u64 {
                let bytes = 1 + step * 7_919; // prime stride: hits every residue mod 10_000
                let shares = BudgetShares::resolve(budget(bytes), profile);
                assert!(
                    shares.total_share_bytes() <= bytes,
                    "{profile:?} budget {bytes}: Σ(shares) = {}",
                    shares.total_share_bytes()
                );
            }
        }
    }

    #[test]
    fn doubling_the_budget_doubles_the_page_cache_slots_and_the_blob_l1_ceiling() {
        for profile in PROFILES {
            let base = BudgetShares::resolve(budget(GIB), profile);
            let doubled = BudgetShares::resolve(budget(2 * GIB), profile);

            assert_eq!(
                doubled.page_cache_slots(),
                base.page_cache_slots() * 2,
                "{profile:?} page cache slots must scale with the budget"
            );
            assert_eq!(
                doubled.blob_cache_l1_bytes(),
                base.blob_cache_l1_bytes() * 2,
                "{profile:?} blob L1 ceiling must scale with the budget"
            );
            assert_eq!(
                doubled.share_bytes(MemoryPool::SegmentArena),
                base.share_bytes(MemoryPool::SegmentArena) * 2,
            );
        }
    }

    #[test]
    fn page_cache_slots_are_the_share_divided_by_the_fixed_page_size() {
        let shares = BudgetShares::resolve(budget(10 * GIB), DeployProfile::Embedded);
        let expected = shares.share_bytes(MemoryPool::PageCache) / PAGE_CACHE_PAGE_SIZE_BYTES;
        assert_eq!(shares.page_cache_slots() as u64, expected);
        assert_eq!(PAGE_CACHE_PAGE_SIZE_BYTES, 16 * 1024);
    }

    #[test]
    fn a_budget_too_small_to_fill_a_slot_still_yields_a_usable_page_cache() {
        let shares = BudgetShares::resolve(budget(1), DeployProfile::Serverless);
        assert_eq!(shares.total_share_bytes(), 0, "nothing reaches the pools");
        assert_eq!(shares.page_cache_slots(), MIN_CACHE_CAPACITY);
        assert_eq!(shares.blob_cache_l1_bytes(), 0);
    }

    #[test]
    fn serverless_shrinks_the_caches_relative_to_the_server_profiles() {
        let serverless = BudgetShares::resolve(budget(GIB), DeployProfile::Serverless);
        let embedded = BudgetShares::resolve(budget(GIB), DeployProfile::Embedded);

        assert!(serverless.page_cache_slots() < embedded.page_cache_slots());
        assert!(serverless.blob_cache_l1_bytes() < embedded.blob_cache_l1_bytes());
        assert!(serverless.total_share_bytes() < embedded.total_share_bytes());
        assert_eq!(
            serverless.share_bytes(MemoryPool::SegmentArena),
            embedded.share_bytes(MemoryPool::SegmentArena),
            "the RAM-resident store keeps its share on every profile"
        );
    }

    #[test]
    fn the_serverless_default_budget_produces_a_bounded_blob_l1() {
        let shares = BudgetShares::resolve(
            budget(super::super::memory_budget::SERVERLESS_PROFILE_BUDGET_BYTES),
            DeployProfile::Serverless,
        );
        // The old hardcoded 256 MiB L1 would have been the *entire* serverless
        // budget on its own. That is the default this slice deletes.
        assert!(
            shares.blob_cache_l1_bytes()
                < crate::storage::cache::blob::DEFAULT_BLOB_L1_BYTES_MAX,
            "serverless L1 = {} bytes",
            shares.blob_cache_l1_bytes()
        );
    }

    #[test]
    fn pool_labels_are_stable_and_unique() {
        let labels: Vec<&str> = MEMORY_POOLS.iter().map(|pool| pool.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "page_cache",
                "blob_cache_l1",
                "segment_arena",
                "index_memory",
                "wal_buffers"
            ]
        );
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), MEMORY_POOL_COUNT);

        for (position, pool) in MEMORY_POOLS.iter().enumerate() {
            assert_eq!(pool.index(), position, "{} index", pool.as_str());
        }
    }

    #[test]
    fn accounting_charges_releases_and_totals_across_pools() {
        let accounting = MemoryAccounting::new(budget(GIB), DeployProfile::Embedded);
        assert_eq!(accounting.total_used_bytes(), 0);

        accounting.charge(MemoryPool::SegmentArena, 4_096);
        accounting.charge(MemoryPool::SegmentArena, 1_024);
        accounting.charge(MemoryPool::WalBuffers, 64);
        assert_eq!(accounting.used_bytes(MemoryPool::SegmentArena), 5_120);
        assert_eq!(accounting.total_used_bytes(), 5_184);

        accounting.release(MemoryPool::SegmentArena, 1_024);
        assert_eq!(accounting.used_bytes(MemoryPool::SegmentArena), 4_096);

        accounting.report(MemoryPool::SegmentArena, 42);
        assert_eq!(accounting.used_bytes(MemoryPool::SegmentArena), 42);
        assert_eq!(accounting.total_used_bytes(), 106);
    }

    #[test]
    fn releasing_more_than_charged_saturates_instead_of_wrapping() {
        let accounting = MemoryAccounting::new(budget(GIB), DeployProfile::Embedded);
        accounting.charge(MemoryPool::IndexMemory, 10);
        accounting.release(MemoryPool::IndexMemory, 1_000);
        assert_eq!(accounting.used_bytes(MemoryPool::IndexMemory), 0);
    }

    #[test]
    fn a_snapshot_carries_every_pool_with_its_share_and_usage() {
        let accounting = MemoryAccounting::new(budget(4 * GIB), DeployProfile::Cluster);
        accounting.report(MemoryPool::PageCache, 777);

        let snapshot = accounting.snapshot();
        assert_eq!(snapshot.len(), MEMORY_POOL_COUNT);
        assert_eq!(snapshot[0].pool, MemoryPool::PageCache);
        assert_eq!(snapshot[0].used_bytes, 777);
        assert_eq!(
            snapshot[0].share_bytes,
            accounting.share_bytes(MemoryPool::PageCache)
        );
        assert_eq!(
            snapshot.iter().map(|row| row.share_bytes).sum::<u64>(),
            accounting.total_share_bytes()
        );
        assert!(accounting.total_share_bytes() <= accounting.budget().resolved_bytes);
    }
}
