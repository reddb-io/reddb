//! #1608 — concurrent claim runtime, end-to-end.
//!
//! The claim execution path is implemented in the server runtime
//! (`impl_dml.rs`): partial (`CLAIM LIMIT n`) and exact (`CLAIM EXACT n`)
//! cardinality, skip-locked over `pending_claim_locks`, lock release on
//! commit/rollback, the RLS read+update dual-policy gate, and `RETURNING`.
//! These tests exercise and pin each of those behaviors through public SQL.
//!
//! Coverage:
//!   1. Partial claim (`CLAIM LIMIT n`): up-to-n, `RETURNING id`, and
//!      rollback releasing the claim locks so the same records claim again.
//!   2. Exact claim (`CLAIM EXACT n`): success, miss (zero writes), and the
//!      explicit-transaction commit-publishes / rollback-restores boundary.
//!   3. Concurrency / skip-locked: two connection ids claiming from one pool
//!      take disjoint sets — no record claimed twice.
//!   4. RLS: claim candidate selection obeys both the read and update policy.
//!   5. Tenant isolation: one tenant's claim cannot select another's records.

use reddb::runtime::mvcc::{
    clear_current_connection_id, clear_current_tenant, set_current_connection_id,
    set_current_tenant,
};
use reddb::runtime::RuntimeQueryResult;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

/// The integer `id` cell of every returned record, in row order. Works for
/// both `RETURNING id` results and `SELECT id …` reads.
fn ids(result: &RuntimeQueryResult) -> Vec<i64> {
    result
        .result
        .records
        .iter()
        .map(|record| match record.get("id") {
            Some(Value::Integer(value)) => *value,
            Some(Value::UnsignedInteger(value)) => i64::try_from(*value).expect("u64 id fits i64"),
            other => panic!("expected integer id, got {other:?} in {record:?}"),
        })
        .collect()
}

/// The `id`s of every row of `table` currently carrying `status`, ascending.
fn ids_with_status(rt: &RedDBRuntime, table: &str, status: &str) -> Vec<i64> {
    let mut found = ids(&exec(
        rt,
        &format!("SELECT id FROM {table} WHERE status = '{status}' ORDER BY id ASC"),
    ));
    found.sort_unstable();
    found
}

/// A pool of `count` rows `(id 1..=count, status='ready')`.
fn seed_ready_pool(rt: &RedDBRuntime, table: &str, count: i64) {
    exec(rt, &format!("CREATE TABLE {table} (id INT, status TEXT)"));
    let rows: Vec<String> = (1..=count).map(|id| format!("({id}, 'ready')")).collect();
    exec(
        rt,
        &format!(
            "INSERT INTO {table} (id, status) VALUES {}",
            rows.join(", ")
        ),
    );
}

// ── Criterion 1: partial claim (`CLAIM LIMIT n`) ──────────────────────

/// `CLAIM LIMIT n` claims up to n immediately-claimable rows matching the
/// WHERE filter and `RETURNING id` delivers exactly those identities.
#[test]
fn partial_claim_takes_up_to_n_and_returns_claimed_ids() {
    let rt = runtime();
    seed_ready_pool(&rt, "partial_pool", 5);

    let claimed = exec(
        &rt,
        "UPDATE partial_pool SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 3 ORDER BY id ASC RETURNING id",
    );

    assert_eq!(claimed.affected_rows, 3, "claims exactly the LIMIT");
    assert_eq!(
        ids(&claimed),
        vec![1, 2, 3],
        "RETURNING delivers claimed ids"
    );
    assert_eq!(
        ids_with_status(&rt, "partial_pool", "claimed"),
        vec![1, 2, 3]
    );
    assert_eq!(
        ids_with_status(&rt, "partial_pool", "ready"),
        vec![4, 5],
        "unclaimed rows remain ready"
    );
}

/// A `CLAIM LIMIT n` whose n exceeds the claimable population takes only
/// what is available — "up to n", never more.
#[test]
fn partial_claim_limit_over_supply_claims_only_available() {
    let rt = runtime();
    seed_ready_pool(&rt, "partial_short", 2);

    let claimed = exec(
        &rt,
        "UPDATE partial_short SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 10 ORDER BY id ASC RETURNING id",
    );

    assert_eq!(
        claimed.affected_rows, 2,
        "only the two available are claimed"
    );
    assert_eq!(ids(&claimed), vec![1, 2]);
    assert!(
        ids_with_status(&rt, "partial_short", "ready").is_empty(),
        "no ready rows remain"
    );
}

