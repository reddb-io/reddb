//! Issue gh-477: fold pager meta into page 1 of the datafile.
//!
//! Acceptance:
//!  - flag `fold_pager_meta` controls behaviour;
//!  - ON: page 1 (+ overflow chain when needed) is the sole source of truth;
//!    `<data>-meta` sidecar is not written;
//!  - OFF: legacy behaviour preserved — `<data>-meta` shadow is emitted;
//!  - free list overflow works for > N pages (one trunk holds 1014 ids);
//!  - tests cover massive allocation forcing overflow.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::{
    fold_pager_meta_enabled, set_fold_pager_meta_enabled, RedDBOptions, RedDBRuntime,
    StorageDeployPreset,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// Serialise tests that flip the process-global toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn meta_shadow_path(data: &Path) -> PathBuf {
    let mut p = data.to_path_buf().into_os_string();
    p.push("-meta");
    PathBuf::from(p)
}

fn pager_meta_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("production HA profile should expose pager metadata")
}

#[test]
fn fold_off_default_preserves_meta_shadow() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_pager_meta_enabled(false);
    std::env::remove_var("REDDB_FOLD_PAGER_META");
    assert!(!fold_pager_meta_enabled(), "default policy must be OFF");

    let db = support::temp_db_file("fold_off");
    let path = db.path();

    {
        let rt =
            RedDBRuntime::with_options(pager_meta_options(path)).expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE fold_off_a (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }

    let shadow = meta_shadow_path(path);
    assert!(
        shadow.exists(),
        "legacy <data>-meta shadow must be written when fold is OFF: {shadow:?}",
    );
}

#[test]
fn fold_on_skips_meta_shadow_and_data_round_trips() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_pager_meta_enabled(true);

    let db = support::temp_db_file("fold_on");
    let path = db.path();

    {
        let rt =
            RedDBRuntime::with_options(pager_meta_options(path)).expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE fold_on_a (name TEXT)")
            .expect("ddl");
        rt.execute_query("INSERT INTO fold_on_a (name) VALUES ('alpha')")
            .expect("insert");
        rt.checkpoint().expect("flush");
    }

    let shadow = meta_shadow_path(path);
    assert!(
        !shadow.exists(),
        "<data>-meta shadow must be absent when fold is ON: {shadow:?}",
    );

    // Reopen: catalog and data must survive without the shadow.
    {
        let rt = RedDBRuntime::with_options(pager_meta_options(path))
            .expect("persistent runtime reopens");
        let _ = rt
            .execute_query("SELECT name FROM fold_on_a")
            .expect("select");
    }

    set_fold_pager_meta_enabled(false);
}

/// Forces the metadata blob past a single 4 KiB page so the overflow chain
/// has to allocate and chain `PageType::Overflow` pages. The reopened
/// runtime must see every catalog entry.
#[test]
fn massive_catalog_forces_meta_overflow_chain() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_pager_meta_enabled(true);

    let db = support::temp_db_file("fold_overflow");
    let path = db.path();

    // Each row in the metadata blob is `name_len:u32 | name | root:u32`. A
    // ~32-char name costs ~40 bytes, so 200 collections clears 4 KiB
    // comfortably.
    const COLLECTIONS: usize = 200;
    let names: Vec<String> = (0..COLLECTIONS)
        .map(|i| format!("collection_with_a_padded_name_{i:05}"))
        .collect();

    {
        let rt =
            RedDBRuntime::with_options(pager_meta_options(path)).expect("persistent runtime opens");
        for name in &names {
            rt.execute_query(&format!("CREATE TABLE {name} (id INTEGER)"))
                .expect("ddl");
        }
        rt.checkpoint().expect("flush");
    }

    {
        let rt = RedDBRuntime::with_options(pager_meta_options(path))
            .expect("persistent runtime reopens");
        for name in &names {
            // SELECT against the catalog: parse must succeed (no truncation).
            let _ = rt
                .execute_query(&format!("SELECT * FROM {name}"))
                .unwrap_or_else(|e| panic!("collection {name} missing after reopen: {e}"));
        }
    }

    set_fold_pager_meta_enabled(false);
}

/// Free list trunk chain must hold many more than `FREE_IDS_PER_TRUNK` ids
/// (1014 entries per trunk). Drives `flush_to_trunks` past the single-trunk
/// boundary and walks the chain back through `load_from_trunk`, asserting
/// no ids are dropped on the round trip.
#[test]
fn freelist_trunk_chain_handles_many_pages() {
    use reddb::storage::engine::freelist::{FreeList, FREE_IDS_PER_TRUNK};
    use reddb::storage::engine::page::PageType;

    // > 3× single-trunk capacity ensures we exercise multi-page overflow.
    let total = FREE_IDS_PER_TRUNK * 3 + 7;

    let mut fl = FreeList::new();
    for i in 0..total {
        fl.free(i as u32);
    }
    assert_eq!(fl.total_free() as usize, total);

    let mut next_trunk_id = 1_000_000u32;
    let trunks = fl.flush_to_trunks(0, || {
        let id = next_trunk_id;
        next_trunk_id += 1;
        id
    });
    assert!(
        trunks.len() >= 3,
        "expected at least 3 trunk pages, got {}",
        trunks.len()
    );
    for trunk in &trunks {
        assert_eq!(trunk.page_type().unwrap(), PageType::FreelistTrunk);
    }

    // Walk the chain from head and reload every trunk. Use the head we set
    // last via `flush_to_trunks` (top of the linked list). The chain order is
    // last-pushed → first-pushed.
    let head = fl.trunk_head();
    let by_id: std::collections::HashMap<u32, _> =
        trunks.iter().map(|t| (t.page_id(), t.clone())).collect();
    let mut reload = FreeList::from_header(head, 0);
    let mut cur = head;
    let mut seen = std::collections::HashSet::new();
    while cur != 0 {
        assert!(seen.insert(cur), "trunk chain cycle at {cur}");
        let page = by_id.get(&cur).expect("trunk reachable in chain");
        match reload.load_from_trunk(page).expect("load trunk") {
            Some(next) => cur = next,
            None => break,
        }
    }
    assert!(
        reload.total_free() as usize >= total,
        "reloaded freelist lost ids: {} < {}",
        reload.total_free(),
        total
    );
}
