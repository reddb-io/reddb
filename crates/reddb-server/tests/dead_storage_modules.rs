use std::path::Path;

#[test]
fn unreachable_storage_modules_stay_retired() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("src/storage");

    for retired in [
        "btree/README.md",
        "btree/cursor.rs",
        "btree/gc.rs",
        "btree/mod.rs",
        "btree/node.rs",
        "btree/prefetch.rs",
        "btree/tree.rs",
        "btree/version.rs",
        "btree/visibility_map.rs",
        "cache/README.md",
        "cache/ring.rs",
        "cache/sieve.rs",
        "cache/strategy.rs",
        "engine/binary_quantize.rs",
        "engine/bulk_writer.rs",
        "engine/int8_quantize.rs",
        "engine/pq.rs",
        "engine/store_strategy.rs",
        "engine/tiered_search.rs",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired module remains: {}",
            retired
        );
    }

    for live in [
        "engine/page_cache.rs",
        "engine/prefetch.rs",
        "unified/visibility_map.rs",
    ] {
        assert!(
            root.join(live).is_file(),
            "live storage leaf is missing: {}",
            live
        );
    }

    assert!(
        !manifest.join("benches/cache_ring_contention_bench.rs").exists(),
        "benchmark for the retired cache ring remains"
    );
}
