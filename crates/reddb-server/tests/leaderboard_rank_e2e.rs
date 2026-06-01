//! #918 / ADR 0035 — leaderboard rank tracer bullet, end-to-end.
//!
//! Pins the slice this issue adds:
//!
//!   1. A Ranking capability declared on an ordinary table's score column
//!      as a catalog record (`CREATE RANKING …`, observable via
//!      `SHOW RANKINGS`) — no new Collection model, no `CREATE SORTEDSET`.
//!   2. `RANK() OVER (ORDER BY score DESC)` window semantics and a
//!      `RANK OF <id> IN <ranking>` rank-of-row read return a position
//!      within the bounded top-K head.
//!   3. The exact rank counts only rows visible to the querying MVCC
//!      snapshot — a row committed by a later transaction does not shift
//!      the rank seen under an older (snapshot-isolated) transaction.
//!   4. The exact rank agrees with the order `ORDER BY score DESC LIMIT`
//!      returns under the same snapshot (ties share a rank).
//!   5. Rank reads honor the same RLS/tenant scope as ordinary reads.
//!   6. Existing top-N (`ORDER BY score LIMIT`) is unchanged.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  -> {e}"));
}

/// Single `rank` cell from a `RANK OF …` read, or `None` when the read
/// returned no row (not visible / beyond the exact head).
fn rank_of(rt: &RedDBRuntime, sql: &str) -> Option<u64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  -> {e}"));
    result
        .result
        .records
        .first()
        .and_then(|rec| match rec.get("rank") {
            Some(Value::UnsignedInteger(n)) => Some(*n),
            Some(Value::Integer(n)) => Some(*n as u64),
            _ => None,
        })
}

/// Entity id (`rid`) of the row whose `name` column matches.
fn rid_of(rt: &RedDBRuntime, table: &str, name: &str) -> u64 {
    let result = rt
        .execute_query(&format!("SELECT * FROM {table}"))
        .expect("scan");
    for rec in &result.result.records {
        let matches = matches!(rec.get("name"), Some(Value::Text(t)) if t.as_ref() == name);
        if matches {
            return match rec.get("rid") {
                Some(Value::UnsignedInteger(n)) => *n,
                other => panic!("rid not an unsigned integer: {other:?}"),
            };
        }
    }
    panic!("no row named {name} in {table}");
}

fn seed_players(rt: &RedDBRuntime, rows: &[(&str, i64)]) {
    exec(rt, "CREATE TABLE players (name TEXT, score INT)");
    for (name, score) in rows {
        exec(
            rt,
            &format!("INSERT INTO players (name, score) VALUES ('{name}', {score})"),
        );
    }
}

// ── Criterion 1: capability declared as a catalog record ──────────────

#[test]
fn ranking_is_declared_as_a_catalog_capability() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE players (name TEXT, score INT)");
    exec(
        &rt,
        "CREATE RANKING top_players ON players (score DESC) TOP 100",
    );

    let shown = rt.execute_query("SHOW RANKINGS").expect("show rankings");
    assert_eq!(shown.result.records.len(), 1, "one ranking declared");
    let rec = &shown.result.records[0];
    assert!(matches!(rec.get("name"), Some(Value::Text(t)) if t.as_ref() == "top_players"));
    assert!(matches!(rec.get("table"), Some(Value::Text(t)) if t.as_ref() == "players"));
    assert!(matches!(rec.get("column"), Some(Value::Text(t)) if t.as_ref() == "score"));
    assert!(matches!(rec.get("direction"), Some(Value::Text(t)) if t.as_ref() == "DESC"));
    assert!(matches!(
        rec.get("top_k"),
        Some(Value::UnsignedInteger(100))
    ));

    // No Collection model was minted for the capability: the ranking name
    // is not a queryable collection.
    assert!(
        rt.execute_query("SELECT * FROM top_players").is_err(),
        "ranking must not materialize a collection named after itself"
    );

    // Declaring the same ranking twice is rejected.
    assert!(
        rt.execute_query("CREATE RANKING top_players ON players (score)")
            .is_err(),
        "duplicate ranking must be rejected"
    );
}

