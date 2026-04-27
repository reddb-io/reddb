//! Vault capacity scale test.
//!
//! Lifts confidence that the dynamic-page-chain vault works for large
//! user populations (the old fixed two-page format capped out around
//! ~22 users). We populate a vault with thousands of users, save,
//! reload from a fresh `Vault` handle, and verify the round-trip is
//! byte-identical at the `VaultState` level.
//!
//! What this test guards against:
//!   * Off-by-one fragment splits when ciphertext spans many pages.
//!   * Page-id collision between the fixed header page and the
//!     pager's dynamic allocator.
//!   * Page-count not growing monotonically as the payload grows.
//!   * AES-GCM tag accidentally being split across pages without
//!     reassembly (would surface as a Decryption error).

use std::collections::HashMap;
use std::time::Instant;

use reddb::auth::vault::{Vault, VaultState};
use reddb::auth::{ApiKey, Role, User, UserId};
use reddb::storage::engine::pager::{Pager, PagerConfig};

/// Shared scratch dir per test invocation. Bytes don't survive
/// across runs but the directory is unique per (pid, counter).
fn scratch_path(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "reddb_vault_capacity_{}_{}_{}",
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

/// Build a `VaultState` with `n` users — enough text to overflow the
/// header page and force a multi-page chain even at modest counts.
fn synth_state(n: usize) -> VaultState {
    let now = now_ms();
    let mut users = Vec::with_capacity(n);
    let mut api_keys = Vec::with_capacity(n);
    for i in 0..n {
        let username = format!("user-{i:06}");
        users.push(User {
            username: username.clone(),
            tenant_id: None,
            password_hash: format!("argon2id$salt-{i:08x}$hash-{i:08x}"),
            scram_verifier: None,
            role: if i % 7 == 0 { Role::Admin } else { Role::Read },
            api_keys: Vec::new(),
            created_at: now,
            updated_at: now + i as u128,
            enabled: i % 3 != 0,
        });
        api_keys.push((
            UserId::platform(username),
            ApiKey {
                key: format!("rk_{i:016x}"),
                name: format!("token-{i}"),
                role: Role::Write,
                created_at: now,
            },
        ));
    }
    VaultState {
        users,
        api_keys,
        bootstrapped: true,
        master_secret: Some(vec![0xAA; 32]),
        kv: HashMap::new(),
    }
}

/// Compare two states field-by-field. Lighter than deriving Eq across
/// the whole auth tree.
fn assert_states_eq(a: &VaultState, b: &VaultState) {
    assert_eq!(a.bootstrapped, b.bootstrapped);
    assert_eq!(a.master_secret, b.master_secret);
    assert_eq!(a.users.len(), b.users.len(), "user count mismatch");
    assert_eq!(
        a.api_keys.len(),
        b.api_keys.len(),
        "api_keys count mismatch"
    );

    for (l, r) in a.users.iter().zip(b.users.iter()) {
        assert_eq!(l.username, r.username);
        assert_eq!(l.password_hash, r.password_hash);
        assert_eq!(l.role, r.role);
        assert_eq!(l.enabled, r.enabled);
        assert_eq!(l.created_at, r.created_at);
        assert_eq!(l.updated_at, r.updated_at);
    }

    // api_keys is HashMap-ish iteration on the way in, so compare as
    // sorted sets to dodge ordering noise.
    let key_for = |id: &UserId, k: &ApiKey| {
        let tenant = id.tenant.clone().unwrap_or_default();
        format!("{}:{}:{}", tenant, id.username, k.key)
    };
    let mut left: Vec<_> = a.api_keys.iter().collect();
    let mut right: Vec<_> = b.api_keys.iter().collect();
    left.sort_by(|x, y| key_for(&x.0, &x.1).cmp(&key_for(&y.0, &y.1)));
    right.sort_by(|x, y| key_for(&x.0, &x.1).cmp(&key_for(&y.0, &y.1)));
    for ((u1, k1), (u2, k2)) in left.iter().zip(right.iter()) {
        assert_eq!(u1.username, u2.username);
        assert_eq!(u1.tenant, u2.tenant);
        assert_eq!(k1.key, k2.key);
        assert_eq!(k1.name, k2.name);
        assert_eq!(k1.role, k2.role);
    }
}

#[test]
fn vault_round_trip_5000_users() {
    let db_path = scratch_path("5k");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();

    let state = synth_state(5_000);
    let serialized_len = state.serialize().len();

    let vault = Vault::open(&pager, Some("vault-capacity-passphrase")).unwrap();

    let pages_before = pager.page_count().unwrap();
    let save_t = Instant::now();
    vault.save(&pager, &state).unwrap();
    let save_ms = save_t.elapsed().as_millis();
    let pages_after_save = pager.page_count().unwrap();

    assert!(
        pages_after_save > pages_before,
        "page_count must grow when saving {} users (before={}, after={})",
        state.users.len(),
        pages_before,
        pages_after_save
    );

    // Drop and re-open the pager to force on-disk rather than cached
    // reads. This is the bit that catches "wrote it to cache, never
    // hit the file" bugs.
    drop(vault);
    drop(pager);
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("vault-capacity-passphrase")).unwrap();

    let load_t = Instant::now();
    let loaded = vault.load(&pager).unwrap().expect("vault must load");
    let load_ms = load_t.elapsed().as_millis();

    assert_states_eq(&state, &loaded);

    let pages_total = pager.page_count().unwrap();
    eprintln!(
        "5000 users -> plaintext {} bytes, {} pages total, save {} ms, load {} ms",
        serialized_len, pages_total, save_ms, load_ms
    );
}

