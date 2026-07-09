use super::super::{
    METRIC_CACHE_BLOB_L1_BYTES_IN_USE, METRIC_CACHE_BLOB_L2_BYTES_IN_USE,
    METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL, METRIC_CACHE_BLOB_SYNOPSIS_BYTES,
    METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL, METRIC_CACHE_VERSION_MISMATCH_TOTAL,
};
use super::*;

fn small_cache(bytes: usize) -> BlobCache {
    BlobCache::new(
        BlobCacheConfig::default()
            .with_l1_bytes_max(bytes)
            .with_shard_count(1)
            .with_max_namespaces(4),
    )
}

fn l2_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "reddb-blob-cache-{name}-{}-{nanos}.rdb",
        std::process::id()
    ))
}

fn l2_cache(path: &Path) -> BlobCache {
    BlobCache::open_with_l2(
        BlobCacheConfig::default()
            .with_l1_bytes_max(128)
            .with_shard_count(1)
            .with_max_namespaces(4)
            .with_l2_path(path),
    )
    .expect("l2_cache test helper")
}

fn remove_l2(path: &Path) {
    // Remove the L2 `.rdb` AND every sidecar so nothing is left for the leak
    // guard (scripts/check-temp-residue.sh). The blob-cache L2 file format is
    // owned by reddb-file, so derive each sidecar path through its authoritative
    // helpers rather than re-declaring suffix literals here (enforced by
    // reddb-file's `server_does_not_redeclare_blob_cache_l2_file_format`).
    let control = reddb_file::blob_cache_control_path(path);
    for sidecar in [
        path.to_path_buf(),
        reddb_file::layout::pager_dwb_path(path),
        reddb_file::blob_cache_double_write_path(path),
        reddb_file::blob_cache_control_temp_path(&control),
        control,
    ] {
        let _ = std::fs::remove_file(sidecar);
    }
}

#[test]
fn put_get_and_exists_round_trip_blob() {
    let cache = small_cache(128);
    cache
        .put("images", "hero", BlobCachePut::new(vec![1, 2, 3]))
        .expect("put");

    assert_eq!(cache.exists("images", "hero"), CachePresence::Present);
    let hit = cache.get("images", "hero").expect("hit");
    assert_eq!(&*hit.bytes, &[1, 2, 3]);

    let stats = cache.stats();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 0);
    assert_eq!(stats.insertions, 1);
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.bytes_in_use, 3);
    assert_eq!(stats.l1_bytes_max, 128);
}

#[test]
fn missing_key_updates_miss_counter() {
    let cache = small_cache(128);
    assert!(cache.get("images", "missing").is_none());
    assert_eq!(cache.exists("images", "missing"), CachePresence::Absent);
    let stats = cache.stats();
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
}

#[test]
fn namespace_isolation_keeps_same_key_separate() {
    let cache = small_cache(128);
    cache
        .put("a", "same", BlobCachePut::new(b"a".to_vec()))
        .unwrap();
    cache
        .put("b", "same", BlobCachePut::new(b"b".to_vec()))
        .unwrap();

    assert_eq!(&*cache.get("a", "same").unwrap().bytes, b"a");
    assert_eq!(&*cache.get("b", "same").unwrap().bytes, b"b");
    assert_eq!(cache.stats().namespaces, 2);
}

#[test]
fn byte_capacity_evicts_with_sieve() {
    let cache = small_cache(6);
    cache
        .put("n", "a", BlobCachePut::new(vec![1, 1, 1]))
        .unwrap();
    cache
        .put("n", "b", BlobCachePut::new(vec![2, 2, 2]))
        .unwrap();
    let _ = cache.get("n", "a");
    cache
        .put("n", "c", BlobCachePut::new(vec![3, 3, 3]))
        .unwrap();

    assert!(cache.get("n", "c").is_some(), "new entry remains cached");
    let stats = cache.stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.bytes_in_use, 6);
    assert!(stats.evictions >= 1);
}

#[test]
fn namespace_cap_rejects_new_namespace() {
    let cache = BlobCache::new(
        BlobCacheConfig::default()
            .with_l1_bytes_max(128)
            .with_shard_count(1)
            .with_max_namespaces(1),
    );
    cache.put("a", "k", BlobCachePut::new(vec![1])).unwrap();
    let err = cache
        .put("b", "k", BlobCachePut::new(vec![1]))
        .expect_err("second namespace rejected");
    assert_eq!(err, CacheError::TooManyNamespaces { max: 1 });
}

#[test]
fn content_metadata_round_trips_and_is_capped() {
    let cache = BlobCache::new(
        BlobCacheConfig::default()
            .with_l1_bytes_max(128)
            .with_shard_count(1)
            .with_content_metadata_limits(2, 64),
    );
    let metadata = BTreeMap::from([
        ("content-type".to_string(), "text/plain".to_string()),
        ("etag".to_string(), "v1".to_string()),
    ]);
    cache
        .put(
            "http",
            "home",
            BlobCachePut::new(b"ok".to_vec()).with_content_metadata(metadata.clone()),
        )
        .unwrap();
    assert_eq!(
        cache.get("http", "home").unwrap().content_metadata,
        metadata
    );

    let too_many = BTreeMap::from([
        ("a".to_string(), "1".to_string()),
        ("b".to_string(), "2".to_string()),
        ("c".to_string(), "3".to_string()),
    ]);
    let err = cache
        .put(
            "http",
            "too_many",
            BlobCachePut::new(b"ok".to_vec()).with_content_metadata(too_many),
        )
        .expect_err("too many metadata keys");
    assert!(matches!(err, CacheError::MetadataTooLarge { .. }));

    let too_large = BTreeMap::from([("long".to_string(), "x".repeat(64))]);
    let err = cache
        .put(
            "http",
            "too_large",
            BlobCachePut::new(b"ok".to_vec()).with_content_metadata(too_large),
        )
        .expect_err("metadata bytes too large");
    assert!(matches!(err, CacheError::MetadataTooLarge { .. }));
}

#[test]
fn blob_larger_than_l1_budget_is_rejected() {
    let cache = small_cache(4);
    let err = cache
        .put("n", "large", BlobCachePut::new(vec![0; 5]))
        .expect_err("blob too large");
    assert_eq!(err, CacheError::BlobTooLarge { size: 5, max: 4 });
}

#[test]
fn hard_ttl_expires_entries_on_get_and_exists() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().ttl_ms(10);
    cache
        .put_at(
            "n",
            "ttl",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();

    assert!(cache.get_at("n", "ttl", 1_009).is_some());
    assert!(cache.get_at("n", "ttl", 1_010).is_none());
    assert_eq!(cache.exists_at("n", "ttl", 1_011), CachePresence::Absent);

    let stats = cache.stats();
    assert_eq!(stats.expirations, 1);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.entries, 0);
    assert_eq!(stats.bytes_in_use, 0);
}

#[test]
fn absolute_expiry_is_hard_boundary() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().expires_at_unix_ms(500);
    cache
        .put_at(
            "n",
            "abs",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            100,
        )
        .unwrap();

    assert!(cache.get_at("n", "abs", 499).is_some());
    assert!(cache.get_at("n", "abs", 500).is_none());
    assert_eq!(cache.stats().expirations, 1);
}

#[test]
fn ttl_and_absolute_expiry_use_earliest_deadline() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default()
        .ttl_ms(100)
        .expires_at_unix_ms(1_050);
    cache
        .put_at(
            "n",
            "earliest",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();

    assert!(cache.get_at("n", "earliest", 1_049).is_some());
    assert!(cache.get_at("n", "earliest", 1_050).is_none());
}

