//! An autocommit UPDATE picks its target rows from a snapshot, then applies
//! its assignments later. A peer can commit in between; the UPDATE must act on
//! the row's current version, not fork it (#2373).

use super::index_store::MutationTestPhase;
use crate::RedDBRuntime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const TABLE: &str = "cas_rows";
const TIMEOUT: Duration = Duration::from_secs(10);

fn runtime_with_row() -> RedDBRuntime {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    runtime
        .execute_query("CREATE TABLE cas_rows (id TEXT, v INTEGER)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO cas_rows (id, v) VALUES ('row1', 0)")
        .expect("seed");
    runtime
}

fn live_values(runtime: &RedDBRuntime) -> Vec<i64> {
    runtime
        .execute_query("SELECT v FROM cas_rows WHERE id = 'row1'")
        .expect("read")
        .result
        .records
        .iter()
        .map(|record| match record.get("v") {
            Some(reddb_types::Value::Integer(v)) => *v,
            other => panic!("unexpected v: {other:?}"),
        })
        .collect()
}

/// Run `paused` up to the point it has chosen its targets, run `rival` to
/// completion, then let `paused` continue. Returns `(paused, rival)` affected rows.
fn interleave(runtime: &RedDBRuntime, paused: &str, rival: &str) -> (u64, u64) {
    let armed = Arc::new(AtomicBool::new(true));
    let (scanned_send, scanned_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel::<()>();
    let release_receive = Mutex::new(release_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        // Only the first UPDATE to reach the hook parks; the rival passes.
        if collection == TABLE
            && phase == MutationTestPhase::TargetsScanned
            && armed.swap(false, Ordering::SeqCst)
        {
            scanned_send.send(()).expect("scanned");
            release_receive
                .lock()
                .expect("release receiver")
                .recv_timeout(TIMEOUT)
                .expect("resume the paused update");
        }
    }));
    std::thread::scope(|scope| {
        let paused = scope.spawn(|| runtime.execute_query(paused).expect("paused update"));
        scanned_receive
            .recv_timeout(TIMEOUT)
            .expect("the first update chose its targets");
        let rival = runtime.execute_query(rival).expect("rival update");
        release_send.send(()).expect("resume");
        let paused = paused.join().expect("paused update finishes");
        (paused.affected_rows, rival.affected_rows)
    })
}

#[test]
fn a_conditional_update_is_compare_and_set_against_a_stale_scan() {
    let runtime = runtime_with_row();
    let (paused, rival) = interleave(
        &runtime,
        "UPDATE cas_rows SET v = 1 WHERE id = 'row1' AND v = 0",
        "UPDATE cas_rows SET v = 2 WHERE id = 'row1' AND v = 0",
    );
    assert_eq!(rival, 1, "the rival saw v = 0 and won");
    assert_eq!(
        paused, 0,
        "the paused update chose the row while v was 0; v is 2 now, so it must not apply"
    );
    assert_eq!(
        live_values(&runtime),
        vec![2],
        "one live version, the winner's"
    );
}

#[test]
fn an_unconditional_update_applies_to_the_current_version_without_forking() {
    let runtime = runtime_with_row();
    let (paused, rival) = interleave(
        &runtime,
        "UPDATE cas_rows SET v = 1 WHERE id = 'row1'",
        "UPDATE cas_rows SET v = 2 WHERE id = 'row1'",
    );
    assert_eq!((paused, rival), (1, 1));
    assert_eq!(
        live_values(&runtime),
        vec![1],
        "the paused update ran last, on the rival's version: one live version"
    );
}

#[test]
fn a_stale_update_does_not_resurrect_a_deleted_row() {
    let runtime = runtime_with_row();
    let (paused, rival) = interleave(
        &runtime,
        "UPDATE cas_rows SET v = 1 WHERE id = 'row1'",
        "DELETE FROM cas_rows WHERE id = 'row1'",
    );
    assert_eq!(rival, 1);
    assert_eq!(paused, 0, "the row is gone; nothing to update");
    assert!(live_values(&runtime).is_empty(), "the delete must stick");
}
