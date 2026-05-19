//! Issue #593 — slice 9a of #575.
//!
//! `CREATE MATERIALIZED VIEW v AS …` must survive a server restart:
//! after the second open, `red.materialized_views` still lists `v`,
//! the view name resolves on `SELECT`, and `DROP MATERIALIZED VIEW`
//! clears the persisted catalog row.

use reddb::{RedDBOptions, RedDBRuntime};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for ext in ["-wal", "-hdr", "-meta", "-dwb"] {
        let mut p = path.to_path_buf().into_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

#[test]
fn materialized_view_survives_restart() {
    let path = persistent_path("mv_persist_survives");
    cleanup(&path);

    // ── First boot: create table, insert rows, define materialized view.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("first open");
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
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("reopen");
        let names: Vec<String> = rt
            .materialized_view_metadata()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "paid_orders"),
            "after restart: paid_orders must still be listed, got {names:?}",
        );

        // The view body resolves: SELECT against the view name runs
        // the underlying query through the rewriter. Two of the
        // three orders are paid.
        let result = exec(&rt, "SELECT * FROM paid_orders");
        assert_eq!(
            result.result.records.len(),
            2,
            "view body must rewrite to filtered orders after restart",
        );

        // REFRESH against the rehydrated view also works — the
        // descriptor came back with its definition intact.
        let refreshed = exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
        assert_eq!(refreshed.statement_type, "refresh_materialized_view");
    }

    cleanup(&path);
}

#[test]
fn drop_materialized_view_removes_persisted_descriptor() {
    let path = persistent_path("mv_persist_drop");
    cleanup(&path);

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("first open");
        exec(&rt, "CREATE TABLE t (id INT)");
        exec(
            &rt,
            "CREATE MATERIALIZED VIEW v AS SELECT * FROM t",
        );
        exec(&rt, "DROP MATERIALIZED VIEW v");
        rt.checkpoint().expect("flush");
    }

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("reopen");
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

    cleanup(&path);
}