#[test]
fn per_entry_max_blob_bytes_rejects_large_blob() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().max_blob_bytes(2);
    let err = cache
        .put(
            "n",
            "large",
            BlobCachePut::new(vec![1, 2, 3]).with_policy(policy),
        )
        .expect_err("per-entry cap rejects blob");

    assert_eq!(err, CacheError::BlobTooLarge { size: 3, max: 2 });
    assert_eq!(cache.stats().insertions, 0);
}

#[test]
fn l1_admission_never_accepts_put_without_storing_l1_entry() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().l1_admission(L1Admission::Never);
    cache
        .put(
            "n",
            "skip",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
        )
        .unwrap();

    assert!(cache.get("n", "skip").is_none());
    let stats = cache.stats();
    assert_eq!(stats.insertions, 1);
    assert_eq!(stats.entries, 0);
    assert_eq!(stats.bytes_in_use, 0);
}

#[test]
fn l1_admission_always_and_auto_store_entries() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "always",
            BlobCachePut::new(b"a".to_vec())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Always)),
        )
        .unwrap();
    cache
        .put(
            "n",
            "auto",
            BlobCachePut::new(b"b".to_vec())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Auto)),
        )
        .unwrap();

    assert_eq!(&*cache.get("n", "always").unwrap().bytes, b"a");
    assert_eq!(&*cache.get("n", "auto").unwrap().bytes, b"b");
}

#[test]
fn priority_biases_sieve_eviction_toward_lower_priority_entries() {
    let cache = small_cache(6);
    cache
        .put(
            "n",
            "low",
            BlobCachePut::new(vec![1, 1, 1]).with_policy(BlobCachePolicy::default().priority(1)),
        )
        .unwrap();
    cache
        .put(
            "n",
            "high",
            BlobCachePut::new(vec![2, 2, 2]).with_policy(BlobCachePolicy::default().priority(250)),
        )
        .unwrap();
    cache
        .put("n", "new", BlobCachePut::new(vec![3, 3, 3]))
        .unwrap();

    assert!(cache.get("n", "high").is_some());
    assert!(cache.get("n", "low").is_none());
    let stats = cache.stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.bytes_in_use, 6);
    assert!(stats.evictions >= 1);
}

#[test]
fn cas_version_must_increase_to_mutate_existing_entry() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"v1".to_vec()).with_policy(BlobCachePolicy::default().version(1)),
        )
        .unwrap();
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"v2".to_vec()).with_policy(BlobCachePolicy::default().version(2)),
        )
        .unwrap();

    let hit = cache.get("n", "cas").unwrap();
    assert_eq!(&*hit.bytes, b"v2");
    assert_eq!(hit.version, Some(2));
}

#[test]
fn cas_equal_or_lower_version_rejects_without_mutating_or_counting_insert() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"v2".to_vec()).with_policy(BlobCachePolicy::default().version(2)),
        )
        .unwrap();

    let equal = cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"equal".to_vec()).with_policy(BlobCachePolicy::default().version(2)),
        )
        .expect_err("equal version rejected");
    assert_eq!(
        equal,
        CacheError::VersionMismatch {
            existing: 2,
            attempted: 2,
        }
    );

    let lower = cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"lower".to_vec()).with_policy(BlobCachePolicy::default().version(1)),
        )
        .expect_err("lower version rejected");
    assert_eq!(
        lower,
        CacheError::VersionMismatch {
            existing: 2,
            attempted: 1,
        }
    );

    let hit = cache.get("n", "cas").unwrap();
    assert_eq!(&*hit.bytes, b"v2");
    assert_eq!(hit.version, Some(2));
    let stats = cache.stats();
    assert_eq!(stats.insertions, 1);
    assert_eq!(stats.version_mismatches, 2);
}

#[test]
fn cas_missing_key_with_version_succeeds() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "missing",
            BlobCachePut::new(b"v7".to_vec()).with_policy(BlobCachePolicy::default().version(7)),
        )
        .unwrap();

    let hit = cache.get("n", "missing").unwrap();
    assert_eq!(&*hit.bytes, b"v7");
    assert_eq!(hit.version, Some(7));
}

#[test]
fn put_without_version_overwrites_unconditionally() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"v9".to_vec()).with_policy(BlobCachePolicy::default().version(9)),
        )
        .unwrap();
    cache
        .put("n", "cas", BlobCachePut::new(b"plain".to_vec()))
        .unwrap();

    let hit = cache.get("n", "cas").unwrap();
    assert_eq!(&*hit.bytes, b"plain");
    assert_eq!(hit.version, None);
}

#[test]
fn invalidate_key_removes_one_entry_and_is_idempotent() {
    let cache = small_cache(128);
    cache
        .put("n", "a", BlobCachePut::new(b"a".to_vec()))
        .unwrap();
    cache
        .put("n", "b", BlobCachePut::new(b"b".to_vec()))
        .unwrap();

    assert_eq!(cache.invalidate_key("n", "a"), 1);
    assert_eq!(cache.invalidate_key("n", "a"), 0);
    assert!(cache.get("n", "a").is_none());
    assert_eq!(&*cache.get("n", "b").unwrap().bytes, b"b");

    let stats = cache.stats();
    assert_eq!(stats.invalidations, 1);
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.bytes_in_use, 1);
}

#[test]
fn invalidate_prefix_removes_matching_namespace_keys_only() {
    let cache = small_cache(128);
    cache
        .put("n", "user:1", BlobCachePut::new(b"1".to_vec()))
        .unwrap();
    cache
        .put("n", "user:2", BlobCachePut::new(b"2".to_vec()))
        .unwrap();
    cache
        .put("n", "post:1", BlobCachePut::new(b"3".to_vec()))
        .unwrap();
    cache
        .put("other", "user:1", BlobCachePut::new(b"4".to_vec()))
        .unwrap();

    assert_eq!(cache.invalidate_prefix("n", "user:"), 2);
    assert!(cache.get("n", "user:1").is_none());
    assert!(cache.get("n", "user:2").is_none());
    assert!(cache.get("n", "post:1").is_some());
    assert!(cache.get("other", "user:1").is_some());
    assert_eq!(cache.stats().invalidations, 2);
}

#[test]
fn invalidate_tag_and_dependency_use_indexes() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "tagged",
            BlobCachePut::new(b"a".to_vec()).with_tags(["hot", "tenant:1"]),
        )
        .unwrap();
    cache
        .put(
            "n",
            "dependent",
            BlobCachePut::new(b"b".to_vec()).with_dependencies(["row:42"]),
        )
        .unwrap();
    cache
        .put("n", "plain", BlobCachePut::new(b"c".to_vec()))
        .unwrap();

    assert_eq!(cache.invalidate_tags("n", &["hot"]), 1);
    assert!(cache.get("n", "tagged").is_none());
    assert_eq!(cache.invalidate_dependencies("n", &["row:42"]), 1);
    assert!(cache.get("n", "dependent").is_none());
    assert!(cache.get("n", "plain").is_some());
    assert_eq!(cache.stats().invalidations, 2);
}

