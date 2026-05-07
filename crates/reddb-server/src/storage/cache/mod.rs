//! Cache Module
//!
//! High-performance caching infrastructure for RedDB.
//!
//! # Components
//!
//! - **sieve**: SIEVE page cache for database pages (O(1) operations)
//! - **blob**: Byte-oriented L1 cache for exact-key cached blobs
//! - **result**: Query result cache with dependency-based invalidation
//! - **aggregates**: Precomputed aggregations (COUNT, SUM, AVG, etc.)
//! - **spill**: Graph spill-to-disk for memory-limited environments
//!
//! # Architecture (inspired by Turso/Milvus/Neo4j)
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │                    Query Layer                         │
//! ├────────────────────────────────────────────────────────┤
//! │  Result Cache   │  Materialized Views  │  Plan Cache   │
//! ├────────────────────────────────────────────────────────┤
//! │           Aggregation Cache (COUNT/SUM/AVG)            │
//! ├────────────────────────────────────────────────────────┤
//! │   SIEVE Page Cache    │     Spill Manager              │
//! ├────────────────────────────────────────────────────────┤
//! │                   Storage Engine                       │
//! └────────────────────────────────────────────────────────┘
//! ```

pub mod aggregates;
pub mod bgwriter;
pub mod blob;
pub mod compressor;
pub mod extended_ttl;
pub mod promotion_pool;
pub mod result;
pub mod ring;
pub mod sieve;
pub mod spill;
pub mod strategy;
pub mod sweeper;

pub use aggregates::{AggCacheStats, AggValue, AggregationCache, CardinalityEstimate, NumericAgg};
pub use compressor::{CompressError, CompressOpts, Compressed, L2BlobCompressor};
pub use extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};
pub use promotion_pool::{
    AsyncPromotionPool, PoolOpts, PromotionExecutor, PromotionMetrics, PromotionRequest,
    ScheduleOutcome,
};
pub use blob::{
    BlobCache, BlobCacheConfig, BlobCacheHit, BlobCachePolicy, BlobCachePut, BlobCacheStats,
    CacheError, L1Admission, L2Compression, DEFAULT_BLOB_L1_BYTES_MAX,
    DEFAULT_BLOB_L2_BYTES_MAX, DEFAULT_BLOB_MAX_NAMESPACES,
    METRIC_CACHE_BLOB_L1_BYTES_IN_USE, METRIC_CACHE_BLOB_L2_BYTES_IN_USE,
    METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL, METRIC_CACHE_VERSION_MISMATCH_TOTAL,
};
pub use result::{
    CacheKey, CachePolicy, MaterializedViewCache, MaterializedViewDef, RefreshPolicy, ResultCache,
    ResultCacheStats,
};
pub use ring::BufferRing;
pub use sieve::{CacheConfig, CacheStats, PageCache, PageId};
pub use spill::{SpillConfig, SpillError, SpillManager, SpillStats, SpillableGraph};
pub use strategy::BufferAccessStrategy;

// ---------------------------------------------------------------------------
// L2 Blob Cache backup helpers (issue #148 follow-up)
// ---------------------------------------------------------------------------
//
// `BlobCache` writes the L2 metadata B+ tree and blob chains into a
// single pager file at `cache.blob.l2_path` plus a sidecar control file
// at `<l2_path>.blob-cache.ctl` (see
// `cache/blob.rs::BlobCacheL2::open`). Both files are required for a
// usable restore — the pager file holds the data, the control file
// holds the root-page pointer + bytes-in-use.
//
// When the operator opts into `include_blob_cache=true` on a backup
// (`red.config.backup.include_blob_cache`), we upload both files to
// the configured remote backend under a stable prefix.
//
// Shape contract:
//
// - Two remote keys: `{prefix}l2.pager` (the pager file) and
//   `{prefix}l2.ctl` (the control sidecar).
// - The cache is *derived* state (ADR 0006). On any per-file failure
//   we surface the error to the caller — `trigger_backup` logs and
//   proceeds so a partial L2 archive never aborts the rest of the
//   backup.
//
// Restore is the symmetric mirror: download both keys back into
// `l2_path` (and its `.blob-cache.ctl` sibling). The cold-start
// synopsis rebuild in `BlobCache::new` then re-indexes the metadata
// B+ tree (per `cache/blob.rs::rebuild_l2_synopsis`).

const L2_BACKUP_PAGER_SUFFIX: &str = "l2.pager";
const L2_BACKUP_CONTROL_SUFFIX: &str = "l2.ctl";
const L2_CONTROL_EXTENSION: &str = "blob-cache.ctl";

