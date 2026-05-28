//! Regression for issue #704 (PRD #662 slice G).
//!
//! The bulk-rebuild path in `UnifiedStore::persist` historically
//! filtered out any row whose serialised value exceeded the legacy
//! per-value cap — silently dropping data on every checkpoint. After
//! slice E this is no longer necessary: rebuild must route every
//! value through the slice-E write ladder so legacy oversized rows
//! land in overflow chains instead of disappearing.
//!
//! This test seeds a row whose value comfortably exceeds the inline
//! overflow threshold (1024 bytes), checkpoints, closes the runtime,
//! reopens against the same file and asserts the row survives
//! byte-identical through the rebuild path.

use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn issue_704_rebuild_preserves_value_above_overflow_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rebuild_spill.rdb");

    // Build a payload safely above the 1024-byte inline overflow
    // threshold so the value must travel through the spill ladder
    // (either inline-compressed or an overflow chain) when rebuild
    // re-inserts it into the fresh B-tree.
    let pattern: Vec<u8> = (0..256).map(|i| i as u8).collect();
    let mut payload: Vec<u8> = Vec::with_capacity(8 * 1024);
    while payload.len() < 8 * 1024 {
        payload.extend_from_slice(&pattern);
    }
    // Hex-encode so the value is a valid SQL TEXT literal. The hex
    // text is 2x the bytes (≈16 KiB) — still well above the inline
    // threshold and unambiguously legacy-skip territory.
    let hex_value: String = payload.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        hex_value.len() > 1024,
        "payload must exceed the inline overflow threshold to exercise spill"
    );

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&db_path))
            .expect("runtime boots persistent");
        rt.execute_query("CREATE TABLE big_rows (tag TEXT, blob TEXT)")
            .expect("create big_rows table");
        rt.execute_query(&format!(
            "INSERT INTO big_rows (tag, blob) VALUES ('legacy', '{hex_value}')"
        ))
        .expect("insert oversized row");

        // Force the rebuild path explicitly. Before slice G this is
        // exactly where the row would have been dropped.
        rt.checkpoint().expect("checkpoint succeeds");
        drop(rt);
    }

    let rt2 = RedDBRuntime::with_options(RedDBOptions::persistent(&db_path))
        .expect("runtime reopens persistent");

    let result = rt2
        .execute_query("SELECT tag, blob FROM big_rows WHERE tag = 'legacy'")
        .expect("select after reopen");

    assert_eq!(
        result.result.records.len(),
        1,
        "oversized row must survive rebuild instead of being dropped"
    );
    let row = &result.result.records[0];
    let blob_value = row.get("blob").expect("blob cell present in returned row");
    let blob_str = blob_value.to_string();
    // The runtime sometimes quotes string scalars in its Display form;
    // strip surrounding single quotes if present before comparing.
    let trimmed = blob_str
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(blob_str.as_str());
    assert_eq!(
        trimmed, hex_value,
        "rebuilt blob must round-trip byte-identical through the spill ladder"
    );
}
