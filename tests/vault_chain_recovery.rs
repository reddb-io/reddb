//! Vault chain corruption / recovery tests.
//!
//! The vault save path is crash-safe by ordering: data pages first,
//! header (commit point) last. These tests don't simulate process
//! crashes literally; instead they construct the post-crash on-disk
//! states the system might land in and verify that `load()` either
//! recovers correctly or fails with a clear `Corrupt` error rather
//! than silently returning bogus state.
//!
//! Coverage:
//!   * Header points to a non-existent next page → clear Corrupt
//!     error, no panic, no decryption with garbage bytes.
//!   * Header chain_count says N but follow-the-pointer hits a sooner
//!     terminator → clear Corrupt error.
//!   * Data page is overwritten with non-vault bytes mid-chain →
//!     clear Corrupt error (magic check).
//!   * Legacy v1 vaults that pre-date the chain refuse to load and
//!     surface operator guidance.

use std::collections::HashMap;

use reddb::auth::vault::{Vault, VaultError, VaultState};
use reddb::auth::{ApiKey, Role, User, UserId};
use reddb::storage::engine::page::{Page, PageType, HEADER_SIZE};
use reddb::storage::engine::pager::{Pager, PagerConfig};

fn scratch_path(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "reddb_vault_chain_{}_{}_{}",
        label,
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("vault.rdb")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Build a state large enough to require a multi-page chain (~50
/// users is well over the header capacity once we add KEY lines).
fn fat_state() -> VaultState {
    let now = now_ms();
    let mut users = Vec::with_capacity(2_000);
    let mut keys = Vec::with_capacity(2_000);
    for i in 0..2_000 {
        let username = format!("u{i:05}");
        users.push(User {
            username: username.clone(),
            tenant_id: None,
            password_hash: format!("argon2id$s{i:08x}$h{i:08x}"),
            scram_verifier: None,
            role: Role::Read,
            api_keys: vec![],
            created_at: now,
            updated_at: now,
            enabled: true,
        });
        keys.push((
            UserId::platform(username),
            ApiKey {
                key: format!("rk_{i:016x}"),
                name: format!("k{i}"),
                role: Role::Write,
                created_at: now,
            },
        ));
    }
    VaultState {
        users,
        api_keys: keys,
        bootstrapped: true,
        master_secret: Some(vec![0x42; 32]),
        kv: HashMap::new(),
    }
}

/// Header page lives at id 2. Patch the `first_data_page_id` field
/// inline. Field offsets mirror the layout described in
/// `src/auth/vault.rs` (must stay in lockstep with that file).
fn corrupt_first_data_pointer(pager: &Pager, new_id: u32) {
    // After the 32-byte page header:
    //   magic(4) version(1) salt(16) payload_len(4) nonce(12)
    //   chain_count(4) first_data_page_id(4)
    // first_data_page_id absolute byte offset within the page:
    let absolute = HEADER_SIZE + 4 + 1 + 16 + 4 + 12 + 4;
    let mut page = pager.read_page_no_checksum(2).unwrap();
    let bytes = page.as_bytes_mut();
    bytes[absolute..absolute + 4].copy_from_slice(&new_id.to_le_bytes());
    pager.write_page_no_checksum(2, page).unwrap();
    pager.flush().unwrap();
}

/// Replace the magic bytes of a data page so the chain walker fails
/// at the right hop. We don't know which id is the first data page
/// without decoding the header, so callers pass it in.
fn corrupt_data_page_magic(pager: &Pager, page_id: u32) {
    let mut page = pager.read_page_no_checksum(page_id).unwrap();
    let bytes = page.as_bytes_mut();
    bytes[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(b"XXXX");
    pager.write_page_no_checksum(page_id, page).unwrap();
    pager.flush().unwrap();
}

/// Read the `first_data_page_id` field from the live header.
fn read_first_data_pointer(pager: &Pager) -> u32 {
    let absolute = HEADER_SIZE + 4 + 1 + 16 + 4 + 12 + 4;
    let page = pager.read_page_no_checksum(2).unwrap();
    let b = page.as_bytes();
    u32::from_le_bytes(b[absolute..absolute + 4].try_into().unwrap())
}

#[test]
fn header_pointer_to_missing_page_returns_clear_error() {
    let db_path = scratch_path("missing-data");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    vault.save(&pager, &fat_state()).unwrap();

    // Verify the saved chain is real before we mangle it.
    let _ = vault.load(&pager).unwrap().expect("baseline load");

    // Point the header at a page id well past page_count. The pager
    // can't fabricate a vault page out of thin air there, so the
    // walker must surface a clear error rather than ploughing on.
    let bogus_id = pager.page_count().unwrap() + 1024;
    corrupt_first_data_pointer(&pager, bogus_id);

    // Force a fresh load that doesn't hit cache.
    drop(vault);
    drop(pager);
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    let err = vault.load(&pager).unwrap_err();
    match err {
        VaultError::Corrupt(_) | VaultError::Pager(_) => {} // both are acceptable
        other => panic!("expected Corrupt/Pager error, got {other:?}"),
    }
}

#[test]
fn data_page_magic_corruption_returns_clear_error() {
    let db_path = scratch_path("bad-magic");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    vault.save(&pager, &fat_state()).unwrap();
    let first_data = read_first_data_pointer(&pager);
    assert!(
        first_data >= 3,
        "fat state should have produced at least one data page"
    );

    // Stomp the first data page's magic. The chain walker should
    // refuse to interpret the bytes.
    corrupt_data_page_magic(&pager, first_data);

    drop(vault);
    drop(pager);
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    let err = vault.load(&pager).unwrap_err();
    match err {
        VaultError::Corrupt(msg) => {
            assert!(
                msg.contains("magic") || msg.contains("data page"),
                "expected magic-related error, got: {msg}"
            );
        }
        other => panic!("expected Corrupt error, got {other:?}"),
    }
}

#[test]
fn premature_terminator_with_outstanding_payload_fails() {
    // chain_count says >0 but first_data_page_id is 0. Make sure the
    // walker doesn't dereference page id 0 (which is the DB header
    // page) and instead produces a clean error.
    let db_path = scratch_path("premature-end");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    vault.save(&pager, &fat_state()).unwrap();
    corrupt_first_data_pointer(&pager, 0);

    drop(vault);
    drop(pager);
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("recovery-pass")).unwrap();

    let err = vault.load(&pager).unwrap_err();
    match err {
        VaultError::Corrupt(msg) => {
            assert!(
                msg.contains("prematurely") || msg.contains("next_id") || msg.contains("chain"),
                "expected chain-end error, got: {msg}"
            );
        }
        other => panic!("expected Corrupt error, got {other:?}"),
    }
}

#[test]
fn legacy_v1_vault_refuses_to_load() {
    // Hand-craft a vault page with version byte = 1. We don't need
    // valid ciphertext — the version check fires first.
    let db_path = scratch_path("legacy-v1");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();

    // Reserve slots so id 2 is a real on-disk page.
    while pager.page_count().unwrap() <= 2 {
        pager.allocate_page(PageType::Vault).unwrap();
    }

    let mut page = Page::new(PageType::Vault, 2);
    {
        let bytes = page.as_bytes_mut();
        bytes[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(b"RDVT");
        bytes[HEADER_SIZE + 4] = 1; // version 1 — legacy.
                                    // salt + payload_len + nonce can be zeros; we never decrypt.
    }
    pager.write_page_no_checksum(2, page).unwrap();
    pager.flush().unwrap();

    let vault = Vault::open(&pager, Some("does-not-matter")).unwrap();
    let err = vault.load(&pager).unwrap_err();
    match err {
        VaultError::Corrupt(msg) => {
            assert!(
                msg.contains("legacy") && msg.contains("re-bootstrap"),
                "expected legacy-format guidance, got: {msg}"
            );
        }
        other => panic!("expected Corrupt error, got {other:?}"),
    }
}
