//! Issue #593 — slice 9a of #575.
//!
//! `CREATE MATERIALIZED VIEW v AS …` must survive a server restart:
//! after the second open, `red.materialized_views` still lists `v`,
//! the view name resolves on `SELECT`, and `DROP MATERIALIZED VIEW`
//! clears the persisted catalog row.

#[allow(dead_code)]
mod support;

use reddb::{RedDBOptions, RedDBRuntime};

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

#[test]
fn materialized_view_survives_restart() {
    std::thread::Builder::new()
        .name("materialized-view-survives-restart".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(materialized_view_survives_restart_impl)
        .expect("spawn materialized view persistence test")
        .join()
        .expect("materialized view persistence test panicked");
}

fn materialized_view_survives_restart_impl() {
    let path = persistent_path("mv_persist_survives");

    // ── First boot: create table, insert rows, define materialized view.
    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("first open");
        exec(&rt, "CREATE TABLE orders (id INT, total INT, status TEXT)");
        exec(
            &rt,
            "INSERT INTO orders (id, total, status) VALUES \
               (1, 100, 'paid'), \
               (2, 200, 'paid'), \
               (3, 300, 'pending')",
        );
        exec(
            &rt,
            "CREATE MATERIALIZED VIEW paid_orders AS \
             SELECT * FROM orders WHERE status = 'paid'",
        );

        // Sanity: the view is present in the metadata snapshot
        // before restart, and the body resolves through the rewriter.
        let names: Vec<String> = rt
            .materialized_view_metadata()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "paid_orders"),
            "first boot: paid_orders must be present in materialized_view_metadata, got {names:?}",
        );
        rt.checkpoint().expect("flush before restart");
    }

    // ── Second boot: rehydrate must repopulate the registry before
    //    the API opens. The view name should resolve on SELECT and
    //    appear in `red.materialized_views`.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("reopen");
        let names: Vec<String> = rt
            .materialized_view_metadata()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "paid_orders"),
            "after restart: paid_orders must still be listed, got {names:?}",
        );

        // Issue #594 slice 9b — `SELECT FROM mv` resolves to the
        // backing collection rather than re-executing the body.
        // REFRESH hasn't been wired through the backing yet (lands
        // in 9c), so post-rehydrate the backing is empty. The point
        // of this slice's restart assertion is that the materialized
        // view itself still resolves as a known relation, not that
        // it returns rows.
        let result = exec(&rt, "SELECT * FROM paid_orders");
        assert_eq!(
            result.result.records.len(),
            0,
            "post-rehydrate, materialized view reads from empty backing collection",
        );

        // REFRESH against the rehydrated view also works — the
        // descriptor came back with its definition intact.
        let refreshed = exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
        assert_eq!(refreshed.statement_type, "refresh_materialized_view");
    }
}

#[test]
fn drop_materialized_view_removes_persisted_descriptor() {
    let path = persistent_path("mv_persist_drop");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("first open");
        exec(&rt, "CREATE TABLE t (id INT)");
        exec(&rt, "CREATE MATERIALIZED VIEW v AS SELECT * FROM t");
        exec(&rt, "DROP MATERIALIZED VIEW v");
        rt.checkpoint().expect("flush");
    }

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("reopen");
        let names: Vec<String> = rt
            .materialized_view_metadata()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            !names.iter().any(|n| n == "v"),
            "dropped view must not rehydrate, got {names:?}",
        );
    }
}