#[test]
fn cold_invalidation_returns_without_stats_changes_when_no_namespace_or_label_can_match() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "tagged",
            BlobCachePut::new(b"a".to_vec()).with_tags(["warm"]),
        )
        .unwrap();
    let before = cache.stats();

    assert_eq!(cache.invalidate_prefix("missing", "x"), 0);
    assert_eq!(cache.invalidate_tags("n", &["cold"]), 0);
    assert_eq!(cache.invalidate_dependencies("n", &["row:missing"]), 0);
    assert_eq!(cache.stats(), before);
}

#[test]
fn namespace_flush_bumps_generation_and_old_entries_are_immediately_absent() {
    let cache = small_cache(128);
    cache
        .put("n", "a", BlobCachePut::new(b"a".to_vec()))
        .unwrap();
    cache
        .put("n", "b", BlobCachePut::new(b"b".to_vec()))
        .unwrap();
    assert_eq!(cache.stats().entries, 2);

    assert!(cache.invalidate_namespace("n"));
    let after_flush = cache.stats();
    assert_eq!(after_flush.namespace_flushes, 1);
    assert_eq!(after_flush.entries, 2, "foreground path does not sweep");

    assert!(cache.get("n", "a").is_none());
    assert_eq!(cache.exists("n", "b"), CachePresence::Absent);
    cache
        .put("n", "c", BlobCachePut::new(b"c".to_vec()))
        .unwrap();
    assert_eq!(&*cache.get("n", "c").unwrap().bytes, b"c");
}

#[test]
fn namespace_flush_makes_prior_versions_irrelevant_for_subsequent_put() {
    let cache = small_cache(128);
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"old".to_vec()).with_policy(BlobCachePolicy::default().version(9)),
        )
        .unwrap();

    assert!(cache.invalidate_namespace("n"));
    cache
        .put(
            "n",
            "cas",
            BlobCachePut::new(b"new".to_vec()).with_policy(BlobCachePolicy::default().version(1)),
        )
        .unwrap();

    let hit = cache.get("n", "cas").unwrap();
    assert_eq!(&*hit.bytes, b"new");
    assert_eq!(hit.version, Some(1));
    assert_eq!(cache.stats().version_mismatches, 0);
}

#[test]
fn invalidation_is_node_local_for_mvp() {
    let primary = small_cache(128);
    let replica = small_cache(128);
    primary
        .put("n", "k", BlobCachePut::new(b"primary".to_vec()))
        .unwrap();
    replica
        .put("n", "k", BlobCachePut::new(b"replica".to_vec()))
        .unwrap();

    assert_eq!(primary.invalidate_key("n", "k"), 1);
    assert!(primary.get("n", "k").is_none());
    assert_eq!(&*replica.get("n", "k").unwrap().bytes, b"replica");
}

#[test]
fn l2_rehydrates_after_reopen_without_json_rows() {
    let path = l2_path("reopen");
    {
        let cache = l2_cache(&path);
        cache
            .put(
                "n",
                "k",
                BlobCachePut::new(b"durable".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();
        assert!(cache.get("n", "k").is_some());
    }
    {
        let cache = l2_cache(&path);
        let hit = cache.get("n", "k").expect("rehydrates from L2");
        assert_eq!(&*hit.bytes, b"durable");
        assert_eq!(cache.stats().l2_bytes_in_use, 7);
    }
    remove_l2(&path);
}

#[test]
fn l2_expired_entry_does_not_rehydrate_on_reopen() {
    let path = l2_path("expired");
    {
        let cache = l2_cache(&path);
        cache
            .put_at(
                "n",
                "ttl",
                BlobCachePut::new(b"old".to_vec())
                    .with_policy(BlobCachePolicy::default().ttl_ms(10)),
                1_000,
            )
            .unwrap();
    }
    {
        let cache = l2_cache(&path);
        assert!(cache.get_at("n", "ttl", 1_010).is_none());
        assert_eq!(cache.stats().l2_bytes_in_use, 0);
    }
    remove_l2(&path);
}

#[test]
fn l2_invalidated_entry_does_not_resurrect_after_reopen() {
    let path = l2_path("invalidated");
    {
        let cache = l2_cache(&path);
        cache
            .put("n", "k", BlobCachePut::new(b"gone".to_vec()))
            .unwrap();
        assert_eq!(cache.invalidate_key("n", "k"), 1);
    }
    {
        let cache = l2_cache(&path);
        assert!(cache.get("n", "k").is_none());
    }
    remove_l2(&path);
}

#[test]
fn l2_rejects_put_when_hard_byte_cap_is_exceeded() {
    let path = l2_path("full");
    let cache = BlobCache::open_with_l2(
        BlobCacheConfig::default()
            .with_l1_bytes_max(128)
            .with_shard_count(1)
            .with_l2_bytes_max(2)
            .with_l2_path(&path),
    )
    .expect("open l2");
    let err = cache
        .put("n", "large", BlobCachePut::new(vec![1, 2, 3]))
        .expect_err("L2 cap rejects");
    assert_eq!(err, CacheError::L2Full { size: 3, max: 2 });
    assert_eq!(cache.stats().l2_full_rejections, 1);
    drop(cache);
    remove_l2(&path);
}

#[test]
fn l2_metadata_last_hides_partial_blob_after_fault() {
    let path = l2_path("fault");
    {
        let cache = l2_cache(&path);
        cache.inject_l2_fault_after_blob_write_once();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache
                .put("n", "partial", BlobCachePut::new(b"partial".to_vec()))
                .unwrap();
        }));
        assert!(result.is_err(), "fault hook should panic mid-write");
    }
    {
        let cache = l2_cache(&path);
        assert!(cache.get("n", "partial").is_none());
        assert_eq!(cache.stats().l2_bytes_in_use, 0);
    }
    remove_l2(&path);
}

#[test]
fn l2_synopsis_negative_skip_avoids_metadata_read() {
    let path = l2_path("synopsis-negative");
    let cache = l2_cache(&path);

    assert!(cache.get("n", "missing").is_none());
    let stats = cache.stats();
    assert_eq!(stats.l2_negative_skips, 1);
    assert_eq!(stats.l2_metadata_reads, 0);

    drop(cache);
    remove_l2(&path);
}

#[test]
fn l2_synopsis_maybe_present_verifies_authoritative_metadata() {
    let path = l2_path("synopsis-maybe");
    let cache = l2_cache(&path);
    cache.inject_l2_synopsis_maybe_present("n", "ghost");

    assert!(cache.get("n", "ghost").is_none());
    let stats = cache.stats();
    assert_eq!(stats.l2_negative_skips, 0);
    assert_eq!(stats.l2_metadata_reads, 1);

    drop(cache);
    remove_l2(&path);
}

