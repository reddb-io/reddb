//! Opt-in qualification over an independently preserved historical copy. Never
//! selected by the normal suite or the managed runtime's storage factory.
use super::*;
use crate::json::Value as JsonValue;
use std::time::Instant;

fn fingerprint(store: &UnifiedStore, expected: &JsonValue) -> u64 {
    let collections = expected["collections"]
        .as_array()
        .expect("reference collections");
    let mut actual_names = store.list_collections();
    actual_names.sort();
    let mut expected_names: Vec<_> = collections
        .iter()
        .map(|item| item["name"].as_str().expect("collection name").to_string())
        .collect();
    expected_names.sort();
    assert_eq!(
        actual_names, expected_names,
        "all physical collections survive"
    );
    let mut total = 0;
    for reference in collections {
        let name = reference["name"].as_str().expect("collection");
        let manager = store.get_collection(name).expect("physical manager");
        let mut count = 0u64;
        let mut sum = [0u8; 32];
        let mut xor = [0u8; 32];
        manager
            .try_scan_for_each(None, 16 * 1024 * 1024, |entity| {
                // This is the actual public native entity DTO used to produce
                // the independently exported reference. Its object maps are
                // ordered, and its integer/decimal lexemes stay lossless.
                let json =
                    crate::presentation::entity_json::entity_json(entity).to_string_compact();
                let digest = crate::crypto::sha256(json.as_bytes());
                let mut carry = 0u16;
                for index in (0..32).rev() {
                    let value = u16::from(sum[index]) + u16::from(digest[index]) + carry;
                    sum[index] = value.to_le_bytes()[0];
                    carry = value >> 8;
                    xor[index] ^= digest[index];
                }
                count += 1;
                true
            })
            .expect("every physical read succeeds; no truncated prefix");
        assert_eq!(count, reference["count"].as_u64().expect("count"), "{name}");
        assert_eq!(
            hex::encode(sum),
            reference["sha256Sum"].as_str().expect("sum"),
            "{name}"
        );
        assert_eq!(
            hex::encode(xor),
            reference["sha256Xor"].as_str().expect("xor"),
            "{name}"
        );
        total += count;
    }
    assert_eq!(total, expected["rows"].as_u64().expect("reference total"));
    total
}

#[test]
#[ignore = "requires an owned historical copy, its independent native fingerprint and an external cgroup limit"]
fn disk_resident_historical_copy_preserves_every_exported_entity() {
    let root = std::env::var_os("REDDB_QUALIFICATION_COPY_DIR").expect("owned copy directory");
    let root = std::fs::canonicalize(root).expect("existing copy");
    let marker = std::fs::read_to_string(root.join(".reddb-owned-qualification-copy"))
        .expect("explicit ownership marker is required before opening mutable native files");
    assert_eq!(marker.trim(), "disk-residency-historical-qualification-v1");
    let path = root.join("reddb.rdb");
    assert!(path.is_file(), "never create an empty substitute database");
    let reference = std::env::var_os("REDDB_QUALIFICATION_REFERENCE").expect("reference path");
    let reference = std::fs::read_to_string(reference).expect("independently exported reference");
    let expected: JsonValue = crate::json::from_str(&reference).expect("reference JSON");
    assert_eq!(expected["complete"].as_bool(), Some(true));
    let config = UnifiedStoreConfig {
        // 128 MiB of pager slots and 256 MiB of resident overlays. The external
        // 1-GiB limit measures all native metadata, scratch and allocator RSS too.
        page_cache_slots: Some(8192),
        resident_memory_bytes_max: Some(256 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    };
    for phase in ["initial", "prepared"] {
        let started = Instant::now();
        let store =
            UnifiedStore::open_with_config(&path, config.clone()).expect("cold native open");
        println!(
            "qualification phase={phase} open_ms={}",
            started.elapsed().as_millis()
        );
        let rows = fingerprint(&store, &expected);
        assert!(store.segment_memory_bytes() <= 256 * 1024 * 1024);
        let checkpoint = Instant::now();
        store.persist().expect("durable generation publication");
        println!(
            "qualification phase={phase} verified_entities={rows} resident_segment_bytes={} checkpoint_ms={}",
            store.segment_memory_bytes(),
            checkpoint.elapsed().as_millis()
        );
    }
}
