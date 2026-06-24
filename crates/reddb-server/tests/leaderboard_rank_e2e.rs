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

/// `(rank, rid, name)` rows from a `RANK RANGE …` read.
fn rank_range_rows(rt: &RedDBRuntime, sql: &str) -> Vec<(u64, u64, String)> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  -> {e}"));
    result
        .result
        .records
        .iter()
        .map(|rec| {
            let rank = match rec.get("rank") {
                Some(Value::UnsignedInteger(n)) => *n,
                Some(Value::Integer(n)) => *n as u64,
                other => panic!("rank missing/typed wrong: {other:?}"),
            };
            let rid = match rec.get("rid") {
                Some(Value::UnsignedInteger(n)) => *n,
                Some(Value::Integer(n)) => *n as u64,
                other => panic!("rid missing/typed wrong: {other:?}"),
            };
            let name = match rec.get("name") {
                Some(Value::Text(t)) => t.to_string(),
                other => panic!("name missing/typed wrong: {other:?}"),
            };
            (rank, rid, name)
        })
        .collect()
}

fn query_snapshot(rt: &RedDBRuntime, sql: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  -> {e}"))
        .result;
    let rows = result
        .records
        .iter()
        .map(|record| {
            result
                .columns
                .iter()
                .map(|column| record.get(column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    (result.columns, rows)
}

/// An `APPROX RANK OF …` row, or `None` when the read returned no row.
struct ApproxRow {
    rank: u64,
    percentile: f64,
    approximate: bool,
}

fn approx_rank_of(rt: &RedDBRuntime, sql: &str) -> Option<ApproxRow> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  -> {e}"));
    let rec = result.result.records.first()?;
    let rank = match rec.get("rank") {
        Some(Value::UnsignedInteger(n)) => *n,
        Some(Value::Integer(n)) => *n as u64,
        other => panic!("rank missing/typed wrong: {other:?}"),
    };
    let percentile = match rec.get("percentile") {
        Some(Value::Float(f)) => *f,
        Some(Value::Integer(n)) => *n as f64,
        Some(Value::UnsignedInteger(n)) => *n as f64,
        other => panic!("percentile missing/typed wrong: {other:?}"),
    };
    let approximate = match rec.get("approximate") {
        Some(Value::Boolean(b)) => *b,
        other => panic!("approximate missing/typed wrong: {other:?}"),
    };
    Some(ApproxRow {
        rank,
        percentile,
        approximate,
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

// ══════════════════════════════════════════════════════════════════════
// Issue #924 — exact head range-by-rank reads for position pagination.
// ══════════════════════════════════════════════════════════════════════

#[test]
fn rank_range_returns_entries_in_rank_order() {
    let rt = runtime();
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

    let rows = rank_range_rows(&rt, "RANK RANGE 2 TO 4 IN r");
    let projected: Vec<(u64, String)> = rows
        .into_iter()
        .map(|(rank, _rid, name)| (rank, name))
        .collect();
    assert_eq!(
        projected,
        vec![
            (2, "bob".to_string()),
            (2, "eve".to_string()),
            (4, "carol".to_string()),
        ],
        "range read must return entries in rank order, including tied rank rows"
    );
}

#[test]
fn rank_range_pages_are_gap_free_on_a_stable_snapshot() {
    let rt = runtime();
    seed_uniform(&rt, 6);
    exec(&rt, "CREATE RANKING r ON players (score DESC)");

    set_current_connection_id(1);
    exec(&rt, "BEGIN");
    let page1 = rank_range_rows(&rt, "RANK RANGE 1 TO 2 IN r");

    set_current_connection_id(2);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        "INSERT INTO players (name, score) VALUES ('late_winner', 1000)",
    );
    exec(&rt, "COMMIT");

    set_current_connection_id(1);
    let page2 = rank_range_rows(&rt, "RANK RANGE 3 TO 4 IN r");
    let page3 = rank_range_rows(&rt, "RANK RANGE 5 TO 6 IN r");
    exec(&rt, "COMMIT");
    clear_current_connection_id();

    let names: Vec<String> = page1
        .into_iter()
        .chain(page2)
        .chain(page3)
        .map(|(_rank, _rid, name)| name)
        .collect();
    assert_eq!(
        names,
        vec![
            "p6".to_string(),
            "p5".to_string(),
            "p4".to_string(),
            "p3".to_string(),
            "p2".to_string(),
            "p1".to_string(),
        ],
        "stable-snapshot pages must not overlap, leave gaps, or shift after a later commit"
    );
}

#[test]
fn rank_range_ties_are_deterministic_and_match_rank_of() {
    let rt = runtime();
    seed_players(&rt, &[("a", 100), ("b", 100), ("c", 100), ("d", 90)]);
    exec(&rt, "CREATE RANKING r ON players (score DESC) TOP 2");

    let a = rid_of(&rt, "players", "a");
    let b = rid_of(&rt, "players", "b");
    let c = rid_of(&rt, "players", "c");

    let rows = rank_range_rows(&rt, "RANK RANGE 1 TO 1 IN r");
    let projected: Vec<(u64, u64, String)> = rows;
    assert_eq!(
        projected,
        vec![(1, a, "a".to_string()), (1, b, "b".to_string())],
        "equal scores inside the bounded head must be ordered by rid"
    );
    assert_eq!(rank_of(&rt, &format!("RANK OF {a} IN r")), Some(1));
    assert_eq!(rank_of(&rt, &format!("RANK OF {b} IN r")), Some(1));
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {c} IN r")),
        None,
        "the same deterministic TOP k boundary applies to RANK OF"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Issue #925 — Redis-flavor Z* sugar desugars to canonical rank reads.
// ══════════════════════════════════════════════════════════════════════

#[test]
fn zrank_returns_the_same_rows_as_rank_of() {
    let rt = runtime();
    seed_players(&rt, &[("alice", 100), ("bob", 80), ("eve", 80)]);
    exec(&rt, "CREATE RANKING r ON players (score DESC)");
    let bob = rid_of(&rt, "players", "bob");

    assert_eq!(
        query_snapshot(&rt, &format!("ZRANK r {bob}")),
        query_snapshot(&rt, &format!("RANK OF {bob} IN r")),
        "ZRANK must be parser sugar for the canonical rank-of-row read"
    );
    assert_eq!(
        query_snapshot(&rt, "ZRANK r 999999"),
        query_snapshot(&rt, "RANK OF 999999 IN r"),
        "missing rows must desugar to the same empty result"
    );
}

#[test]
fn zrange_withscores_returns_the_same_rows_as_rank_range_for_ties_and_empty_pages() {
    let rt = runtime();
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

    assert_eq!(
        query_snapshot(&rt, "ZRANGE r 1 3 WITHSCORES"),
        query_snapshot(&rt, "RANK RANGE 2 TO 4 IN r"),
        "ZRANGE offsets must desugar to the canonical one-based rank range"
    );
    assert_eq!(
        query_snapshot(&rt, "ZRANGE r 20 30 WITHSCORES"),
        query_snapshot(&rt, "RANK RANGE 21 TO 31 IN r"),
        "out-of-range pages must desugar to the same empty result"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Issue #923 — approximate tail: percentile/rank for entries below the
// exact top-K head, served from a per-(collection, score column) sketch.
// ══════════════════════════════════════════════════════════════════════

/// Seed `n` players with score == their index (1..=n), a known uniform
/// distribution the error band can be checked against.
fn seed_uniform(rt: &RedDBRuntime, n: u64) {
    exec(rt, "CREATE TABLE players (name TEXT, score INT)");
    for v in 1..=n {
        exec(
            rt,
            &format!("INSERT INTO players (name, score) VALUES ('p{v}', {v})"),
        );
    }
}

// ── #923 criteria 1 & 2: tail entry gets an approximate, labeled rank ──

#[test]
fn tail_entry_below_head_gets_a_labeled_approximate_rank() {
    let rt = runtime();
    seed_uniform(&rt, 100);
    // Exact head of just 5: everything below rank 5 is the approximate tail.
    exec(&rt, "CREATE RANKING r ON players (score DESC) TOP 5");

    // p50 (score 50) ranks 51 exactly (50 higher scores) — well into the
    // tail. The exact surface serves no rank for it…
    let p50 = rid_of(&rt, "players", "p50");
    assert_eq!(
        rank_of(&rt, &format!("RANK OF {p50} IN r")),
        None,
        "an entry below the top-K head has no exact rank"
    );

    // …but the approximate surface does, explicitly labeled approximate.
    let approx = approx_rank_of(&rt, &format!("APPROX RANK OF {p50} IN r"))
        .expect("approximate tail rank is served");
    assert!(
        approx.approximate,
        "tail result must be labeled approximate, not presented as exact"
    );
    assert!(
        approx.rank >= 49 && approx.rank <= 53,
        "approx rank {} should sit near the exact 51",
        approx.rank
    );
    // Percentile: ~half the field ranks at or below p50.
    assert!(
        (40.0..=60.0).contains(&approx.percentile),
        "percentile {} should sit near the middle",
        approx.percentile
    );

    // A non-existent / invisible row yields no approximate row either.
    assert!(approx_rank_of(&rt, "APPROX RANK OF 999999 IN r").is_none());
}

// ── #923 criterion 3: estimate within the documented error band ───────

#[test]
fn approx_rank_stays_within_documented_error_band() {
    let rt = runtime();
    let n = 500u64;
    seed_uniform(&rt, n);
    exec(&rt, "CREATE RANKING r ON players (score DESC) TOP 10");

    // The sketch uses 256 equi-width buckets; the documented bound is
    // |approx − exact| ≤ max bucket population, which for this spread
    // distribution is ≈ total/256. Allow a small slack for rounding.
    let band = (n / 256) + 3;
    for v in (20..=n).step_by(37) {
        let exact = n - v + 1; // descending: v higher scores beat score v
        let id = rid_of(&rt, "players", &format!("p{v}"));
        let approx = approx_rank_of(&rt, &format!("APPROX RANK OF {id} IN r"))
            .expect("approx rank")
            .rank;
        let delta = exact.abs_diff(approx);
        assert!(
            delta <= band,
            "score {v}: approx {approx} vs exact {exact} exceeds band {band}"
        );
    }
}

// ── #923 criterion 4: sketch is per-(table,column) & tracks score changes ──

#[test]
fn approx_rank_tracks_score_changes() {
    let rt = runtime();
    seed_uniform(&rt, 100);
    exec(&rt, "CREATE RANKING r ON players (score DESC) TOP 5");

    let p50 = rid_of(&rt, "players", "p50");
    let before = approx_rank_of(&rt, &format!("APPROX RANK OF {p50} IN r"))
        .expect("approx rank before")
        .rank;
    assert!(
        (49..=53).contains(&before),
        "baseline approx rank {before} near 51"
    );

    // Insert 100 new players that all outscore p50. The per-(table,column)
    // sketch must reflect the new scores: p50 drops far down the board.
    for k in 1..=100 {
        exec(
            &rt,
            &format!("INSERT INTO players (name, score) VALUES ('hi{k}', 1000)"),
        );
    }
    let after = approx_rank_of(&rt, &format!("APPROX RANK OF {p50} IN r"))
        .expect("approx rank after")
        .rank;
    assert!(
        after >= before + 80,
        "after adding 100 higher scores p50 must drop sharply: {before} -> {after}"
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