#[test]
fn stale_synopsis_bits_after_delete_cannot_produce_present() {
    let path = l2_path("synopsis-delete");
    let cache = l2_cache(&path);
    cache
        .put(
            "n",
            "deleted",
            BlobCachePut::new(b"gone".to_vec())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .unwrap();
    assert_eq!(cache.invalidate_key("n", "deleted"), 1);

    // Bloom filter cannot clear bits, so the key still hashes positive
    // — but `exists` must surface that ambiguity as `MaybePresent`, not a
    // false `Present`. The authoritative `get` then returns None and
    // bumps the synopsis-false-positive counter.
    assert_eq!(cache.exists("n", "deleted"), CachePresence::MaybePresent);
    assert!(cache.get("n", "deleted").is_none());
    let stats = cache.stats();
    assert_eq!(stats.l2_metadata_reads, 1);
    assert_eq!(stats.synopsis_metadata_reads, 1);

    drop(cache);
    remove_l2(&path);
}

#[test]
fn stale_synopsis_bits_after_expiry_cannot_produce_present() {
    let path = l2_path("synopsis-expiry");
    let cache = l2_cache(&path);
    cache
        .put_at(
            "n",
            "expired",
            BlobCachePut::new(b"old".to_vec()).with_policy(
                BlobCachePolicy::default()
                    .ttl_ms(10)
                    .l1_admission(L1Admission::Never),
            ),
            1_000,
        )
        .unwrap();

    // Filter still says maybe (bits cannot be cleared), so `exists`
    // returns MaybePresent. The authoritative `get` walks the metadata,
    // observes the expiry, and returns None.
    assert_eq!(
        cache.exists_at("n", "expired", 1_010),
        CachePresence::MaybePresent
    );
    assert!(cache.get_at("n", "expired", 1_010).is_none());
    let stats = cache.stats();
    assert_eq!(stats.l2_metadata_reads, 1);
    assert_eq!(stats.l2_bytes_in_use, 0);

    drop(cache);
    remove_l2(&path);
}

#[test]
fn l2_synopsis_rebuilds_from_metadata_on_reopen() {
    let path = l2_path("synopsis-rebuild");
    {
        let cache = l2_cache(&path);
        cache
            .put(
                "n",
                "known",
                BlobCachePut::new(b"known".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();
    }
    {
        let cache = l2_cache(&path);
        assert_eq!(&*cache.get("n", "known").unwrap().bytes, b"known");
        let stats = cache.stats();
        assert_eq!(stats.l2_negative_skips, 0);
        assert_eq!(stats.l2_metadata_reads, 1);
    }
    remove_l2(&path);
}

#[test]
fn deleted_l2_entries_never_return_present_under_repeated_stale_synopsis() {
    let path = l2_path("synopsis-deleted-many");
    let cache = l2_cache(&path);
    for i in 0..1_000 {
        let key = format!("k{i}");
        cache
            .put(
                "n",
                &key,
                BlobCachePut::new(vec![1])
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();
        assert_eq!(cache.invalidate_key("n", &key), 1);
        // After delete the Bloom filter still has stale bits — exists
        // can answer MaybePresent or Absent depending on whether the
        // hash collides with a still-live key. The strict invariant is
        // that `get` (the authoritative path) NEVER returns Some for a
        // deleted key.
        assert!(matches!(
            cache.exists("n", &key),
            CachePresence::MaybePresent | CachePresence::Absent
        ));
        assert!(cache.get("n", &key).is_none());
    }
    // Each `get` of a deleted key with positive Bloom bits walks the
    // metadata and finds nothing; that's the false-positive cost. Filter
    // sizing (10K capacity / 1% FPR) means most lookups hit fast.
    assert_eq!(cache.stats().l2_metadata_reads, 1_000);
    drop(cache);
    remove_l2(&path);
}

#[test]
fn metric_name_is_stable_for_observability_adapter() {
    assert_eq!(
        METRIC_CACHE_BLOB_L1_BYTES_IN_USE,
        "cache_blob_l1_bytes_in_use"
    );
    assert_eq!(
        METRIC_CACHE_VERSION_MISMATCH_TOTAL,
        "cache_version_mismatch_total"
    );
    assert_eq!(
        METRIC_CACHE_BLOB_L2_BYTES_IN_USE,
        "reddb_cache_blob_l2_bytes_in_use"
    );
    assert_eq!(
        METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL,
        "reddb_cache_blob_l2_full_rejections_total"
    );
    assert_eq!(
        METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL,
        "cache_blob_synopsis_metadata_reads_total"
    );
    assert_eq!(
        METRIC_CACHE_BLOB_SYNOPSIS_BYTES,
        "cache_blob_synopsis_bytes"
    );
}

// -- API review #151 follow-ups -----------------------------------------

#[test]
fn cache_presence_from_bool_round_trips_present_and_absent() {
    assert_eq!(CachePresence::from(true), CachePresence::Present);
    assert_eq!(CachePresence::from(false), CachePresence::Absent);
    // The `MaybePresent` variant is emitted by the L2 Bloom synopsis
    // (#146); the `From<bool>` adapter still maps the binary case
    // exactly so callers that have a definitive answer can lift it
    // without going through the synopsis.
    let _ = CachePresence::MaybePresent;
}

#[test]
fn exists_returns_present_or_absent_today() {
    let cache = small_cache(128);
    cache
        .put("n", "k", BlobCachePut::new(b"v".to_vec()))
        .unwrap();

    assert_eq!(cache.exists("n", "k"), CachePresence::Present);
    assert_eq!(cache.exists("n", "missing"), CachePresence::Absent);
    assert_eq!(cache.exists("missing", "k"), CachePresence::Absent);
}

#[test]
fn invalidate_tags_batched_call_removes_keys_from_multiple_labels() {
    let cache = small_cache(256);
    cache
        .put(
            "n",
            "a",
            BlobCachePut::new(b"a".to_vec()).with_tags(["red"]),
        )
        .unwrap();
    cache
        .put(
            "n",
            "b",
            BlobCachePut::new(b"b".to_vec()).with_tags(["green"]),
        )
        .unwrap();
    cache
        .put(
            "n",
            "c",
            BlobCachePut::new(b"c".to_vec()).with_tags(["blue"]),
        )
        .unwrap();

    // One batched call removes the two named tags but leaves "blue".
    assert_eq!(cache.invalidate_tags("n", &["red", "green"]), 2);
    assert!(cache.get("n", "a").is_none());
    assert!(cache.get("n", "b").is_none());
    assert!(cache.get("n", "c").is_some());
    assert_eq!(cache.stats().invalidations(), 2);
}

#[test]
fn invalidate_dependencies_batched_call_dedups_multi_label_keys() {
    let cache = small_cache(256);
    cache
        .put(
            "n",
            "shared",
            BlobCachePut::new(b"x".to_vec()).with_dependencies(["row:1", "row:2"]),
        )
        .unwrap();

    // The same key matches both dependencies; the batched form must
    // count it once, not twice.
    assert_eq!(cache.invalidate_dependencies("n", &["row:1", "row:2"]), 1);
    assert!(cache.get("n", "shared").is_none());
}

#[test]
fn invalidate_tags_with_empty_slice_is_a_no_op() {
    let cache = small_cache(128);
    cache
        .put("n", "a", BlobCachePut::new(b"a".to_vec()).with_tags(["x"]))
        .unwrap();
    assert_eq!(cache.invalidate_tags("n", &[]), 0);
    assert_eq!(cache.invalidate_dependencies("n", &[]), 0);
    assert!(cache.get("n", "a").is_some());
}

#[test]
fn blob_cache_config_builder_constructs_cache_end_to_end() {
    let config = BlobCacheConfig::builder()
        .l1_bytes_max(64)
        .max_namespaces(2)
        .shard_count(1)
        .build();
    assert_eq!(config.l1_bytes_max(), 64);
    assert_eq!(config.max_namespaces(), 2);
    assert_eq!(config.shard_count(), 1);

    let cache = BlobCache::new(config);
    cache
        .put("n", "k", BlobCachePut::new(b"v".to_vec()))
        .unwrap();
    assert_eq!(cache.exists("n", "k"), CachePresence::Present);
}

#[test]
fn blob_cache_stats_getters_match_internal_field_state() {
    let cache = small_cache(128);
    cache
        .put("n", "k", BlobCachePut::new(b"abc".to_vec()))
        .unwrap();
    let _ = cache.get("n", "k");
    let _ = cache.get("n", "missing");

    let stats = cache.stats();
    // Each getter must mirror the internal field that backs it.
    assert_eq!(stats.hits(), stats.hits);
    assert_eq!(stats.misses(), stats.misses);
    assert_eq!(stats.insertions(), stats.insertions);
    assert_eq!(stats.evictions(), stats.evictions);
    assert_eq!(stats.expirations(), stats.expirations);
    assert_eq!(stats.invalidations(), stats.invalidations);
    assert_eq!(stats.namespace_flushes(), stats.namespace_flushes);
    assert_eq!(stats.version_mismatches(), stats.version_mismatches);
    assert_eq!(stats.entries(), stats.entries);
    assert_eq!(stats.bytes_in_use(), stats.bytes_in_use as u64);
    assert_eq!(stats.l1_bytes_max(), stats.l1_bytes_max);
    assert_eq!(stats.l2_bytes_in_use(), stats.l2_bytes_in_use);
    assert_eq!(stats.l2_bytes_max(), stats.l2_bytes_max);
    assert_eq!(stats.l2_full_rejections(), stats.l2_full_rejections);
    assert_eq!(stats.l2_metadata_reads(), stats.l2_metadata_reads);
    assert_eq!(stats.l2_negative_skips(), stats.l2_negative_skips);
    assert_eq!(
        stats.synopsis_metadata_reads(),
        stats.synopsis_metadata_reads
    );
    assert_eq!(stats.synopsis_bytes(), stats.synopsis_bytes);
    assert_eq!(stats.namespaces(), stats.namespaces);
    assert_eq!(stats.max_namespaces(), stats.max_namespaces);
    assert_eq!(stats.promotion_queued(), stats.promotion_queued);
    assert_eq!(stats.promotion_dropped(), stats.promotion_dropped);
    assert_eq!(stats.promotion_completed(), stats.promotion_completed);
    assert_eq!(stats.promotion_queue_depth(), stats.promotion_queue_depth);
}

#[test]
fn blob_cache_hit_getters_expose_payload_and_metadata() {
    let cache = small_cache(128);
    let metadata = BTreeMap::from([("ct".to_string(), "t".to_string())]);
    cache
        .put(
            "n",
            "k",
            BlobCachePut::new(b"hello".to_vec())
                .with_content_metadata(metadata.clone())
                .with_policy(BlobCachePolicy::default().version(7)),
        )
        .unwrap();
    let hit = cache.get("n", "k").expect("hit");
    assert_eq!(hit.value(), b"hello");
    assert_eq!(&**hit.bytes(), b"hello");
    assert_eq!(hit.content_metadata(), &metadata);
    assert_eq!(hit.version(), Some(7));
}

#[test]
fn blob_cache_policy_setter_then_getter_round_trips() {
    let policy = BlobCachePolicy::default()
        .ttl_ms(60)
        .expires_at_unix_ms(1_000)
        .max_blob_bytes(512)
        .l1_admission(L1Admission::Always)
        .priority(7)
        .version(42);
    assert_eq!(policy.ttl_ms_value(), Some(60));
    assert_eq!(policy.expires_at_unix_ms_value(), Some(1_000));
    assert_eq!(policy.max_blob_bytes_value(), Some(512));
    assert_eq!(policy.l1_admission_value(), L1Admission::Always);
    assert_eq!(policy.priority_value(), 7);
    assert_eq!(policy.version_value(), Some(42));
}

#[test]
fn blob_cache_is_send_and_sync_across_thread_boundary() {
    // Belt and braces alongside the file-level `assert_send_sync` const:
    // actually exercise the contract by sharing an `Arc<BlobCache>` with
    // a worker thread.
    use std::thread;
    let cache = Arc::new(small_cache(128));
    cache
        .put("n", "k", BlobCachePut::new(b"v".to_vec()))
        .unwrap();
    let worker = {
        let cache = Arc::clone(&cache);
        thread::spawn(move || {
            assert_eq!(cache.exists("n", "k"), CachePresence::Present);
            cache.get("n", "k").map(|hit| hit.value().to_vec())
        })
    };
    let observed = worker.join().expect("worker thread");
    assert_eq!(observed.as_deref(), Some(b"v".as_slice()));
}

// -- Async promotion wiring (issue #193, lane 1/5) ----------------------

fn cleanup_l2(path: &Path) {
    // Remove the L2 `.rdb` and every sidecar via the authoritative reddb-file
    // path helpers (see `remove_l2`). Callers must drop the cache BEFORE calling
    // this so the pager's drop-time flush cannot re-create a sidecar after
    // removal.
    remove_l2(path);
}

/// Slow executor that sleeps `delay` then increments a counter.
/// Used to make the hot-path / worker-path latency split observable
/// without relying on real L2 read time.
fn slow_executor(
    delay: std::time::Duration,
    counter: Arc<std::sync::atomic::AtomicUsize>,
) -> PromotionExecutor {
    Arc::new(move |_req| {
        std::thread::sleep(delay);
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
}

/// Hot-path latency on an L2-hit drops to near-zero when async
/// promotion is enabled — the slow executor only blocks the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn l2_hit_with_async_on_returns_immediately() {
    let path = l2_path("async-on");
    let cache = Arc::new(l2_cache(&path));
    // Seed L2: put then evict from L1 by namespace flush so next get
    // misses L1 and re-reads L2.
    cache
        .put("ns", "k", BlobCachePut::new(b"hello".to_vec()))
        .expect("put");
    // Force L1 eviction of "k" by overflowing the byte cap with fillers.
    for i in 0..40 {
        cache
            .put(
                "ns",
                &format!("filler{i}"),
                BlobCachePut::new(vec![0u8; 16]),
            )
            .expect("filler");
    }

    let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    cache.enable_async_promotion_with_executor(
        PoolOpts {
            queue_capacity: 16,
            worker_count: 1,
        },
        slow_executor(std::time::Duration::from_millis(50), Arc::clone(&executed)),
    );

    let start = std::time::Instant::now();
    let hit = cache.get("ns", "k").expect("L2 hit");
    let elapsed = start.elapsed();
    assert_eq!(&*hit.bytes, b"hello");
    eprintln!("async-on hot-path latency: {elapsed:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(20),
        "hot path should not block on slow executor; elapsed={elapsed:?}"
    );

    // Wait for the worker to drain so cleanup is sound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while executed.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(executed.load(Ordering::Relaxed) >= 1, "worker did not run");

    cache.shutdown_async_promotion();
    drop(cache);
    cleanup_l2(&path);
}

/// Same slow executor, but with async OFF (default). The hot path
/// pays the full sync promotion cost — but with no executor in the
/// loop it should still be fast. Sanity: opt-in didn't break legacy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn l2_hit_with_async_off_uses_legacy_sync_path() {
    let path = l2_path("async-off");
    let cache = Arc::new(l2_cache(&path));
    cache
        .put("ns", "k", BlobCachePut::new(b"hello".to_vec()))
        .expect("put");
    for i in 0..40 {
        cache
            .put(
                "ns",
                &format!("filler{i}"),
                BlobCachePut::new(vec![0u8; 16]),
            )
            .expect("filler");
    }
    // Async NOT enabled — pool is None.
    assert!(cache.promotion_pool_handle().is_none());

    let start = std::time::Instant::now();
    let hit = cache.get("ns", "k").expect("L2 hit");
    let elapsed = start.elapsed();
    eprintln!("async-off (legacy sync) hot-path latency: {elapsed:?}");
    assert_eq!(&*hit.bytes, b"hello");
    // Stats show zero promotion activity in legacy mode.
    let s = cache.stats();
    assert_eq!(s.promotion_queued(), 0);
    assert_eq!(s.promotion_completed(), 0);
    assert_eq!(s.promotion_dropped(), 0);
    assert_eq!(s.promotion_queue_depth(), 0);

    drop(cache);
    cleanup_l2(&path);
}

/// Saturating the promotion queue does not corrupt `get`'s response —
/// the L2 read still happens on the hot path, so callers always see
/// the correct bytes even when the pool drops the promotion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_on_saturation_never_loses_correctness() {
    let path = l2_path("async-saturate");
    let cache = Arc::new(l2_cache(&path));
    // Seed many distinct keys in L2.
    for i in 0..32 {
        cache
            .put("ns", &format!("k{i}"), BlobCachePut::new(vec![i as u8; 4]))
            .expect("put");
    }
    // Evict L1.
    for i in 0..40 {
        cache
            .put(
                "ns",
                &format!("filler{i}"),
                BlobCachePut::new(vec![0u8; 16]),
            )
            .expect("filler");
    }
    // Tiny queue, sleep-forever-ish executor — first request blocks
    // the worker, queue saturates almost instantly.
    let blocked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    cache.enable_async_promotion_with_executor(
        PoolOpts {
            queue_capacity: 1,
            worker_count: 1,
        },
        slow_executor(std::time::Duration::from_millis(500), Arc::clone(&blocked)),
    );

    // Hammer the cache; bytes must always come back unchanged.
    for i in 0..32 {
        let hit = cache.get("ns", &format!("k{i}")).expect("L2 hit");
        assert_eq!(&*hit.bytes, &vec![i as u8; 4][..]);
    }

    let s = cache.stats();
    assert!(
        s.promotion_dropped() > 0,
        "expected at least one drop under saturation; got {s:?}"
    );

    cache.shutdown_async_promotion();
    drop(cache);
    cleanup_l2(&path);
}

/// Shutdown drains queued requests within a bounded budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_pool_within_budget() {
    let path = l2_path("async-shutdown");
    let cache = Arc::new(l2_cache(&path));
    for i in 0..20 {
        cache
            .put("ns", &format!("k{i}"), BlobCachePut::new(vec![i as u8; 4]))
            .expect("put");
    }
    for i in 0..40 {
        cache
            .put(
                "ns",
                &format!("filler{i}"),
                BlobCachePut::new(vec![0u8; 16]),
            )
            .expect("filler");
    }
    let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    cache.enable_async_promotion_with_executor(
        PoolOpts {
            queue_capacity: 64,
            worker_count: 2,
        },
        slow_executor(std::time::Duration::from_millis(1), Arc::clone(&executed)),
    );

    let mut scheduled = 0u64;
    for i in 0..20 {
        let _ = cache.get("ns", &format!("k{i}"));
        // Each L2 hit schedules at most one promotion.
        scheduled += 1;
    }
    cache.shutdown_async_promotion();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let s = cache.stats();
        if s.promotion_completed() + s.promotion_dropped() >= scheduled {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("shutdown did not drain: {:?}", cache.stats());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    drop(cache);
    cleanup_l2(&path);
}

/// The executor closure holds only a `Weak<BlobCache>` — dropping the
/// `Arc<BlobCache>` releases the cache even while the pool is alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_arc_cycle_executor_holds_only_weak_reference() {
    let path = l2_path("async-noarccycle");
    let cache = Arc::new(l2_cache(&path));
    let pool = cache.enable_async_promotion(PoolOpts {
        queue_capacity: 4,
        worker_count: 1,
    });

    // Construct the canary weak BEFORE we drop the strong arc.
    let canary: Weak<BlobCache> = Arc::downgrade(&cache);
    assert!(canary.upgrade().is_some());

    // Drop the user-held strong arc. The pool itself may still hold
    // refs to its own internal queue/executor, but the executor
    // closure was built on a `Weak<BlobCache>`, so the cache should
    // be deallocatable.
    drop(cache);

    // Pool is still alive (its workers are running), but the cache
    // is gone — the canary cannot be upgraded.
    assert!(
        canary.upgrade().is_none(),
        "BlobCache leaked: executor closure still holds a strong reference"
    );

    // Cleanup: tell the pool to stop. We can't call shutdown via the
    // cache (it's dropped) but we have the pool handle.
    Arc::clone(&pool).shutdown();
    cleanup_l2(&path);
}

// -- L2 compression wiring (#192 lane 2/5) -----------------------------

/// Build a 4 KB payload of repetitive Lorem text — guaranteed to
/// compress well under the default zstd settings.
fn lorem_4kb() -> Vec<u8> {
    let unit = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                     Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
                     nisi ut aliquip ex ea commodo consequat. ";
    let mut out = Vec::with_capacity(4096 + unit.len());
    while out.len() < 4096 {
        out.extend_from_slice(unit);
    }
    out.truncate(4096);
    out
}

/// Linear congruential generator — deterministic high-entropy bytes
/// without pulling in `rand`.
fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

fn l2_cache_with_compression(path: &Path, mode: L2Compression) -> BlobCache {
    // L1 is sized large enough to admit the blob (`validate_blob_size`
    // checks against `l1_bytes_max`), but every put uses
    // `L1Admission::Never` so the L2 path is what we exercise.
    BlobCache::open_with_l2(
        BlobCacheConfig::default()
            .with_l1_bytes_max(64 * 1024)
            .with_shard_count(1)
            .with_max_namespaces(4)
            .with_l2_path(path)
            .with_l2_compression(mode),
    )
    .expect("l2_cache_with_compression test helper")
}

#[test]
fn l2_round_trip_compresses_text_payload_and_returns_original_bytes() {
    let path = l2_path("compression-text");
    let cache = l2_cache_with_compression(&path, L2Compression::On);
    let payload = lorem_4kb();

    cache
        .put(
            "n",
            "doc",
            BlobCachePut::new(payload.clone())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .expect("put");

    // Round-trip through L2: bytes returned must match the original.
    let hit = cache.get("n", "doc").expect("L2 hit");
    assert_eq!(&*hit.bytes, &payload[..]);

    // L2 budget accounting must reflect the *compressed* size, well
    // below the original 4 KB.
    let stats = cache.stats();
    assert!(
        stats.l2_bytes_in_use < payload.len() as u64,
        "expected stored bytes < {}, got {}",
        payload.len(),
        stats.l2_bytes_in_use
    );
    assert_eq!(stats.l2_compression_skipped_total(), 0);
    assert!(stats.l2_compression_ratio_observed() > 1.0);
    assert!(stats.l2_bytes_saved_total() > 0);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_round_trip_with_compression_off_stores_raw_bytes() {
    let path = l2_path("compression-off");
    let cache = l2_cache_with_compression(&path, L2Compression::Off);
    let payload = lorem_4kb();

    cache
        .put(
            "n",
            "doc",
            BlobCachePut::new(payload.clone())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .expect("put");

    let hit = cache.get("n", "doc").expect("L2 hit");
    assert_eq!(&*hit.bytes, &payload[..]);

    let stats = cache.stats();
    // `Off` skips the compress call entirely → entry counted as
    // skipped, stored bytes equal original size.
    assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
    assert_eq!(stats.l2_compression_skipped_total(), 1);
    assert_eq!(stats.l2_bytes_saved_total(), 0);
    assert_eq!(stats.l2_compression_ratio_observed(), 1.0);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_round_trip_with_image_content_type_stores_raw() {
    let path = l2_path("compression-image-ct");
    let cache = l2_cache_with_compression(&path, L2Compression::On);
    // 4 KB of zero bytes would otherwise compress superbly; the
    // content-type rule must short-circuit that.
    let payload = vec![0u8; 4096];
    let metadata = BTreeMap::from([("content-type".to_string(), "image/png".to_string())]);

    cache
        .put(
            "n",
            "img",
            BlobCachePut::new(payload.clone())
                .with_content_metadata(metadata)
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .expect("put");

    let hit = cache.get("n", "img").expect("L2 hit");
    assert_eq!(&*hit.bytes, &payload[..]);

    let stats = cache.stats();
    assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
    assert_eq!(stats.l2_compression_skipped_total(), 1);
    assert_eq!(stats.l2_bytes_saved_total(), 0);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_round_trip_with_high_entropy_payload_falls_back_to_raw_via_ratio_gate() {
    let path = l2_path("compression-entropy");
    let cache = l2_cache_with_compression(&path, L2Compression::On);
    // 8 KB of LCG output — zstd cannot meaningfully shrink it, so
    // the `max_ratio` gate fires and the entry stores raw.
    let payload = pseudo_random(0xCAFE_F00D, 8 * 1024);

    cache
        .put(
            "n",
            "noise",
            BlobCachePut::new(payload.clone())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .expect("put");

    let hit = cache.get("n", "noise").expect("L2 hit");
    assert_eq!(&*hit.bytes, &payload[..]);

    let stats = cache.stats();
    assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
    assert_eq!(stats.l2_compression_skipped_total(), 1);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_forward_compat_reads_legacy_v1_entry_written_before_compression() {
    let path = l2_path("compression-v1-compat");
    let cache = l2_cache_with_compression(&path, L2Compression::On);
    // Synthesise a legacy entry on disk: raw bytes, no v2 framing.
    let payload = b"legacy-payload-pre-issue-192".to_vec();
    cache
        .inject_l2_v1_entry("n", "legacy", &payload)
        .expect("inject v1");

    // Subsequent `get` must dispatch on the record's `format_version`
    // and return the raw bytes verbatim — no decompress, no framing.
    let hit = cache.get("n", "legacy").expect("L2 hit");
    assert_eq!(&*hit.bytes, &payload[..]);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_budget_amplifies_when_entries_compress() {
    // Original L2 budget that fits ~10 raw entries of 4 KB (40 KB
    // total). With compression on, the *stored* bytes are far smaller,
    // so all 10 entries must be admitted without `L2Full`.
    let path = l2_path("compression-budget");
    let payload = lorem_4kb();
    let raw_total = (payload.len() * 10) as u64;
    // Pick a budget below `raw_total` but above the expected
    // compressed total. zstd typically shrinks Lorem to <30% so 25%
    // of `raw_total` is a comfortable headroom.
    let budget = raw_total / 4;
    let cache = BlobCache::open_with_l2(
        BlobCacheConfig::default()
            .with_l1_bytes_max(64 * 1024)
            .with_shard_count(1)
            .with_max_namespaces(4)
            .with_l2_bytes_max(budget)
            .with_l2_path(&path)
            .with_l2_compression(L2Compression::On),
    )
    .expect("open l2");

    for i in 0..10 {
        cache
            .put(
                "n",
                &format!("doc{i}"),
                BlobCachePut::new(payload.clone())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put admitted under compressed budget");
    }

    let stats = cache.stats();
    assert_eq!(stats.l2_full_rejections(), 0, "no rejections expected");
    assert!(
        stats.l2_bytes_in_use <= budget,
        "stored bytes {} exceed budget {}",
        stats.l2_bytes_in_use,
        budget
    );
    // Sanity: would have blown past the budget at raw sizing.
    assert!(stats.l2_bytes_in_use < raw_total / 2);

    drop(cache);
    cleanup_l2(&path);
}

#[test]
fn l2_compression_metrics_partition_compressible_and_skipped_entries() {
    let path = l2_path("compression-metrics");
    let cache = l2_cache_with_compression(&path, L2Compression::On);

    // 10 compressible Lorem entries.
    let payload = lorem_4kb();
    for i in 0..10 {
        cache
            .put(
                "n",
                &format!("text{i}"),
                BlobCachePut::new(payload.clone())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");
    }
    // 5 high-entropy entries — these must hit the `max_ratio` gate
    // and land in the `skipped` counter.
    for i in 0..5 {
        let bin = pseudo_random(0x1234_5678 ^ i as u64, 4 * 1024);
        cache
            .put(
                "n",
                &format!("bin{i}"),
                BlobCachePut::new(bin)
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");
    }

    let stats = cache.stats();
    assert_eq!(stats.l2_compression_skipped_total(), 5);
    assert!(
        stats.l2_compression_ratio_observed() > 1.0,
        "compressed entries did not contribute to ratio"
    );
    assert!(stats.l2_bytes_saved_total() > 0);

    drop(cache);
    cleanup_l2(&path);
}

// ----------------------------------------------------------------------
// Extended TTL wiring (issue #194 lane 3/5)
// ----------------------------------------------------------------------

/// Backwards compat — when extended is `off()`, the cache must behave
/// exactly like the legacy hard-TTL path: past hard TTL → None, no
/// stale serve, no idle bookkeeping leaking.
#[test]
fn extended_off_preserves_legacy_hard_ttl_behavior() {
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().ttl_ms(50);
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    // Past hard TTL → None.
    assert!(cache.get_at("n", "k", 1_051).is_none());
    let stats = cache.stats();
    assert_eq!(stats.expirations(), 1);
    assert_eq!(stats.l1_idle_evicts_total(), 0);
    assert_eq!(stats.l1_stale_serves_total(), 0);
}

/// Idle TTL evicts an entry that has not been accessed within
/// `idle_ttl_ms`, even when its hard TTL is far in the future.
#[test]
fn extended_idle_ttl_evicts_dormant_entry() {
    let cache = small_cache(128);
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: Some(100),
        stale_serve_ms: None,
        jitter_pct: 0,
    };
    let policy = BlobCachePolicy::default().ttl_ms(10_000).extended(extended);
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    // 200ms after put, no intervening access → idle window blown.
    assert!(cache.get_at("n", "k", 1_200).is_none());
    let stats = cache.stats();
    assert_eq!(stats.l1_idle_evicts_total(), 1);
    assert_eq!(stats.expirations(), 1);
}

/// Idle TTL must reset on every successful `get`. Two accesses spaced
/// 150ms apart with `idle_ttl_ms = 200ms` keep the entry alive across
/// 250ms of wall clock.
#[test]
fn extended_idle_ttl_resets_on_access() {
    let cache = small_cache(128);
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: Some(200),
        stale_serve_ms: None,
        jitter_pct: 0,
    };
    let policy = BlobCachePolicy::default().ttl_ms(10_000).extended(extended);
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    // First access at +100ms — still within idle window, last_access
    // bumps to 1_100.
    assert!(cache.get_at("n", "k", 1_100).is_some());
    // Second access at +250ms (= 150ms past the previous access):
    // because last_access was reset, idle = 150 ≤ 200 → still Fresh.
    assert!(cache.get_at("n", "k", 1_250).is_some());
    let stats = cache.stats();
    assert_eq!(stats.l1_idle_evicts_total(), 0);
    assert_eq!(stats.hits(), 2);
}

/// SWR window — past hard TTL but inside `stale_serve_ms` returns a
/// `BlobCacheHit` flagged stale with the remaining window.
#[test]
fn extended_stale_serve_returns_stale_hit() {
    let cache = small_cache(128);
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: None,
        stale_serve_ms: Some(100),
        jitter_pct: 0,
    };
    let policy = BlobCachePolicy::default().ttl_ms(50).extended(extended);
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    // hard expires at 1_050, stale window runs to 1_150.
    // get at 1_060 → Stale with ~90ms remaining.
    let hit = cache.get_at("n", "k", 1_060).expect("stale hit");
    assert!(hit.is_stale());
    assert_eq!(hit.stale_window_remaining_ms(), Some(90));
    let stats = cache.stats();
    assert_eq!(stats.l1_stale_serves_total(), 1);
}

/// Past the cumulative `hard + stale_serve_ms` window, the entry is
/// hard-expired regardless of how big the stale window was.
#[test]
fn extended_stale_serve_expires_after_window_closes() {
    let cache = small_cache(128);
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: None,
        stale_serve_ms: Some(100),
        jitter_pct: 0,
    };
    let policy = BlobCachePolicy::default().ttl_ms(50).extended(extended);
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    // get at 1_200 → past 1_150 stale deadline → Expired.
    assert!(cache.get_at("n", "k", 1_200).is_none());
    let stats = cache.stats();
    assert_eq!(stats.l1_stale_serves_total(), 0);
    assert_eq!(stats.expirations(), 1);
}

/// Jitter at insert time spreads `expires_at_unix_ms` deterministically
/// inside `[base_ttl, base_ttl * (1 + pct/100)]` for unique keys.
#[test]
fn extended_jitter_spreads_expires_at_within_bound() {
    let cache = BlobCache::new(
        BlobCacheConfig::default()
            .with_l1_bytes_max(1_024 * 1024)
            .with_shard_count(1)
            .with_max_namespaces(4),
    );
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: None,
        stale_serve_ms: None,
        jitter_pct: 20,
    };
    let policy = BlobCachePolicy::default().ttl_ms(1_000).extended(extended);
    let now = 10_000u64;
    // Probe 1000 entries; each must remain Fresh at +1000ms (the base
    // TTL floor) and Expired by +1200ms (the jittered ceiling, +20%).
    for i in 0..1_000u32 {
        let key = format!("k{i}");
        cache
            .put_at(
                "n",
                &key,
                BlobCachePut::new(vec![i as u8]).with_policy(policy),
                now,
            )
            .expect("put");
        // At now + base_ttl - 1 → must still be Fresh (jitter only
        // ever pushes expiry later, never earlier).
        assert!(
            cache.get_at("n", &key, now + 999).is_some(),
            "entry {key} should be Fresh at base_ttl - 1",
        );
        // At now + base_ttl * (1 + pct/100) + 1 → must be Expired
        // (jitter ceiling crossed).
        assert!(
            cache.get_at("n", &key, now + 1_201).is_none(),
            "entry {key} should be Expired beyond jitter ceiling",
        );
    }
}

/// Jitter must be deterministic — same `(namespace, key, now_ms)`
/// triple must produce the same expires_at across independent caches.
#[test]
fn extended_jitter_is_deterministic_per_triple() {
    let extended = ExtendedTtlPolicy {
        idle_ttl_ms: None,
        stale_serve_ms: None,
        jitter_pct: 50,
    };
    let policy = BlobCachePolicy::default().ttl_ms(1_000).extended(extended);
    let now = 42_000u64;

    let cache_a = small_cache(1_024);
    let cache_b = small_cache(1_024);
    for key in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        cache_a
            .put_at(
                "n",
                key,
                BlobCachePut::new(b"x".to_vec()).with_policy(policy),
                now,
            )
            .unwrap();
        cache_b
            .put_at(
                "n",
                key,
                BlobCachePut::new(b"x".to_vec()).with_policy(policy),
                now,
            )
            .unwrap();
        // The two caches will have computed identical expires_at_unix_ms.
        // Probe the boundary: any time `t` where one cache returns Some
        // and the other returns None proves they diverge.
        for t_offset in [999u64, 1_000, 1_100, 1_250, 1_499, 1_500, 1_501] {
            let a = cache_a.get_at("n", key, now + t_offset).is_some();
            let b = cache_b.get_at("n", key, now + t_offset).is_some();
            assert_eq!(
                a, b,
                "jitter diverged for key={key} t_offset={t_offset}: a={a} b={b}",
            );
        }
    }
}

/// Performance contract — when extended is `off()`,
/// `EffectiveExpiry::compute` must NEVER be called from the hot path.
/// Verified via a process-global counter incremented inside the
/// extended branch of `Shard::get`.
#[test]
fn extended_off_skips_effective_expiry_compute() {
    // Thread-local counter — no cross-test race. Reset to 0 at the
    // start so the absolute value below is the count contributed by
    // this test alone.
    EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.set(0));
    let cache = small_cache(128);
    let policy = BlobCachePolicy::default().ttl_ms(10_000); // extended defaults to off()
    cache
        .put_at(
            "n",
            "k",
            BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            1_000,
        )
        .unwrap();
    for t in [1_001u64, 1_500, 2_000, 5_000, 9_999] {
        let _ = cache.get_at("n", "k", t);
    }
    let calls = EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.get());
    assert_eq!(
        calls, 0,
        "EffectiveExpiry::compute was invoked {calls} times despite extended=off()",
    );
}

// -------------------------------------------------------------------------
// open_with_l2 error-path tests (#220)
// -------------------------------------------------------------------------

#[test]
fn open_with_l2_returns_err_on_corrupt_control_sidecar() {
    let path = l2_path("corrupt-ctl");
    // Write garbage to the control sidecar so L2Control::read returns Err.
    let ctl = reddb_file::blob_cache_control_path(&path);
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    std::fs::write(&ctl, b"not-a-valid-control-file").unwrap();

    let result = BlobCache::open_with_l2(BlobCacheConfig::default().with_l2_path(&path));
    match &result {
        Err(CacheError::L2Io(_)) => {}
        Err(other) => panic!("expected L2Io error, got: {other:?}"),
        Ok(_) => panic!("expected L2Io error, got Ok(BlobCache)"),
    }
    // Process is still alive — test reaches here.
    let _ = std::fs::remove_file(&ctl);
}

#[test]
fn open_with_l2_returns_err_on_readonly_path() {
    // Create the pager file's parent as a file (so opening the pager path
    // as a file underneath it fails with an I/O error).
    let path = l2_path("readonly");
    // Write a regular file at the path so Pager::open gets an I/O error
    // when it tries to create/open the pager file (or the control sidecar
    // can't be created because the path itself is a directory with no write
    // permission — use a read-only directory instead).
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    // Create the pager path as a directory so opening it as a file fails.
    std::fs::create_dir_all(&path).unwrap();

    let result = BlobCache::open_with_l2(BlobCacheConfig::default().with_l2_path(&path));
    assert!(
        result.is_err(),
        "expected Err when l2_path is a directory, got Ok",
    );
    let _ = std::fs::remove_dir_all(&path);
}
