//! Identical volatile MVCC assertions on native hosts and inside browser workers.
//! These fixtures do not claim durable transaction recovery or stored-data SQL.
use std::collections::HashSet;
use std::sync::Arc;

use reddb_engine::storage::entity_id::EntityId;
use reddb_engine::transaction::{
    is_visible, state::TransactionState, IsolationLevel, SnapshotManager, Xid,
};

fn fallible_version_reads_and_retry() {
    use reddb_engine::storage::entity::UnifiedEntity;
    use reddb_engine::transaction::read::{try_resolve_logical_id, FallibleTableRowVersionStore};
    use std::cell::Cell;

    struct VersionStore {
        rows: Vec<UnifiedEntity>,
        failure_at: Cell<Option<usize>>,
        reads: Cell<usize>,
        point_failure: Cell<bool>,
        points: Cell<usize>,
    }
    impl FallibleTableRowVersionStore for VersionStore {
        type Error = &'static str;
        fn try_get_table_row_by_logical_id(
            &self,
            collection: &str,
            logical_id: EntityId,
        ) -> Result<Option<UnifiedEntity>, Self::Error> {
            assert_eq!(collection, "versions");
            assert_eq!(logical_id, EntityId::new(7));
            self.points.set(self.points.get() + 1);
            if self.point_failure.get() {
                return Err("point read failed");
            }
            Ok(self.rows.iter().find(|row| row.xmax == 0).cloned())
        }
        fn try_for_each_table_row_version(
            &self,
            collection: &str,
            logical_id: EntityId,
            mut visit: impl FnMut(&UnifiedEntity) -> bool,
        ) -> Result<(), Self::Error> {
            assert_eq!(collection, "versions");
            for row in &self.rows {
                assert_eq!(row.logical_id(), logical_id);
                self.reads.set(self.reads.get() + 1);
                if self.failure_at.get() == Some(self.reads.get()) {
                    return Err("version read failed");
                }
                if !visit(row) {
                    break;
                }
            }
            Ok(())
        }
    }
    let rows = [(1, 1, 2), (2, 2, 0), (3, 1, 2)]
        .into_iter()
        .map(|(id, xmin, xmax)| {
            let mut row = UnifiedEntity::table_row(EntityId::new(id), "versions", 7, vec![]);
            row.set_logical_id(EntityId::new(7));
            row.set_xmin(xmin);
            row.set_xmax(xmax);
            row
        })
        .collect();
    let store = VersionStore {
        rows,
        failure_at: Cell::new(None),
        reads: Cell::new(0),
        point_failure: Cell::new(false),
        points: Cell::new(0),
    };
    for failure_at in 1..=3 {
        store.failure_at.set(Some(failure_at));
        store.reads.set(0);
        assert_eq!(
            try_resolve_logical_id(&store, "versions", EntityId::new(7), true, |_| true).err(),
            Some("version read failed")
        );
        assert_eq!(store.reads.get(), failure_at);
        store.failure_at.set(None);
        store.reads.set(0);
        let selected = try_resolve_logical_id(&store, "versions", EntityId::new(7), true, |_| true)
            .expect("version retry")
            .expect("visible version");
        assert_eq!(selected.id, EntityId::new(2));
        assert_eq!(store.reads.get(), 3);
    }
    let old = try_resolve_logical_id(&store, "versions", EntityId::new(7), true, |row| {
        row.xmin < 2
    })
    .expect("older snapshot")
    .expect("older visible version");
    assert_eq!(
        old.id,
        EntityId::new(3),
        "equal xmin keeps the last visible candidate"
    );
    store.point_failure.set(true);
    store.reads.set(0);
    assert_eq!(
        try_resolve_logical_id(&store, "versions", EntityId::new(7), false, |_| true).err(),
        Some("point read failed")
    );
    store.point_failure.set(false);
    let current = try_resolve_logical_id(&store, "versions", EntityId::new(7), false, |_| true)
        .expect("point retry")
        .expect("current version");
    assert_eq!(current.id, EntityId::new(2));
    assert_eq!(store.reads.get(), 0);
    assert_eq!(store.points.get(), 2);
}