/// Drive the chain through three sizes so we exercise grow → grow →
/// shrink. The shrink leg verifies that surplus pages are returned to
/// the freelist (page_count must not blow up unboundedly).
#[test]
fn vault_grows_and_shrinks_monotonically() {
    let db_path = scratch_path("growshrink");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("vault-grow-shrink")).unwrap();

    // 1) small — fits comfortably
    let small = synth_state(10);
    vault.save(&pager, &small).unwrap();
    let pc_small = pager.page_count().unwrap();

    // 2) big — forces the chain to grow
    let big = synth_state(2_000);
    vault.save(&pager, &big).unwrap();
    let pc_big = pager.page_count().unwrap();
    assert!(
        pc_big > pc_small,
        "page_count should grow from small to big (small={pc_small}, big={pc_big})"
    );

    let loaded_big = vault.load(&pager).unwrap().expect("must load big state");
    assert_states_eq(&big, &loaded_big);

    // 3) back down — chain pages should be freed (page_count may
    //    stay flat because the file doesn't shrink, but the freelist
    //    must absorb the unused ids — re-saving big shouldn't grow
    //    the file again).
    vault.save(&pager, &small).unwrap();
    let pc_small_again = pager.page_count().unwrap();
    let loaded_small = vault.load(&pager).unwrap().expect("must load shrunk state");
    assert_states_eq(&small, &loaded_small);

    // Re-save the big state. Because step 3 freed pages, this should
    // recycle them rather than extending the file beyond pc_big.
    vault.save(&pager, &big).unwrap();
    let pc_big_again = pager.page_count().unwrap();
    assert!(
        pc_big_again <= pc_big + 1,
        "page_count must not grow on second big save (was {pc_big}, now {pc_big_again}); \
         freelist isn't reclaiming surplus chain pages"
    );

    eprintln!(
        "grow/shrink: small={pc_small} pages, big={pc_big} pages, small_again={pc_small_again} pages, big_again={pc_big_again} pages"
    );
}

/// One user — the chain should have zero data pages because the
/// payload fits in the header alone. This is the "no chain at all"
/// edge case the spec explicitly calls out.
#[test]
fn vault_single_user_fits_in_header_page() {
    let db_path = scratch_path("single");
    let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
    let vault = Vault::open(&pager, Some("vault-single-user")).unwrap();

    let state = synth_state(1);
    vault.save(&pager, &state).unwrap();

    let pages_after = pager.page_count().unwrap();
    // We had to bump page_count up to 3 to reserve the header slot;
    // we don't allocate any data pages for a one-user payload, so
    // page_count should sit at exactly 3.
    assert_eq!(
        pages_after, 3,
        "one-user vault should not allocate any data pages; got page_count={pages_after}"
    );

    let loaded = vault.load(&pager).unwrap().unwrap();
    assert_states_eq(&state, &loaded);
}