/// Rolling back an explicit transaction that partial-claimed rows releases
/// the claim locks and reverts the writes, so the very same identities are
/// claimable again in a subsequent transaction.
#[test]
fn partial_claim_rollback_releases_locks_for_reclaim() {
    let rt = runtime();
    seed_ready_pool(&rt, "partial_rollback", 3);

    set_current_connection_id(160_801);
    exec(&rt, "BEGIN");
    let first = exec(
        &rt,
        "UPDATE partial_rollback SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(first.affected_rows, 2);
    let first_ids = ids(&first);
    assert_eq!(first_ids, vec![1, 2]);
    exec(&rt, "ROLLBACK");

    // After rollback the rows are ready again and unlocked: a fresh claim
    // (auto-commit, no open transaction) reclaims the identical identities.
    let second = exec(
        &rt,
        "UPDATE partial_rollback SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY id ASC RETURNING id",
    );
    clear_current_connection_id();

    assert_eq!(second.affected_rows, 2);
    assert_eq!(
        ids(&second),
        first_ids,
        "rolled-back claim releases the lock so the same rows reclaim"
    );
}

// ── Criterion 2: exact claim (`CLAIM EXACT n`) ────────────────────────

/// `CLAIM EXACT n` succeeds when at least n rows are claimable: it applies
/// exactly n writes and reports `affected_rows = n`.
#[test]
fn exact_claim_succeeds_when_enough_are_claimable() {
    let rt = runtime();
    seed_ready_pool(&rt, "exact_ok", 5);

    let claimed = exec(
        &rt,
        "UPDATE exact_ok SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 3 ORDER BY id ASC RETURNING id",
    );

    assert_eq!(claimed.affected_rows, 3);
    assert_eq!(ids(&claimed), vec![1, 2, 3]);
    assert_eq!(ids_with_status(&rt, "exact_ok", "ready"), vec![4, 5]);
}

/// `CLAIM EXACT n` misses when fewer than n rows are claimable: it applies
/// zero writes, `affected_rows = 0`, and `RETURNING` is empty.
#[test]
fn exact_claim_misses_and_writes_nothing_when_short() {
    let rt = runtime();
    seed_ready_pool(&rt, "exact_miss", 2);

    let claimed = exec(
        &rt,
        "UPDATE exact_miss SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 3 ORDER BY id ASC RETURNING id",
    );

    assert_eq!(
        claimed.affected_rows, 0,
        "an exact miss applies zero writes"
    );
    assert!(
        claimed.result.records.is_empty(),
        "RETURNING empty on a miss"
    );
    assert_eq!(
        ids_with_status(&rt, "exact_miss", "ready"),
        vec![1, 2],
        "every candidate stays ready after a miss"
    );
    assert!(
        ids_with_status(&rt, "exact_miss", "claimed").is_empty(),
        "no row is left claimed after a miss"
    );
}

/// An exact claim inside an explicit transaction: commit publishes the
/// claimed state, and a rollback of an identical claim restores it.
#[test]
fn exact_claim_in_explicit_transaction_commit_then_rollback() {
    let rt = runtime();
    seed_ready_pool(&rt, "exact_txn", 4);

    // Commit path — the claim becomes durable after COMMIT.
    set_current_connection_id(160_802);
    exec(&rt, "BEGIN");
    let committed = exec(
        &rt,
        "UPDATE exact_txn SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 2 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(committed.affected_rows, 2);
    exec(&rt, "COMMIT");
    assert_eq!(
        ids_with_status(&rt, "exact_txn", "claimed"),
        vec![1, 2],
        "COMMIT publishes the claimed state"
    );

    // Rollback path — a second claim inside a transaction is undone.
    exec(&rt, "BEGIN");
    let rolled = exec(
        &rt,
        "UPDATE exact_txn SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 2 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(
        rolled.affected_rows, 2,
        "the in-transaction claim reports n"
    );
    assert_eq!(ids(&rolled), vec![3, 4]);
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();

    assert_eq!(
        ids_with_status(&rt, "exact_txn", "ready"),
        vec![3, 4],
        "ROLLBACK restores the rows the transaction claimed"
    );
    assert_eq!(
        ids_with_status(&rt, "exact_txn", "claimed"),
        vec![1, 2],
        "only the committed claim survives"
    );
}

// ── Criterion 3: concurrency / skip-locked ────────────────────────────

/// Two distinct connection ids concurrently claiming from the same pool of
/// eligible rows each take a disjoint set — the second claimer skips the
/// rows the first still holds in its open transaction, so no record is
/// claimed twice.
#[test]
fn two_connections_claim_disjoint_sets_from_one_pool() {
    let rt = runtime();
    seed_ready_pool(&rt, "claim_pool", 4);

    // Connection A opens a transaction and claims the head of the pool. Its
    // pending claim locks persist for the life of the open transaction.
    set_current_connection_id(9001);
    exec(&rt, "BEGIN");
    let a = exec(
        &rt,
        "UPDATE claim_pool SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(a.affected_rows, 2);
    let a_ids = ids(&a);
    assert_eq!(a_ids, vec![1, 2]);

    // Connection B — a separate session — claims while A is still open. A's
    // uncommitted rows are still 'ready' under B's snapshot, but the
    // skip-locked filter hides them, so B takes the disjoint tail.
    set_current_connection_id(9002);
    exec(&rt, "BEGIN");
    let b = exec(
        &rt,
        "UPDATE claim_pool SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(b.affected_rows, 2);
    let b_ids = ids(&b);
    assert_eq!(
        b_ids,
        vec![3, 4],
        "the second claimer skips the rows the first still holds"
    );

    assert!(
        a_ids.iter().all(|id| !b_ids.contains(id)),
        "the two claim sets are disjoint: {a_ids:?} vs {b_ids:?}"
    );
    let mut union: Vec<i64> = a_ids.iter().chain(&b_ids).copied().collect();
    union.sort_unstable();
    union.dedup();
    assert_eq!(union, vec![1, 2, 3, 4], "no record was claimed twice");

    // Publish both transactions.
    exec(&rt, "COMMIT");
    set_current_connection_id(9001);
    exec(&rt, "COMMIT");
    clear_current_connection_id();

    assert_eq!(
        ids_with_status(&rt, "claim_pool", "claimed"),
        vec![1, 2, 3, 4],
        "every row is claimed exactly once after both commit"
    );
}

// ── Criterion 4: RLS read + update dual policy ────────────────────────

/// Claim candidate selection intersects the read (SELECT) policy and the
/// update (UPDATE) policy: a row must be admitted by BOTH to be claimed.
#[test]
fn claim_obeys_both_read_and_update_policy() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_rls (id INT, tenant_id TEXT, status TEXT)",
    );
    // Row 1,2: acme + ready  → admitted by both policies.
    // Row 3:   globex + ready → read policy (tenant) denies.
    // Row 4:   acme + done    → update policy (status='ready') denies.
    exec(
        &rt,
        "INSERT INTO claim_rls (id, tenant_id, status) VALUES \
         (1, 'acme', 'ready'), (2, 'acme', 'ready'), \
         (3, 'globex', 'ready'), (4, 'acme', 'done')",
    );
    exec(
        &rt,
        "CREATE POLICY rls_read ON claim_rls FOR SELECT USING (tenant_id = CURRENT_TENANT())",
    );
    exec(
        &rt,
        "CREATE POLICY rls_update ON claim_rls FOR UPDATE USING (status = 'ready')",
    );
    exec(&rt, "ALTER TABLE claim_rls ENABLE ROW LEVEL SECURITY");

    set_current_tenant("acme".to_string());
    let claimed = exec(
        &rt,
        "UPDATE claim_rls SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 10 ORDER BY id ASC RETURNING id",
    );
    clear_current_tenant();

    assert_eq!(
        ids(&claimed),
        vec![1, 2],
        "only rows admitted by both the read and the update policy are claimed"
    );
    assert_eq!(claimed.affected_rows, 2);

    // The globex row (read-denied) and the acme/done row (update-denied)
    // are both untouched.
    set_current_tenant("globex".to_string());
    assert_eq!(
        ids_with_status(&rt, "claim_rls", "ready"),
        vec![3],
        "the other tenant's row is never claimed by acme"
    );
    clear_current_tenant();
    set_current_tenant("acme".to_string());
    assert_eq!(
        ids_with_status(&rt, "claim_rls", "done"),
        vec![4],
        "the update-policy-denied row stays untouched"
    );
    clear_current_tenant();
}

// ── Criterion 5: tenant isolation ─────────────────────────────────────

/// One tenant's claim cannot select another tenant's records — even when
/// the WHERE clause explicitly names the other tenant, the RLS read policy
/// scopes the candidate set to the caller's tenant.
#[test]
fn cross_tenant_claim_is_denied() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_tenants (id INT, tenant_id TEXT, status TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO claim_tenants (id, tenant_id, status) VALUES \
         (1, 'acme', 'ready'), (2, 'globex', 'ready'), (3, 'globex', 'ready')",
    );
    exec(
        &rt,
        "CREATE POLICY tenant_read ON claim_tenants FOR SELECT USING (tenant_id = CURRENT_TENANT())",
    );
    exec(
        &rt,
        "CREATE POLICY tenant_update ON claim_tenants FOR UPDATE USING (tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE claim_tenants ENABLE ROW LEVEL SECURITY");

    // acme deliberately targets globex rows — RLS forces tenant='acme', so
    // the candidate set is empty and nothing is claimed.
    set_current_tenant("acme".to_string());
    let cross = exec(
        &rt,
        "UPDATE claim_tenants SET status = 'claimed' WHERE tenant_id = 'globex' \
         CLAIM LIMIT 10 ORDER BY id ASC RETURNING id",
    );
    clear_current_tenant();
    assert_eq!(
        cross.affected_rows, 0,
        "a cross-tenant claim selects nothing"
    );
    assert!(cross.result.records.is_empty());

    // The globex rows are still claimable by their own tenant, proving they
    // were withheld by isolation and not by some unrelated condition.
    set_current_tenant("globex".to_string());
    let owned = exec(
        &rt,
        "UPDATE claim_tenants SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 10 ORDER BY id ASC RETURNING id",
    );
    assert_eq!(
        ids(&owned),
        vec![2, 3],
        "globex claims its own rows once isolation admits them"
    );
    clear_current_tenant();
}