// ── Criteria 2 & 4: rank-of-row + window RANK agree with ORDER BY ─────

#[test]
fn rank_of_row_agrees_with_order_by_and_window_rank() {
    let rt = runtime();
    // Deliberate tie: bob and eve both score 80.
    seed_players(
        &rt,
        &[
            ("alice", 100),
            ("bob", 80),
            ("eve", 80),
            ("carol", 60),
            ("dave", 40),
        ],
    );
    exec(&rt, "CREATE RANKING r ON players (score DESC)");

    // Rank-of-row matches RANK() semantics: alice=1, bob=eve=2 (tie),
    // carol=4 (gap after the tie), dave=5.
    let id = |name| rid_of(&rt, "players", name);
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {} IN r", id("alice"))),
        Some(1)
    );
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {} IN r", id("bob"))),
        Some(2)
    );
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {} IN r", id("eve"))),
        Some(2)
    );
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {} IN r", id("carol"))),
        Some(4)
    );
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {} IN r", id("dave"))),
        Some(5)
    );

    // The canonical SQL window form returns the same ranks.
    let windowed = rt
        .execute_query("SELECT name, RANK() OVER (ORDER BY score DESC) AS rnk FROM players")
        .expect("window rank");
    let mut by_name: Vec<(String, i64)> = windowed
        .result
        .records
        .iter()
        .map(|rec| {
            let name = match rec.get("name") {
                Some(Value::Text(t)) => t.to_string(),
                other => panic!("name missing: {other:?}"),
            };
            let rnk = match rec.get("rnk") {
                Some(Value::Integer(n)) => *n,
                Some(Value::UnsignedInteger(n)) => *n as i64,
                other => panic!("rnk missing: {other:?}"),
            };
            (name, rnk)
        })
        .collect();
    by_name.sort();
    assert_eq!(
        by_name,
        vec![
            ("alice".to_string(), 1),
            ("bob".to_string(), 2),
            ("carol".to_string(), 4),
            ("dave".to_string(), 5),
            ("eve".to_string(), 2),
        ],
        "window RANK() must agree with the rank-of-row read"
    );

    // A non-existent / not-visible row id yields no rank row.
    assert_eq!(rank_of(&rt, "RANK OF 999999 IN r"), None);
}

// ── Criterion 4 (bound): rows beyond the exact head are not served ────

#[test]
fn rows_beyond_the_top_k_head_return_no_exact_rank() {
    let rt = runtime();
    seed_players(&rt, &[("a", 100), ("b", 90), ("c", 80), ("d", 70)]);
    // Exact head of size 2: only the top two ranks are served exactly.
    exec(&rt, "CREATE RANKING r ON players (score DESC) TOP 2");

    let id = |name| rid_of(&rt, "players", name);
    assert_eq!(rank_of(&rt, &format!("RANK OF {} IN r", id("a"))), Some(1));
    assert_eq!(rank_of(&rt, &format!("RANK OF {} IN r", id("b"))), Some(2));
    // c would rank 3, beyond the K=2 head ⇒ no exact rank (approximate
    // tail is a separate slice).
    assert_eq!(rank_of(&rt, &format!("RANK OF {} IN r", id("c"))), None);
    assert_eq!(rank_of(&rt, &format!("RANK OF {} IN r", id("d"))), None);
}

// ── Criterion 3: MVCC snapshot correctness ────────────────────────────