fn snapshot_isolation_and_read_committed() {
    let state = TransactionState::new();
    let manager = state.snapshot_manager();
    let old = manager.begin();
    manager.commit(old);
    let writer = state.begin(1, IsolationLevel::SnapshotIsolation);
    state.begin(2, IsolationLevel::SnapshotIsolation);
    state.begin(3, IsolationLevel::ReadCommitted);
    for reader in [2, 3] {
        let snapshot = state.current_snapshot(reader);
        assert!(!snapshot.sees(writer, 0), "active creator stays hidden");
        assert!(snapshot.sees(old, writer), "active delete stays hidden");
    }
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("prepare succeeds");
    assert!(!state.current_snapshot(2).sees(writer, 0));
    assert!(state.current_snapshot(2).sees(old, writer));
    assert!(state.current_snapshot(3).sees(writer, 0));
    assert!(!state.current_snapshot(3).sees(old, writer));

    let aborted = manager.begin();
    manager.rollback(aborted);
    let snapshot = state.current_snapshot(3);
    let outcomes = HashSet::from([aborted]);
    assert!(!is_visible(
        aborted,
        0,
        snapshot.xid,
        &snapshot.in_progress,
        &outcomes
    ));
    assert!(is_visible(
        old,
        aborted,
        snapshot.xid,
        &snapshot.in_progress,
        &outcomes
    ));
    state.rollback(2).expect("snapshot reader exists");
    state.rollback(3).expect("read committed reader exists");
}

fn failed_prepare_aborts_all_writers() {
    let state = TransactionState::new();
    let parent = state.begin(1, IsolationLevel::SnapshotIsolation);
    let released = state.savepoint(1, "released").expect("active transaction");
    assert_eq!(
        state
            .release_savepoint(1, "released")
            .expect("savepoint exists"),
        Some(1)
    );
    let open = state.savepoint(1, "open").expect("active transaction");
    state.set_local_tenant(1, Some("tenant-a".into()));
    let manager = state.snapshot_manager();
    let (revision, excluded) = manager.checkpoint_excluded_state();
    assert_eq!(excluded, HashSet::from([parent, released, open]));
    assert_eq!(
        state.commit(1, |_| Err::<(), _>("WAL append failed")).err(),
        Some("WAL append failed")
    );
    assert!(!state.in_transaction(1));
    assert_eq!(state.local_tenant(1), None);
    for xid in [parent, released, open] {
        assert!(!manager.is_active(xid));
        assert!(manager.is_aborted(xid));
    }
    let (after, excluded) = manager.checkpoint_excluded_state();
    assert!(after > revision);
    assert_eq!(excluded, HashSet::from([parent, released, open]));
    let snapshot = manager.fresh_read_snapshot();
    for xid in excluded.iter().copied() {
        assert!(!is_visible(
            xid,
            0,
            snapshot.xid,
            &snapshot.in_progress,
            &excluded
        ));
    }
}

