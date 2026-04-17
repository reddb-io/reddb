//! Perf-parity config matrix — Tier A self-healing + Tier B defaults.
//!
//! On boot every `Tier::Critical` key must be visible through
//! `SHOW CONFIG <key>` (populated into red_config if absent). Tier B
//! keys stay silent until a user `SET CONFIG` writes them.

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn show_value(rt: &RedDBRuntime, key: &str) -> Option<String> {
    let result = rt
        .execute_query(&format!("SHOW CONFIG {key}"))
        .unwrap_or_else(|err| panic!("SHOW CONFIG {key}: {err:?}"));
    let record = result.result.records.first()?;
    // SHOW CONFIG echoes `{ key: <dotted>, value: <scalar> }`. The
    // value representation depends on the underlying Value type, so
    // the helper returns a string rendering — tests assert by
    // `.contains(...)`.
    record.values.get("value").map(|v| format!("{v:?}"))
}

#[test]
fn tier_a_critical_keys_are_self_healed_on_boot() {
    let rt = open_runtime();

    // Every Tier A key must resolve to something non-empty via
    // SHOW CONFIG immediately after boot. The scalar repr varies
    // (`Text("sync")`, `Boolean(true)`, `Number(10.0)`), so we only
    // confirm presence + a rough value match.
    assert!(show_value(&rt, "durability.mode").unwrap().contains("sync"));
    assert!(show_value(&rt, "concurrency.locking.enabled")
        .unwrap()
        .contains("true"));
    assert!(show_value(&rt, "storage.wal.max_interval_ms")
        .unwrap()
        .contains("10"));
    assert!(show_value(&rt, "storage.bgwriter.delay_ms")
        .unwrap()
        .contains("200"));
    assert!(show_value(&rt, "storage.btree.lehman_yao")
        .unwrap()
        .contains("true"));
}

#[test]
fn tier_b_optional_keys_stay_silent_until_user_sets_them() {
    let rt = open_runtime();

    // No record written yet — SHOW CONFIG either returns NULL or no
    // row. Either way, the stringified form must NOT carry a
    // user-visible numeric / text value from the default.
    let pre = show_value(&rt, "concurrency.locking.deadlock_timeout_ms");
    assert!(
        pre.as_deref() == Some("Null") || pre.is_none(),
        "Tier B key leaked a default into red_config: {pre:?}"
    );

    // After SET CONFIG, the same lookup must reveal the user's value.
    rt.execute_query("SET CONFIG concurrency.locking.deadlock_timeout_ms = 7500")
        .unwrap();
    assert!(show_value(&rt, "concurrency.locking.deadlock_timeout_ms")
        .unwrap()
        .contains("7500"));
}

#[test]
fn heal_is_idempotent_across_reboots() {
    use reddb::runtime::config_matrix::{default_for, tier_for, Tier, MATRIX};

    // Assert the matrix shape without rebooting (the healer already
    // ran on open_runtime). The intent is a tripwire: if somebody
    // drops a Tier A entry or mis-namespaces one, the lookup fails.
    for entry in MATRIX {
        assert_eq!(tier_for(entry.key), Some(entry.tier));
        assert!(default_for(entry.key).is_some());
    }

    // Idempotence check via SHOW CONFIG: value after second boot
    // call matches the initial default.
    let rt = open_runtime();
    let snap = show_value(&rt, "durability.mode").unwrap();
    drop(rt);
    let rt2 = open_runtime();
    let second = show_value(&rt2, "durability.mode").unwrap();
    assert_eq!(snap, second);

    // Unused import guard.
    let _ = Tier::Critical;
}
