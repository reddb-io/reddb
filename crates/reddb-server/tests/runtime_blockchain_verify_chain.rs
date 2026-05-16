//! Issue #525 — `verify_chain` endpoint + `integrity = broken` state.
//!
//! Exercises:
//!   - intact chain (genesis only, multi-block) verifies ok
//!   - tampered middle block reports the right height
//!   - tampered tip reports tip height
//!   - mismatch sets the integrity flag; new INSERTs surface
//!     `ChainIntegrityBroken`
//!   - admin clear-integrity-flag unblocks INSERTs
//!   - integrity flag is durable through repeat reads (mirrors restart lazy-load)

use reddb_server::runtime::blockchain_kind::{
    is_integrity_broken_persisted, persist_integrity_flag, VerifyChainOutcome,
};
use reddb_server::storage::blockchain::{compute_block_hash, GENESIS_PREV_HASH};
use reddb_server::storage::schema::Value;
use reddb_server::storage::{EntityData, RowData, UnifiedEntity};
use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn hex32(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn insert_block(rt: &RedDBRuntime, name: &str, actor: &str) {
    let tip = rt.chain_tip_for_collection(name).expect("tip");
    let prev = hex32(&tip.hash);
    let height = tip.height + 1;
    let ts = reddb_server::runtime::blockchain_kind::now_ms();
    let stmt = format!(
        "INSERT INTO {name} (actor, prev_hash, block_height, timestamp) \
         VALUES ('{actor}', '{prev}', {height}, {ts})"
    );
    rt.execute_query(&stmt).expect("insert");
}

fn tamper_block_actor(rt: &RedDBRuntime, collection: &str, height: u64, new_actor: &str) {
    let store = rt.db().store();
    let manager = store.get_collection(collection).expect("collection");
    let target = manager
        .query_all(|_| true)
        .into_iter()
        .find(|e| {
            let EntityData::Row(r) = &e.data else { return false };
            let Some(named) = &r.named else { return false };
            matches!(named.get("block_height"), Some(Value::UnsignedInteger(h)) if *h == height)
        })
        .expect("target block exists");
    let mut new_named = match &target.data {
        EntityData::Row(r) => r.named.clone().unwrap_or_default(),
        _ => panic!("not a row"),
    };
    new_named.insert("actor".to_string(), Value::text(new_actor));
    let tampered = UnifiedEntity::new(
        target.id,
        target.kind.clone(),
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(new_named),
            schema: None,
        }),
    );
    manager.update(tampered).expect("update");
}

#[test]
fn genesis_only_chain_verifies_ok() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    let outcome = rt
        .verify_chain_for_collection("audit_log")
        .expect("verify");
    assert_eq!(
        outcome,
        VerifyChainOutcome {
            checked: 1,
            ok: true,
            first_bad_height: None,
        }
    );
}

#[test]
fn intact_multi_block_chain_verifies_ok() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    for actor in ["alice", "bob", "carol"] {
        insert_block(&rt, "audit_log", actor);
    }
    let outcome = rt
        .verify_chain_for_collection("audit_log")
        .expect("verify");
    assert!(outcome.ok, "intact chain must be ok, got {outcome:?}");
    assert_eq!(outcome.checked, 4);
    assert_eq!(outcome.first_bad_height, None);
}

#[test]
fn corrupting_middle_block_reports_its_height() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    for actor in ["alice", "bob", "carol", "dave"] {
        insert_block(&rt, "audit_log", actor);
    }
    tamper_block_actor(&rt, "audit_log", 2, "EVE");
    let outcome = rt
        .verify_chain_for_collection("audit_log")
        .expect("verify");
    assert!(!outcome.ok);
    assert_eq!(outcome.first_bad_height, Some(2));
}

#[test]
fn corrupting_tip_reports_tip_height() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    for actor in ["alice", "bob"] {
        insert_block(&rt, "audit_log", actor);
    }
    tamper_block_actor(&rt, "audit_log", 2, "MALLORY");
    let outcome = rt
        .verify_chain_for_collection("audit_log")
        .expect("verify");
    assert!(!outcome.ok);
    assert_eq!(outcome.first_bad_height, Some(2));
}

#[test]
fn failed_verify_blocks_new_inserts_until_admin_clears_flag() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    insert_block(&rt, "audit_log", "alice");
    insert_block(&rt, "audit_log", "bob");
    tamper_block_actor(&rt, "audit_log", 1, "ZZZ");

    let outcome = rt.verify_chain_for_collection("audit_log").unwrap();
    assert!(!outcome.ok);
    assert_eq!(outcome.first_bad_height, Some(1));

    // Subsequent INSERT must surface ChainIntegrityBroken.
    let err = rt
        .execute_query("INSERT INTO audit_log (actor) VALUES ('charlie')")
        .expect_err("expected lock");
    match err {
        RedDBError::InvalidOperation(msg) => {
            assert!(msg.starts_with("ChainIntegrityBroken:"), "got {msg}");
        }
        other => panic!("expected InvalidOperation, got {other:?}"),
    }

    // Admin clears -> writes resume.
    assert!(rt.clear_chain_integrity_flag("audit_log"));
    rt.execute_query("INSERT INTO audit_log (actor) VALUES ('charlie')")
        .expect("insert after clear");
}

#[test]
fn integrity_flag_persisted_round_trips() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    let store = rt.db().store();
    persist_integrity_flag(&*store, "audit_log", true);
    assert_eq!(is_integrity_broken_persisted(&*store, "audit_log"), Some(true));
    persist_integrity_flag(&*store, "audit_log", false);
    assert_eq!(is_integrity_broken_persisted(&*store, "audit_log"), Some(false));
}

#[test]
fn verify_returns_none_for_non_chain_collection() {
    let rt = rt();
    rt.execute_query("CREATE TABLE plain (id INTEGER)").expect("create plain");
    assert!(rt.verify_chain_for_collection("plain").is_none());
}

#[test]
fn canonical_encoder_matches_engine_genesis_hash() {
    // Alignment regression: collect_blocks rebuilds the canonical payload
    // from the stored `named` map.  If genesis_fields ever diverges from
    // canonical_payload (the #524 deferred bug), verify_chain would report
    // `Inconsistent` at height 0.  This test pins the alignment.
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    let store = rt.db().store();
    let blocks = reddb_server::runtime::blockchain_kind::collect_blocks(&*store, "audit_log")
        .expect("blocks");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].block_height, 0);
    assert_eq!(blocks[0].prev_hash, GENESIS_PREV_HASH);
    let recomputed = compute_block_hash(
        &GENESIS_PREV_HASH,
        0,
        blocks[0].timestamp_ms,
        &blocks[0].payload,
        None,
    );
    assert_eq!(recomputed, blocks[0].hash);
}