fn normalize_prefix(prefix: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

fn control_sidecar_for(l2_path: &std::path::Path) -> std::path::PathBuf {
    l2_path.with_extension(L2_CONTROL_EXTENSION)
}

/// Archive the L2 pager file + control sidecar to `backend` under
/// `{prefix}l2.pager` and `{prefix}l2.ctl`. Returns the number of
/// files uploaded (0..=2).
///
/// Caller (`trigger_backup`) decides what to do on error — the cache is
/// derived state so a partial upload is logged, not fatal.
pub fn archive_blob_cache_l2(
    backend: &dyn crate::storage::backend::RemoteBackend,
    l2_path: &std::path::Path,
    prefix: &str,
) -> Result<usize, crate::storage::backend::BackendError> {
    let prefix = normalize_prefix(prefix);
    let mut count = 0usize;
    if l2_path.is_file() {
        backend.upload(l2_path, &format!("{prefix}{L2_BACKUP_PAGER_SUFFIX}"))?;
        count += 1;
    }
    let control = control_sidecar_for(l2_path);
    if control.is_file() {
        backend.upload(&control, &format!("{prefix}{L2_BACKUP_CONTROL_SUFFIX}"))?;
        count += 1;
    }
    Ok(count)
}

/// Restore the L2 pager file + control sidecar from `backend`'s
/// `{prefix}l2.pager` and `{prefix}l2.ctl` keys into `l2_path` (and its
/// `<l2_path>.blob-cache.ctl` sibling). Returns the number of files
/// downloaded.
///
/// Cold-start synopsis rebuild on next `BlobCache::new` re-indexes the
/// metadata. Surfaced for the documented restore procedure
/// (`docs/operations/blob-cache-backup-restore.md` §3); not yet wired
/// into a programmatic restore endpoint.
pub fn restore_blob_cache_l2(
    backend: &dyn crate::storage::backend::RemoteBackend,
    prefix: &str,
    l2_path: &std::path::Path,
) -> Result<usize, crate::storage::backend::BackendError> {
    let prefix = normalize_prefix(prefix);
    if let Some(parent) = l2_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| crate::storage::backend::BackendError::Transport(err.to_string()))?;
        }
    }
    let mut count = 0usize;
    if backend.download(&format!("{prefix}{L2_BACKUP_PAGER_SUFFIX}"), l2_path)? {
        count += 1;
    }
    let control = control_sidecar_for(l2_path);
    if backend.download(&format!("{prefix}{L2_BACKUP_CONTROL_SUFFIX}"), &control)? {
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod backup_helpers_tests {
    use super::*;
    use crate::storage::backend::LocalBackend;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn write_file(path: &std::path::Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Per-test unique scratch root under the system temp dir.
    /// The `tempfile` crate is not in dev-deps; we synthesize a
    /// collision-free path using the test name + a process-local
    /// monotonic counter, mirroring the convention already used by
    /// `cache/blob.rs::tests::l2_path`.
    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);
    fn scratch(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("reddb-blobcache-bk-{label}-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// `LocalBackend` interprets keys as on-disk paths verbatim; we
    /// therefore use absolute paths under tempdirs as the "prefix"
    /// the test backends see. A real S3/HTTP backend would ignore the
    /// path prefix and treat the keys as opaque strings — both
    /// directions of the helper round-trip the same way.
    #[test]
    fn archive_then_restore_round_trips_l2_pager_and_control_files() {
        let scratch_dir = scratch("pair-src");
        let l2_src = scratch_dir.join("cache.rdb");
        write_file(&l2_src, b"pager-bytes-on-disk");
        write_file(&control_sidecar_for(&l2_src), b"control-sidecar-bytes");

        let backend_root = scratch("pair-be");
        let prefix = format!("{}/blob_cache/", backend_root.display());

        let uploaded = archive_blob_cache_l2(&LocalBackend, &l2_src, &prefix)
            .expect("archive succeeds");
        assert_eq!(uploaded, 2, "pager + control sidecar uploaded");

        let dst_dir = scratch("pair-dst");
        let l2_dst = dst_dir.join("cache.rdb");
        let downloaded = restore_blob_cache_l2(&LocalBackend, &prefix, &l2_dst)
            .expect("restore succeeds");
        assert_eq!(downloaded, 2);

        assert_eq!(std::fs::read(&l2_dst).unwrap(), b"pager-bytes-on-disk");
        assert_eq!(
            std::fs::read(control_sidecar_for(&l2_dst)).unwrap(),
            b"control-sidecar-bytes"
        );

        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_dir_all(&backend_root);
        let _ = std::fs::remove_dir_all(&dst_dir);
    }

    #[test]
    fn archive_missing_l2_path_is_noop() {
        let backend_root = scratch("be-missing");
        let prefix = format!("{}/blob_cache/", backend_root.display());
        let count = archive_blob_cache_l2(
            &LocalBackend,
            std::path::Path::new("/nonexistent/path/for/reddb-test.rdb"),
            &prefix,
        )
        .expect("missing path treated as nothing to archive");
        assert_eq!(count, 0);
        let _ = std::fs::remove_dir_all(&backend_root);
    }

    #[test]
    fn restore_with_no_objects_creates_empty_parent_dir() {
        let backend_root = scratch("be-empty");
        let prefix = format!("{}/blob_cache/", backend_root.display());
        let dst_dir = scratch("dst-empty");
        let l2_dst = dst_dir.join("cache.rdb");
        let count = restore_blob_cache_l2(&LocalBackend, &prefix, &l2_dst)
            .expect("empty restore is ok");
        assert_eq!(count, 0);
        let _ = std::fs::remove_dir_all(&backend_root);
        let _ = std::fs::remove_dir_all(&dst_dir);
    }

    /// End-to-end round-trip through a real `BlobCache`:
    /// 1. Build a cache with an L2 path, put two entries (so blob bytes
    ///    plus metadata land on disk).
    /// 2. Drop the cache (closes the L2 metadata B+ tree).
    /// 3. Archive the L2 directory to the backend.
    /// 4. Restore into a fresh L2 path.
    /// 5. Open a new `BlobCache` against the restored path and verify
    ///    `get` returns the original bytes — proves the cold-start
    ///    synopsis rebuild (`blob.rs::rebuild_l2_synopsis`) re-indexes
    ///    the restored tree end-to-end.
    ///
    /// This is the integration-test that ADR 0006 §"backup-restore"
    /// commits to and that the lane plan calls out as the
    /// `include_blob_cache=true` round-trip success criterion.
    #[test]
    fn full_round_trip_via_blob_cache_preserves_entries_after_restore() {
        use crate::storage::cache::blob::{BlobCache, BlobCacheConfig, BlobCachePut};

        let src_dir = scratch("rt-src");
        let dst_dir = scratch("rt-dst");
        let backend_root = scratch("rt-be");
        let l2_src = src_dir.join("blob-cache.rdb");
        let l2_dst = dst_dir.join("blob-cache.rdb");
        let prefix = format!("{}/blob_cache/", backend_root.display());

        // 1+2: put entries + drop the cache so L2 fsyncs.
        {
            let cache = BlobCache::new(
                BlobCacheConfig::default()
                    .with_l1_bytes_max(64 * 1024)
                    .with_shard_count(2)
                    .with_max_namespaces(8)
                    .with_l2_path(&l2_src),
            );
            cache
                .put("ns-a", "k1", BlobCachePut::new(b"value-1".to_vec()))
                .expect("put k1");
            cache
                .put("ns-b", "k2", BlobCachePut::new(b"value-2-longer-payload".to_vec()))
                .expect("put k2");
            // L2 path accessor must see the configured file path.
            assert_eq!(cache.l2_path(), Some(l2_src.as_path()));
        } // drop cache

        // 3: archive L2 (pager file + control sidecar).
        let uploaded = archive_blob_cache_l2(&LocalBackend, &l2_src, &prefix)
            .expect("archive l2");
        assert_eq!(uploaded, 2, "pager + control uploaded");

        // 4: restore into fresh path.
        let restored = restore_blob_cache_l2(&LocalBackend, &prefix, &l2_dst)
            .expect("restore l2");
        assert_eq!(restored, 2, "pager + control downloaded");

        // 5: re-open against restored path and verify entries are
        //    addressable. The synopsis rebuild fires automatically in
        //    BlobCache::new (per blob.rs::rebuild_l2_synopsis at
        //    blob.rs:1069-1085).
        let restored_cache = BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(64 * 1024)
                .with_shard_count(2)
                .with_max_namespaces(8)
                .with_l2_path(&l2_dst),
        );
        let hit_a = restored_cache.get("ns-a", "k1").expect("k1 survives restore");
        assert_eq!(hit_a.value(), b"value-1");
        let hit_b = restored_cache.get("ns-b", "k2").expect("k2 survives restore");
        assert_eq!(hit_b.value(), b"value-2-longer-payload");

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&dst_dir);
        let _ = std::fs::remove_dir_all(&backend_root);
    }

    /// Inverse contract: with `include_blob_cache` left at its default
    /// (false) — i.e., we simply skip the archive step — restoring into
    /// a fresh path leaves the cache cold. This is the documented
    /// default behaviour (`docs/operations/blob-cache-backup-restore.md`
    /// §1: "Consequences for restore: the cache starts empty").
    #[test]
    fn skipped_archive_leaves_restored_cache_cold() {
        use crate::storage::cache::blob::{BlobCache, BlobCacheConfig, BlobCachePut};

        let src_dir = scratch("cold-src");
        let dst_dir = scratch("cold-dst");
        let l2_src = src_dir.join("blob-cache.rdb");
        let l2_dst = dst_dir.join("blob-cache.rdb");

        {
            let cache = BlobCache::new(
                BlobCacheConfig::default().with_l2_path(&l2_src),
            );
            cache
                .put("ns", "k", BlobCachePut::new(b"value".to_vec()))
                .expect("put k");
        }

        // Note: NO archive step. The fresh L2 path stays empty, mirroring
        // the default backup posture (include_blob_cache=false).
        let cold_cache = BlobCache::new(
            BlobCacheConfig::default().with_l2_path(&l2_dst),
        );
        assert!(
            cold_cache.get("ns", "k").is_none(),
            "restore without include_blob_cache must yield a cold cache"
        );

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&dst_dir);
    }
}