#[test]
fn uncommitted_row_in_another_transaction_does_not_shift_rank() {
    let rt = Arc::new(runtime());
    seed_players(&rt, &[("alice", 100), ("carol", 60)]);
    exec(&rt, "CREATE RANKING r ON players (score DESC)");
    let carol = rid_of(&rt, "players", "carol");

    // Baseline: carol is 2nd (alice above).
    assert_eq!(rank_of(&rt, &format!("RANK OF {carol} IN r")), Some(2));

    // A writer on another connection inserts a higher-scoring row inside
    // an *open, uncommitted* transaction and holds it open until the
    // reader has checked the rank. Channels make the handshake
    // deterministic — no sleeps, no timing assumptions.
    let (inserted_tx, inserted_rx) = mpsc::channel::<()>();
    let (commit_tx, commit_rx) = mpsc::channel::<()>();
    let writer = rt.clone();
    let writer_thread = thread::spawn(move || {
        // Bind a distinct connection id so this thread is an independent
        // session — otherwise it shares the default (auto-commit) id 0
        // with the reader and its uncommitted write would be self-visible.
        set_current_connection_id(2);
        writer.execute_query("BEGIN").expect("begin");
        writer
            .execute_query("INSERT INTO players (name, score) VALUES ('dave', 90)")
            .expect("insert dave");
        inserted_tx.send(()).expect("signal inserted");
        commit_rx.recv().expect("await commit signal");
        writer.execute_query("COMMIT").expect("commit");
        clear_current_connection_id();
    });

    // Wait until dave exists but is still uncommitted, then read the rank.
    inserted_rx.recv().expect("await insert");
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {carol} IN r")),
        Some(2),
        "an uncommitted row in another transaction must not shift the rank"
    );

    // Let the writer commit, then a fresh read sees dave: carol is 3rd.
    commit_tx.send(()).expect("signal commit");
    writer_thread.join().expect("writer thread");
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {carol} IN r")),
        Some(3),
        "once committed, the row counts and carol drops to 3rd"
    );
}

// ── Criterion 5: rank reads honor RLS / tenant scope ──────────────────

#[test]
fn rank_reads_honor_tenant_rls() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE players (name TEXT, tenant_id TEXT, score INT)",
    );
    exec(
        &rt,
        "INSERT INTO players (name, tenant_id, score) VALUES ('alice', 'acme', 100)",
    );
    exec(
        &rt,
        "INSERT INTO players (name, tenant_id, score) VALUES ('zoe', 'globex', 200)",
    );
    let alice = rid_of(&rt, "players", "alice");

    exec(
        &rt,
        "CREATE POLICY tenant_only ON players FOR SELECT USING (tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE players ENABLE ROW LEVEL SECURITY");
    exec(&rt, "CREATE RANKING r ON players (score DESC)");

    // Under tenant acme, zoe (score 200, globex) is invisible, so alice
    // ranks 1 — NOT 2. The rank walk inherits the same RLS filter as an
    // ordinary read.
    assert_eq!(
        rank_of(&rt, &format!("WITHIN TENANT 'acme' RANK OF {alice} IN r")),
        Some(1),
        "RLS must hide the other tenant's higher-scoring row from the rank"
    );

    // Under tenant globex, alice's own row is hidden ⇒ no rank.
    assert_eq!(
        rank_of(&rt, &format!("WITHIN TENANT 'globex' RANK OF {alice} IN r")),
        None,
        "a row hidden by RLS has no rank for that tenant"
    );
}

// ── Criterion 6: existing top-N is unchanged ──────────────────────────

#[test]
fn top_n_order_by_limit_is_unchanged() {
    let rt = runtime();
    seed_players(
        &rt,
        &[("alice", 100), ("bob", 80), ("carol", 60), ("dave", 40)],
    );
    // Declaring a ranking must not perturb ordinary top-N reads.
    exec(&rt, "CREATE RANKING r ON players (score DESC)");

    let top2 = rt
        .execute_query("SELECT name FROM players ORDER BY score DESC LIMIT 2")
        .expect("top-2");
    let names: Vec<String> = top2
        .result
        .records
        .iter()
        .map(|rec| match rec.get("name") {
            Some(Value::Text(t)) => t.to_string(),
            other => panic!("name missing: {other:?}"),
        })
        .collect();
    assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);
}