fn publication_guard_covers_visibility() {
    struct Publication {
        manager: Arc<SnapshotManager>,
        xids: HashSet<Xid>,
        released: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Drop for Publication {
        fn drop(&mut self) {
            for xid in &self.xids {
                assert!(
                    !self.manager.is_active(*xid),
                    "guard outlives child and parent visibility"
                );
                assert!(!self.manager.is_aborted(*xid));
            }
            self.released
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let state = TransactionState::new();
    let parent = state.begin(1, IsolationLevel::SnapshotIsolation);
    let released_child = state.savepoint(1, "released").expect("active transaction");
    state
        .release_savepoint(1, "released")
        .expect("savepoint exists");
    let child = state.savepoint(1, "open").expect("active transaction");
    let xids = HashSet::from([parent, released_child, child]);
    assert_eq!(state.own_xids(1), xids);
    let manager = state.snapshot_manager();
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    state
        .commit(1, |context| {
            assert_eq!(context.xid, parent);
            for xid in &xids {
                assert!(manager.is_active(*xid), "prepare precedes visibility");
            }
            Ok::<_, ()>(Publication {
                manager,
                xids,
                released: Arc::clone(&released),
            })
        })
        .expect("prepare succeeds")
        .expect("transaction exists");
    assert!(released.load(std::sync::atomic::Ordering::Acquire));
}

fn savepoints_and_connection_isolation() {
    let state = TransactionState::new();
    let parent = state.begin(1, IsolationLevel::SnapshotIsolation);
    let sibling = state.begin(2, IsolationLevel::ReadCommitted);
    state.set_local_tenant(1, Some("tenant-a".into()));
    state.set_local_tenant(2, Some("tenant-b".into()));
    let outer = state.savepoint(1, "outer").expect("active transaction");
    let inner = state.savepoint(1, "inner").expect("active transaction");
    assert_eq!(state.writer_xid(1), Some(inner));
    assert_eq!(state.writer_xid(2), Some(sibling));
    assert!(state.has_savepoint(1, "outer"));
    let error = state
        .release_savepoint(1, "missing")
        .expect_err("missing savepoint");
    assert_eq!(error.to_string(), "savepoint missing does not exist");
    assert_eq!(
        state.writer_xid(1),
        Some(inner),
        "failed release preserves stack"
    );
    let undo = state
        .rollback_to_savepoint(1, "outer")
        .expect("savepoint exists")
        .expect("active transaction");
    assert_eq!(undo.savepoint_xid, outer);
    assert_eq!(undo.aborted_xids, vec![outer, inner]);
    assert_eq!(state.writer_xid(1), Some(parent));
    assert_eq!(state.own_xids(1), HashSet::from([parent]));
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("prepare succeeds");
    assert!(state.snapshot_manager().is_aborted(outer));
    assert!(state.snapshot_manager().is_aborted(inner));
    assert_eq!(state.local_tenant(1), None);
    assert_eq!(state.local_tenant(2).flatten().as_deref(), Some("tenant-b"));
    assert!(state.in_transaction(2));
    state.rollback(2).expect("sibling exists");
    assert_eq!(state.local_tenant(2), None);
    assert_eq!(
        state
            .release_savepoint(1, "absent")
            .expect("outside transaction is a no-op"),
        None
    );
    assert!(state
        .rollback_to_savepoint(1, "absent")
        .expect("outside transaction is a no-op")
        .is_none());
}

fn retention_and_checkpoint_outcomes() {
    let state = TransactionState::new();
    let manager = state.snapshot_manager();
    let writer = state.begin(1, IsolationLevel::SnapshotIsolation);
    let reader = state.begin(2, IsolationLevel::SnapshotIsolation);
    let (snapshot, guard) = state.retained_current_snapshot(2);
    let (_, nested) = manager.retain_snapshot(snapshot.clone());
    assert!(snapshot.in_progress.contains(&writer));
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("writer commits");
    state.rollback(2).expect("reader exits");
    assert_eq!(manager.oldest_active_xid(), None);
    assert_eq!(manager.oldest_required_xid(), Some(writer));
    drop(guard);
    assert_eq!(
        manager.oldest_required_xid(),
        Some(writer),
        "nested frame retains history"
    );
    drop(nested);
    assert_eq!(manager.oldest_required_xid(), None);
    let (_, excluded, untracked) = manager.checkpoint_projection_state();
    assert_eq!(excluded, HashSet::from([reader]));
    assert_eq!(untracked, 0);
    manager.pin(reader);
    manager.pin(reader);
    manager.prune_aborted(reader + 1);
    assert!(manager.is_aborted(reader));
    assert_eq!(manager.oldest_required_xid(), Some(reader));
    manager.unpin(reader);
    assert_eq!(manager.pin_count(reader), 1);
    manager.unpin(reader);
    assert_eq!(manager.oldest_required_xid(), None);
    manager.prune_aborted(reader + 1);
    assert!(!manager.is_aborted(reader));
    let (_snapshot, statement) = manager.fresh_retained_snapshot();
    assert!(manager.oldest_required_xid().is_some());
    drop(statement);
    assert_eq!(manager.oldest_required_xid(), None);
    manager.rollback(writer);
    assert_eq!(manager.checkpoint_projection_state().2, untracked + 1);
}

fn unique_keys_follow_outcomes() {
    let manager = SnapshotManager::new();
    let creator = manager.begin();
    let none = HashSet::new();
    assert!(manager.row_reserves_unique_key(creator, 0, &none));
    manager.rollback(creator);
    assert!(!manager.row_reserves_unique_key(creator, 0, &none));
    let creator = manager.begin();
    manager.commit(creator);
    let deleter = manager.begin();
    assert!(manager.row_reserves_unique_key(creator, deleter, &none));
    assert!(!manager.row_reserves_unique_key(creator, deleter, &HashSet::from([deleter])));
    manager.rollback(deleter);
    assert!(manager.row_reserves_unique_key(creator, deleter, &none));
    let deleter = manager.begin();
    manager.commit(deleter);
    assert!(!manager.row_reserves_unique_key(creator, deleter, &none));
}

fn serializable_write_skew() {
    let state = TransactionState::new();
    let manager = state.snapshot_manager();
    let first = state.begin(1, IsolationLevel::Serializable);
    let second = state.begin(2, IsolationLevel::Serializable);
    let row_a = EntityId::new(1024);
    let row_b = EntityId::new(1025);
    manager.record_serializable_read(first, "doctors", row_b);
    manager.record_serializable_read(second, "doctors", row_a);
    assert!(!manager.serializable_commit_would_be_dangerous(
        first,
        &HashSet::from([("doctors".into(), row_a)])
    ));
    state.commit(1, |_| Ok::<_, ()>(())).expect("first commits");
    assert!(manager.serializable_commit_would_be_dangerous(
        second,
        &HashSet::from([("doctors".into(), row_b)])
    ));
    state.rollback(2).expect("second must abort");
    assert!(manager.is_aborted(second));
}

fn serializable_vault_predicates() {
    // Both absent-key reads and listing predicates must detect the skew.
    for key in [Some("b"), None] {
        let state = TransactionState::new();
        let manager = state.snapshot_manager();
        let first = state.begin(1, IsolationLevel::Serializable);
        let second = state.begin(2, IsolationLevel::Serializable);
        manager.record_serializable_vault_read(first, "settings", Some("tenant-a"), key);
        manager.record_serializable_vault_read(second, "settings", Some("tenant-a"), Some("a"));
        assert!(!manager.serializable_vault_commit_would_be_dangerous(
            first,
            &HashSet::from([("settings".into(), Some("tenant-a".into()), "a".into())])
        ));
        state.commit(1, |_| Ok::<_, ()>(())).expect("first commits");
        assert!(
            !manager.serializable_vault_commit_would_be_dangerous(
                second,
                &HashSet::from([("settings".into(), Some("tenant-b".into()), "b".into())])
            ),
            "another tenant is not the predicate read"
        );
        assert!(manager.serializable_vault_commit_would_be_dangerous(
            second,
            &HashSet::from([("settings".into(), Some("tenant-a".into()), "b".into())])
        ));
        state.rollback(2).expect("second aborts");
    }
}

fn autocommit_reservations_and_replay() {
    let manager = SnapshotManager::new();
    let first = manager.allocate_committed_xid();
    let reader = manager.begin();
    let captured = manager.snapshot(reader);
    let later = manager.allocate_committed_xid();
    assert!(
        later < reader,
        "exercise an unused reserved xid below the reader"
    );
    assert!(captured.sees(first, 0));
    assert!(!captured.sees(later, 0));
    assert!(manager.fresh_read_snapshot().sees(later, 0));
    let recovered = later + 1;
    manager.observe_committed_xid(recovered);
    assert!(!captured.sees(recovered, 0));
    assert!(manager.fresh_read_snapshot().sees(recovered, 0));
    assert!(manager.allocate_committed_xid() > recovered);
    manager.rollback(reader);
}

pub const CASES: &[(&str, fn())] = &[
    (
        "mvcc_fallible_version_reads_and_retry",
        fallible_version_reads_and_retry,
    ),
    (
        "mvcc_snapshot_isolation_and_read_committed",
        snapshot_isolation_and_read_committed,
    ),
    (
        "mvcc_failed_prepare_aborts_all_writers",
        failed_prepare_aborts_all_writers,
    ),
    (
        "mvcc_publication_guard_covers_visibility",
        publication_guard_covers_visibility,
    ),
    (
        "mvcc_savepoints_and_connection_isolation",
        savepoints_and_connection_isolation,
    ),
    (
        "mvcc_retention_and_checkpoint_outcomes",
        retention_and_checkpoint_outcomes,
    ),
    (
        "mvcc_unique_keys_follow_outcomes",
        unique_keys_follow_outcomes,
    ),
    ("mvcc_serializable_write_skew", serializable_write_skew),
    (
        "mvcc_serializable_vault_predicates",
        serializable_vault_predicates,
    ),
    (
        "mvcc_autocommit_reservations_and_replay",
        autocommit_reservations_and_replay,
    ),
];
