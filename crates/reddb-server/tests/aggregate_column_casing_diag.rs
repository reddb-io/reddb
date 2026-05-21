//! THROWAWAY DIAGNOSTIC for reported bug #9 (1.1.2): inconsistent
//! casing of aggregate result-set column names (COUNT(*) vs count(*),
//! SUM vs sum). Goal: observe externally-visible column-name keys for
//! the SAME aggregate across pushdown vs general execution and across
//! lower/upper user input. Not an assertion of internals — we print the
//! actual projected column-name strings.

use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn col_names(r: &RuntimeQueryResult) -> Vec<String> {
    r.result.records[0]
        .column_names()
        .iter()
        .map(|c| c.to_string())
        .collect()
}

fn seed(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE t (id INT, amount INT)")
        .expect("create t");
    rt.execute_query("INSERT INTO t (id, amount) VALUES (1, 10), (2, 20), (3, 30)")
        .expect("insert t");
}

fn run(rt: &RedDBRuntime, label: &str, sql: &str) -> Vec<String> {
    let r = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed [{label}]: {sql}\n  err: {e}"));
    assert!(
        !r.result.records.is_empty(),
        "no rows returned for [{label}]: {sql}"
    );
    let names = col_names(&r);
    println!("[{label:<28}] {sql}\n    -> columns: {names:?}");
    names
}

#[test]
fn aggregate_column_name_casing_diag() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed(&rt);

    println!("\n===== AGGREGATE COLUMN-NAME CASING DIAGNOSTIC =====\n");

    // --- COUNT(*), no alias, pushdownable plain form ---
    let count_lower_plain = run(&rt, "count-lower-plain", "SELECT count(*) FROM t");
    let count_upper_plain = run(&rt, "COUNT-upper-plain", "SELECT COUNT(*) FROM t");

    // --- SUM(amount), no alias, pushdownable plain form ---
    let sum_lower_plain = run(&rt, "sum-lower-plain", "SELECT sum(amount) FROM t");
    let sum_upper_plain = run(&rt, "SUM-upper-plain", "SELECT SUM(amount) FROM t");

    // --- Forms that try to defeat aggregate pushdown / route general ---
    // GROUP BY with a HAVING-ish / extra column tends to route differently.
    let count_lower_group = run(
        &rt,
        "count-lower-groupby",
        "SELECT count(*) FROM t GROUP BY id",
    );
    let count_upper_group = run(
        &rt,
        "COUNT-upper-groupby",
        "SELECT COUNT(*) FROM t GROUP BY id",
    );
    let sum_lower_group = run(
        &rt,
        "sum-lower-groupby",
        "SELECT sum(amount) FROM t GROUP BY id",
    );
    let sum_upper_group = run(
        &rt,
        "SUM-upper-groupby",
        "SELECT SUM(amount) FROM t GROUP BY id",
    );

    // --- Wrapped in subquery (general path, projected out) ---
    let count_lower_sub = run(
        &rt,
        "count-lower-subquery",
        "SELECT (SELECT count(*) FROM t) AS dummy FROM t LIMIT 1",
    );
    let _ = &count_lower_sub; // alias forces 'dummy'; kept for record only

    // --- Aggregate alongside another aggregate (multi-agg can route general) ---
    let count_lower_mixed = run(
        &rt,
        "count-lower-multi",
        "SELECT count(*), sum(amount) FROM t",
    );
    let sum_lower_mixed = run(&rt, "SUM-upper-multi", "SELECT COUNT(*), SUM(amount) FROM t");

    println!("\n===== SUMMARY =====");
    println!("COUNT plain : lower={count_lower_plain:?}  upper={count_upper_plain:?}");
    println!("SUM   plain : lower={sum_lower_plain:?}  upper={sum_upper_plain:?}");
    println!("COUNT group : lower={count_lower_group:?}  upper={count_upper_group:?}");
    println!("SUM   group : lower={sum_lower_group:?}  upper={sum_upper_group:?}");
    println!("COUNT mixed : lower={count_lower_mixed:?}");
    println!("SUM   mixed : lower={sum_lower_mixed:?}");
    println!("===================\n");

    // Helper: pick the column matching a given aggregate kind prefix.
    let agg_of = |names: &[String], prefix: &str| -> String {
        names
            .iter()
            .find(|n| n.to_ascii_lowercase().starts_with(prefix))
            .cloned()
            .unwrap_or_else(|| format!("<no {prefix} col in {names:?}>"))
    };
    let count_of = |n: &[String]| agg_of(n, "count(");
    let sum_of = |n: &[String]| agg_of(n, "sum(");

    let observed = [
        ("count-lower-plain", count_of(&count_lower_plain)),
        ("COUNT-upper-plain", count_of(&count_upper_plain)),
        ("sum-lower-plain", sum_of(&sum_lower_plain)),
        ("SUM-upper-plain", sum_of(&sum_upper_plain)),
        ("count-lower-groupby", count_of(&count_lower_group)),
        ("COUNT-upper-groupby", count_of(&count_upper_group)),
        ("sum-lower-groupby", sum_of(&sum_lower_group)),
        ("SUM-upper-groupby", sum_of(&sum_upper_group)),
        ("count-lower-multi", count_of(&count_lower_mixed)),
        ("sum-lower-multi", sum_of(&count_lower_mixed)),
        ("COUNT-upper-multi", count_of(&sum_lower_mixed)),
        ("SUM-upper-multi", sum_of(&sum_lower_mixed)),
    ];

    println!("AGGREGATE COLUMN KEY PER FORM:");
    for (label, name) in &observed {
        println!("  {label:<24} -> {name:?}");
    }
    println!();

    // VERDICT computation: do all COUNT forms agree, and all SUM forms agree?
    let count_keys: Vec<&String> = observed
        .iter()
        .filter(|(l, _)| l.to_ascii_lowercase().contains("count"))
        .map(|(_, n)| n)
        .collect();
    let sum_keys: Vec<&String> = observed
        .iter()
        .filter(|(l, _)| l.to_ascii_lowercase().contains("sum"))
        .map(|(_, n)| n)
        .collect();

    let count_consistent = count_keys.windows(2).all(|w| w[0] == w[1]);
    let sum_consistent = sum_keys.windows(2).all(|w| w[0] == w[1]);

    println!(
        "COUNT keys consistent across all forms? {count_consistent} ({count_keys:?})"
    );
    println!("SUM   keys consistent across all forms? {sum_consistent} ({sum_keys:?})");

    // This assertion encodes the DESIRED behavior (consistent casing).
    // If the bug reproduces, it FAILS and the printout above shows the divergence.
    assert!(
        count_consistent && sum_consistent,
        "INCONSISTENT aggregate column-name casing reproduced. See per-form keys above."
    );
}
