//! Chaos: SIGKILL the engine mid-migration, restart, assert no
//! double-apply (#43 follow-up to #37).
//!
//! The in-process resume tests in `chaos_migration_batch_resume.rs`
//! pin the algorithmic correctness of `APPLY MIGRATION ... BATCH N
//! ROWS`. This drill exercises the same path under a real OS-level
//! kill: the engine cannot run Drop, cannot flush gracefully, and the
//! WAL/pager has to be the source of truth on restart.
//!
//! ## Design choices (per HITL approval)
//!
//! - **Custom temp dir** instead of the `tempfile` crate — keeps the
//!   dep tree stable; we manage cleanup with a Drop guard.
//! - **`VACUUM FULL` before kill** (durability gate) — forces a pager
//!   sync so the seeded rows are guaranteed on disk regardless of
//!   the autocommit-async-flush default.
//! - **TcpListener bind+drop port allocation** — a window race exists
//!   but in practice is fine for solo CI; the alternative
//!   (`portpicker` crate) would add another dep we don't want.
//! - **Linux-only** — `Child::kill()` on Unix sends SIGKILL with the
//!   exact semantics we want; on Windows it maps to `TerminateProcess`
//!   which has different finalisation behaviour. Test is gated to
//!   `#[cfg(unix)]`.
//! - **`#[ignore]` by default** — boots a real `red` server child
//!   which adds ~5–10s per run; runs explicitly via
//!   `cargo test -- --ignored chaos_migration_sigkill`.

#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reddb::client::RedDBClient;
use reddb::{RedDBOptions, RedDBRuntime};

/// Pick an OS-allocated port via bind+drop. Race window between drop
/// and the server's bind is small enough for serial CI runs; if it
/// becomes flaky in practice, swap for `portpicker`.
fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 0");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Spawn a `red server` child bound to `127.0.0.1:<port>` over gRPC
/// with persistent storage at `data_path`. Returns the live `Child`.
fn spawn_server(port: u16, data_path: &PathBuf) -> Child {
    let bin = env!("CARGO_BIN_EXE_red");
    Command::new(bin)
        .args([
            "server",
            "--grpc-bind",
            &format!("127.0.0.1:{port}"),
            "--path",
            data_path.to_str().expect("utf-8 path"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn red server")
}

/// Poll the gRPC endpoint until `health_status` succeeds or the
/// deadline expires. The server takes a few hundred ms to bind; this
/// avoids a flat sleep that's either too short (race) or too long
/// (CI slowdown).
fn wait_until_ready(port: u16) -> Result<(), String> {
    let endpoint = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");
    while Instant::now() < deadline {
        let probe = rt.block_on(async {
            let mut c = RedDBClient::connect(&endpoint, None).await?;
            c.health_status().await
        });
        if probe.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err(format!("server at {endpoint} never became healthy"))
}

#[test]
#[ignore = "spawns a real red server subprocess; run with --ignored"]
fn sigkill_mid_batched_migration_resumes_without_double_apply() {
    let temp = support::temp_data_dir("chaos-sigkill-migration");
    let data_path = temp.join("data.rdb");
    let port = pick_port();

    let mut child = spawn_server(port, &data_path);
    wait_until_ready(port).expect("server ready");

    // Seed + register migration via gRPC.
    let endpoint = format!("127.0.0.1:{port}");
    let tokio_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio");
    tokio_rt.block_on(async {
        let mut c = RedDBClient::connect(&endpoint, None)
            .await
            .expect("connect");
        c.query("CREATE TABLE work_items (id BIGINT, status TEXT)")
            .await
            .expect("create");
        // Modest row count — large enough that batched apply takes
        // multiple iterations, small enough to keep the test under
        // a few seconds. Resume correctness doesn't scale with N.
        for i in 0..200u64 {
            c.query(&format!(
                "INSERT INTO work_items (id, status) VALUES ({i}, 'pending')"
            ))
            .await
            .expect("insert");
        }
        // Durability gate: force a pager sync so SIGKILL after this
        // line cannot lose the seed. Without VACUUM FULL the
        // autocommit-async-flush default would race.
        c.query("VACUUM FULL").await.expect("vacuum full");

        c.query(
            "CREATE MIGRATION mark_done BATCH 5 ROWS AS \
             UPDATE work_items SET status = 'done' WHERE status = 'pending'",
        )
        .await
        .expect("create migration");
    });

    // Kick off APPLY MIGRATION on a background tokio task and
    // SIGKILL the server while it's mid-batch.
    let endpoint_kill = endpoint.clone();
    let _apply_handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio");
        let _ = rt.block_on(async {
            let mut c = RedDBClient::connect(&endpoint_kill, None).await?;
            c.query("APPLY MIGRATION mark_done").await
        });
    });

    // Sleep through ~1–2 batches, then kill. Sleep length is best-
    // effort: long enough that some work_items happens, short enough that
    // not all work_items happens.
    std::thread::sleep(Duration::from_millis(80));
    child.kill().expect("kill child");
    let _ = child.wait();

    // Reopen the data file in-process and resume.
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
        .expect("reopen persistent");

    rt.execute_query("APPLY MIGRATION mark_done")
        .expect("re-apply");

    // Every row must be 'done' exactly once — no skip, no double.
    let done = rt
        .execute_query("SELECT * FROM work_items WHERE status = 'done'")
        .expect("select done");
    assert_eq!(
        done.result.records.len(),
        200,
        "every row should be 'done' after resume — got {}",
        done.result.records.len()
    );

    let pending = rt
        .execute_query("SELECT * FROM work_items WHERE status = 'pending'")
        .expect("select pending");
    assert_eq!(
        pending.result.records.len(),
        0,
        "no row should remain 'pending'"
    );
}
