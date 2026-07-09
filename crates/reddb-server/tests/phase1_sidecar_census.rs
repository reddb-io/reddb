//! ADR 0038 §4 phase 1, exit criterion (a): the fresh-store sidecar census.
//!
//! A store on the promoted embedded profile goes through DDL, DML, checkpoint
//! and reopen. At **every** step a directory glob asserts that no retired
//! phase-1 artifact — `rdb-hdr`, `rdb-meta`, or their shadow-suffix companions —
//! exists. A retired extension reappearing is a test failure, not a review
//! comment.
//!
//! The census is deliberately scoped to the phase-1 family. The WAL region
//! (phase 2) and the double-write buffer (phase 3) keep their sidecars for now,
//! so a blanket "one file only" assertion would fail for the wrong reason and
//! would silently start passing once those phases land.

use std::fs;
use std::path::{Path, PathBuf};

use reddb_server::{RedDBOptions, RedDBRuntime};

/// Suffixes the phase-1 retirement removed. Written out here rather than
/// derived from `reddb_file::layout::retired` on purpose: the census must fail
/// if a future change quietly redefines what "retired" means.
const RETIRED_PHASE1_SUFFIXES: [&str; 4] = ["rdb-hdr", "rdb-meta", "-hdr", "-meta"];

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-phase1-census-{label}-"))
        .tempdir()
        .expect("temp dir")
}

/// Every retired phase-1 artifact anywhere under `dir`, by glob over the names.
fn census(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if RETIRED_PHASE1_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn assert_no_phase1_sidecars(dir: &Path, after: &str) {
    let found = census(dir);
    assert!(
        found.is_empty(),
        "phase-1 sidecar reappeared after {after}: {found:?}\n\
         The superblock pair and the internal manifest live inside the .rdb \
         (ADR 0038 §2); nothing may recreate these files."
    );
}

#[test]
fn a_promoted_embedded_store_never_creates_a_phase_one_sidecar() {
    let dir = temp_dir("lifecycle");
    let path = dir.path().join("data.rdb");

    assert_no_phase1_sidecars(dir.path(), "an empty directory");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        assert_no_phase1_sidecars(dir.path(), "store creation");

        runtime
            .execute_query("CREATE TABLE users (id INT, name TEXT)")
            .expect("create table");
        assert_no_phase1_sidecars(dir.path(), "DDL");

        runtime
            .execute_query("INSERT INTO users (id, name) VALUES (1, 'ada'), (2, 'linus')")
            .expect("insert rows");
        assert_no_phase1_sidecars(dir.path(), "DML");

        runtime.flush().expect("checkpoint");
        assert_no_phase1_sidecars(dir.path(), "checkpoint");
    }
    assert_no_phase1_sidecars(dir.path(), "close");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen runtime");
        assert_no_phase1_sidecars(dir.path(), "reopen");

        let rows = runtime
            .execute_query("SELECT * FROM users")
            .expect("select rows");
        assert_eq!(
            rows.result.records.len(),
            2,
            "the round trip must survive the zoned layout"
        );

        runtime
            .execute_query("INSERT INTO users (id, name) VALUES (3, 'grace')")
            .expect("insert after reopen");
        runtime.flush().expect("checkpoint after reopen");
        assert_no_phase1_sidecars(dir.path(), "DML + checkpoint after reopen");
    }
    assert_no_phase1_sidecars(dir.path(), "final close");
}

#[test]
fn the_promoted_embedded_profile_keeps_everything_in_one_file() {
    // The census above proves the phase-1 family is gone. This pins the wider
    // promise for the profile the ADR binds: one operator-visible artifact.
    let dir = temp_dir("single_file");
    let path = dir.path().join("data.rdb");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        runtime
            .execute_query("CREATE TABLE t (id INT)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO t (id) VALUES (1)")
            .expect("insert row");
        runtime.flush().expect("checkpoint");
    }

    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
        .collect();
    names.sort();
    assert_eq!(names, vec!["data.rdb".to_string()]);
}
