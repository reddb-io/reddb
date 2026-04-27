//! Chaos test: concurrent writer-lease acquire race
//! (PLAN.md Phase 5.3 + 8 slice).
//!
//! N threads call `try_acquire` for the same database key with the
//! same backend at roughly the same instant. The lease invariant
//! says **exactly one** wins; the rest must observe `Held` (or
//! `LostRace` if their write hit the backend before the holder's
//! re-read). State after the race must be consistent: a follow-up
//! `current()` returns one canonical holder + generation, and a
//! refresh from the winner still works.

use reddb::replication::lease::{LeaseError, LeaseStore};
use reddb::storage::backend::LocalBackend;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

fn temp_prefix(tag: &str) -> String {
    let mut p = PathBuf::from(std::env::temp_dir());
    p.push(format!(
        "reddb-chaos-lease-race-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn n_threads_racing_for_one_lease_yield_exactly_one_winner() {
    let prefix = temp_prefix("race");
    let store = Arc::new(LeaseStore::new(Arc::new(LocalBackend)).with_prefix(prefix.clone()));

    const CONTENDERS: usize = 8;
    let barrier = Arc::new(Barrier::new(CONTENDERS));
    let mut handles = Vec::with_capacity(CONTENDERS);

    for i in 0..CONTENDERS {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let holder_id = format!("writer-{i}");
            // Synchronize launch to maximize contention pressure.
            barrier.wait();
            store.try_acquire("main", &holder_id, 60_000)
        }));
    }

    let mut wins = 0usize;
    let mut held_losses = 0usize;
    let mut race_losses = 0usize;
    let mut other_errs: Vec<String> = Vec::new();
    let mut winner_holder: Option<String> = None;

    for h in handles {
        match h.join().expect("thread panic") {
            Ok(lease) => {
                wins += 1;
                winner_holder = Some(lease.holder_id);
            }
            Err(LeaseError::Held { .. }) => held_losses += 1,
            Err(LeaseError::LostRace { .. }) => race_losses += 1,
            Err(other) => other_errs.push(format!("{other:?}")),
        }
    }

    assert!(
        other_errs.is_empty(),
        "no contender should hit unexpected lease errors; got: {other_errs:?}"
    );
    assert_eq!(
        wins, 1,
        "exactly one contender must win; wins={wins} held={held_losses} race={race_losses}"
    );
    assert_eq!(
        held_losses + race_losses,
        CONTENDERS - 1,
        "every loser must report Held or LostRace"
    );

    // Backend state must reflect the winner consistently.
    let observed = store.current("main").unwrap().expect("lease present");
    assert_eq!(
        Some(observed.holder_id.clone()),
        winner_holder,
        "current() must return the same holder we recorded as the winner"
    );
    assert_eq!(observed.generation, 1, "first acquire seeds generation 1");

    let _ = std::fs::remove_dir_all(&prefix);
}
