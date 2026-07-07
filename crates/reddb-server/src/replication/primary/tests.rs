use super::*;
use crate::replication::cdc::{ChangeOperation, ChangeRecord};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_data_path(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{name}_{suffix}.rdb"))
}

#[test]
fn logical_wal_spool_roundtrip_and_prune() {
    let data_path = temp_data_path("logical_spool");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let spool = LogicalWalSpool::open(&data_path).expect("open spool");

    let record1 = ChangeRecord {
        term: 2,
        lsn: 7,
        timestamp: 1,
        operation: ChangeOperation::Insert,
        collection: "users".to_string(),
        entity_id: 10,
        entity_kind: "row".to_string(),
        entity_bytes: Some(vec![1, 2, 3]),
        metadata: None,
        refresh_records: None,
        range_id: None,
        ownership_epoch: None,
    };
    let record2 = ChangeRecord {
        term: 2,
        lsn: 8,
        timestamp: 2,
        operation: ChangeOperation::Update,
        collection: "users".to_string(),
        entity_id: 10,
        entity_kind: "row".to_string(),
        entity_bytes: Some(vec![4, 5, 6]),
        metadata: None,
        refresh_records: None,
        range_id: None,
        ownership_epoch: None,
    };

    spool
        .append_with_term_and_timestamp(record1.term, record1.lsn, 11, &record1.encode())
        .expect("append 1");
    spool
        .append_with_term_and_timestamp(record2.term, record2.lsn, 12, &record2.encode())
        .expect("append 2");

    let entries = spool.read_since(0, usize::MAX).expect("read");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, 7);
    assert_eq!(entries[1].0, 8);
    assert_eq!(ChangeRecord::decode(&entries[0].1).unwrap().term, 2);

    let framed =
        reddb_file::read_and_repair_logical_wal_entries(&spool_path).expect("read framed entries");
    assert_eq!(framed[0].term, 2);
    assert_eq!(framed[0].timestamp_ms, 11);

    spool.prune_through(7).expect("prune");
    let retained = spool.read_since(0, usize::MAX).expect("read retained");
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].0, 8);
    assert_eq!(ChangeRecord::decode(&retained[0].1).unwrap().term, 2);

    let _ = fs::remove_file(spool_path);
}

// Issue #991 — a logical record stamped with range identity and ownership
// epoch must survive derivation through the logical-WAL spool: the primary
// appends the encoded ChangeRecord, and a replica/recovery read decodes the
// range authority unchanged. The binary v3 envelope carries the payload
// opaquely, so no format bump is needed for the range metadata to flow.
#[test]
fn logical_wal_spool_preserves_range_authority() {
    let data_path = temp_data_path("logical_spool_range");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let spool = LogicalWalSpool::open(&data_path).expect("open spool");

    let record = ChangeRecord {
        term: 5,
        lsn: 21,
        timestamp: 7,
        operation: ChangeOperation::Insert,
        collection: "orders".to_string(),
        entity_id: 99,
        entity_kind: "row".to_string(),
        entity_bytes: Some(vec![7, 7, 7]),
        metadata: None,
        refresh_records: None,
        range_id: None,
        ownership_epoch: None,
    }
    .with_range_authority(13, 4);

    spool
        .append_with_term_and_timestamp(record.term, record.lsn, 7, &record.encode())
        .expect("append stamped record");

    let entries = spool.read_since(0, usize::MAX).expect("read");
    assert_eq!(entries.len(), 1);
    let derived = ChangeRecord::decode(&entries[0].1).expect("decode derived record");
    assert_eq!(derived.range_id, Some(13));
    assert_eq!(derived.ownership_epoch, Some(4));
    assert_eq!(derived.term, 5);

    let framed =
        reddb_file::read_and_repair_logical_wal_entries(&spool_path).expect("read framed entries");
    assert_eq!(framed[0].term, 5);
    assert_eq!(framed[0].ownership_epoch, Some(4));
    assert_eq!(framed[0].version, reddb_file::LOGICAL_WAL_SPOOL_VERSION_V3);

    let _ = fs::remove_file(spool_path);
}

#[test]
fn logical_wal_spool_reads_v2_without_term() {
    let data_path = temp_data_path("logical_spool_v2");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let payload =
        br#"{"lsn":3,"timestamp":44,"operation":"delete","collection":"users","rid":9,"kind":"row"}"#;
    let lsn = 3u64;
    let timestamp = 44u64;
    let frame =
        reddb_file::encode_logical_wal_v2_for_compat(lsn, timestamp, payload).expect("v2 frame");
    fs::write(&spool_path, frame).expect("write v2 spool");

    let spool = LogicalWalSpool::open(&data_path).expect("open v2 spool");
    let records = spool.read_since(0, usize::MAX).expect("read v2 spool");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, 3);
    let decoded = ChangeRecord::decode(&records[0].1).expect("decode v2 payload");
    assert_eq!(decoded.term, crate::replication::DEFAULT_REPLICATION_TERM);
    assert_eq!(decoded.lsn, 3);

    let framed = reddb_file::read_and_repair_logical_wal_entries(&spool_path)
        .expect("read framed v2 entries");
    assert_eq!(framed[0].term, 0);
    assert_eq!(framed[0].ownership_epoch, None);

    let _ = fs::remove_file(spool_path);
}

#[test]
fn topology_epoch_monotonic_on_register_and_unregister() {
    let primary = PrimaryReplication::new(None);
    let e0 = primary.topology_epoch();
    primary.register_replica("r1".to_string());
    let e1 = primary.topology_epoch();
    primary.register_replica("r2".to_string());
    let e2 = primary.topology_epoch();
    assert!(e1 > e0, "register must bump epoch ({e0} -> {e1})");
    assert!(e2 > e1, "second register must bump epoch ({e1} -> {e2})");

    let removed = primary.unregister_replica("r1");
    assert!(removed);
    let e3 = primary.topology_epoch();
    assert!(e3 > e2, "unregister must bump epoch ({e2} -> {e3})");

    let absent = primary.unregister_replica("ghost");
    assert!(!absent);
    assert_eq!(primary.topology_epoch(), e3);
}

#[test]
fn register_replica_is_idempotent_on_reconnect() {
    let primary = PrimaryReplication::new(None);

    primary.register_replica("r1".to_string());
    assert_eq!(primary.replica_count(), 1);
    let epoch_after_first = primary.topology_epoch();

    primary.note_replica_pull("r1", 42);
    primary.ack_replica_lsn("r1", 40, 40);
    let before = primary
        .replica_snapshots()
        .into_iter()
        .find(|r| r.id == "r1")
        .expect("r1 present");
    assert_eq!(before.last_sent_lsn, 42);
    assert_eq!(before.last_acked_lsn, 40);
    assert_eq!(before.last_durable_lsn, 40);

    let resume_lsn = primary.register_replica("r1".to_string());

    assert_eq!(primary.replica_count(), 1);
    assert_eq!(primary.topology_epoch(), epoch_after_first);
    let after = primary
        .replica_snapshots()
        .into_iter()
        .find(|r| r.id == "r1")
        .expect("r1 still present");
    assert_eq!(after.last_sent_lsn, 42);
    assert_eq!(after.last_acked_lsn, 40);
    assert_eq!(after.last_durable_lsn, 40);
    assert_eq!(resume_lsn, 40);
}

#[test]
fn replica_slot_persists_and_reconnect_resumes_from_restart_lsn() {
    let data_path = temp_data_path("replication_slots");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let slot_path = PrimaryReplication::slot_path_for(&data_path);
    let slot_catalog_path = PrimaryReplication::slot_catalog_path_for(&data_path);

    {
        let primary = PrimaryReplication::new(Some(&data_path));
        primary.register_replica("r1".to_string());
        primary.note_replica_pull("r1", 12);
        primary.ack_replica_lsn("r1", 10, 8);

        let slot = primary
            .slot_snapshots()
            .into_iter()
            .find(|slot| slot.replica_id == "r1")
            .expect("r1 slot present");
        assert_eq!(slot.restart_lsn, 8);
        assert_eq!(slot.confirmed_lsn(), 10);
    }

    let catalog = reddb_file::ReplicationSlotCatalog::read_from_path(&slot_catalog_path)
        .expect("binary slot catalog");
    let file_slot = catalog
        .slots
        .iter()
        .find(|slot| slot.replica_id == "r1")
        .expect("r1 binary slot");
    assert_eq!(file_slot.restart_lsn, 8);
    assert_eq!(file_slot.confirmed_write_lsn, 10);
    assert_eq!(file_slot.confirmed_flush_lsn, 8);
    assert_eq!(file_slot.confirmed_apply_lsn, 8);
    assert!(file_slot.active);

    let reopened = PrimaryReplication::new(Some(&data_path));
    let slot = reopened
        .slot_snapshots()
        .into_iter()
        .find(|slot| slot.replica_id == "r1")
        .expect("r1 slot loaded after reopen");
    assert_eq!(slot.restart_lsn, 8);
    assert_eq!(slot.confirmed_lsn(), 10);
    assert_eq!(reopened.register_replica("r1".to_string()), 8);

    let _ = fs::remove_file(spool_path);
    let _ = fs::remove_file(slot_path);
    let _ = fs::remove_dir_all(PrimaryReplication::primary_replica_root_for(&data_path));
}

#[test]
fn binary_replication_slot_catalog_can_bootstrap_without_legacy_json() {
    let data_path = temp_data_path("replication_slots_binary_only");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let slot_path = PrimaryReplication::slot_path_for(&data_path);

    {
        let primary = PrimaryReplication::new(Some(&data_path));
        primary.register_replica("r1".to_string());
        primary.ack_replica_lsn("r1", 17, 13);
    }
    fs::remove_file(&slot_path).expect("remove legacy json slot file");

    let reopened = PrimaryReplication::new(Some(&data_path));
    let slot = reopened
        .slot_snapshots()
        .into_iter()
        .find(|slot| slot.replica_id == "r1")
        .expect("r1 slot loaded from binary catalog");
    assert_eq!(slot.restart_lsn, 13);
    assert_eq!(slot.confirmed_lsn(), 17);
    assert_eq!(reopened.register_replica("r1".to_string()), 13);

    let _ = fs::remove_file(spool_path);
    let _ = fs::remove_dir_all(PrimaryReplication::primary_replica_root_for(&data_path));
}

#[test]
fn retention_floor_follows_slowest_slot_and_prunes_wal() {
    let primary = PrimaryReplication::new(None);
    primary.register_replica("fast".to_string());
    primary.register_replica("slow".to_string());

    for lsn in 1..=6 {
        primary.wal_buffer.append(lsn, vec![lsn as u8]);
    }

    primary.ack_replica_lsn("fast", 5, 5);
    primary.ack_replica_lsn("slow", 3, 2);

    assert_eq!(primary.retention_floor_lsn(), Some(2));
    assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 2);
    let retained: Vec<_> = primary
        .wal_buffer
        .read_since(0, usize::MAX)
        .into_iter()
        .map(|(lsn, _)| lsn)
        .collect();
    assert_eq!(retained, vec![3, 4, 5, 6]);

    primary.ack_replica_lsn("slow", 6, 6);
    assert_eq!(primary.retention_floor_lsn(), Some(5));
    assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 5);
    let retained: Vec<_> = primary
        .wal_buffer
        .read_since(0, usize::MAX)
        .into_iter()
        .map(|(lsn, _)| lsn)
        .collect();
    assert_eq!(retained, vec![6]);
}

#[test]
fn bootstrap_slot_pin_prevents_wal_removed_rebootstrap_after_prune() {
    let primary = PrimaryReplication::new(None);
    for lsn in 1..=5 {
        primary.wal_buffer.append(lsn, vec![lsn as u8]);
    }

    let slot_lsn = primary.register_replica("bootstrapping".to_string());
    assert_eq!(slot_lsn, 5);

    for lsn in 6..=8 {
        primary.wal_buffer.append(lsn, vec![lsn as u8]);
    }

    assert_eq!(primary.prune_retained_wal_through(8).unwrap(), 5);
    assert_eq!(
        primary.slot_rebootstrap_reason("bootstrapping", 0, primary.wal_buffer.oldest_lsn()),
        None
    );
}

#[test]
fn default_config_enables_finite_slot_retention_cap() {
    let config = crate::replication::ReplicationConfig::primary();

    assert!(config.slot_retention_max_lag_lsn > 0);
}

#[test]
fn retention_cap_invalidates_slow_slot_and_releases_wal_floor() {
    let primary = PrimaryReplication::new_with_config(
        None,
        &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
    );
    primary.register_replica("fast".to_string());
    primary.register_replica("slow".to_string());

    for lsn in 1..=6 {
        primary.wal_buffer.append(lsn, vec![lsn as u8]);
    }
    primary.ack_replica_lsn("fast", 6, 6);

    assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 6);

    let slow = primary
        .slot_snapshots()
        .into_iter()
        .find(|slot| slot.replica_id == "slow")
        .expect("slow slot present");
    assert_eq!(
        slow.invalidation_reason,
        Some(ReplicationSlotInvalidationCause::Horizon)
    );

    let retained: Vec<_> = primary
        .wal_buffer
        .read_since(0, usize::MAX)
        .into_iter()
        .map(|(lsn, _)| lsn)
        .collect();
    assert!(retained.is_empty());
}

#[test]
fn slot_invalidation_cause_codes_cover_wal_removed_horizon_and_idle_timeout() {
    let wal_removed = PrimaryReplication::new_with_config(
        None,
        &crate::replication::ReplicationConfig::primary()
            .with_slot_retention_max_lag_lsn(3)
            .with_slot_idle_timeout_ms(10),
    );
    wal_removed.register_replica("wal".to_string());
    assert_eq!(
        wal_removed.slot_rebootstrap_reason("wal", 0, Some(2)),
        Some(ReplicationSlotInvalidationCause::WalRemoved)
    );

    let horizon = PrimaryReplication::new_with_config(
        None,
        &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
    );
    horizon.register_replica("horizon".to_string());
    for lsn in 1..=4 {
        horizon.wal_buffer.append(lsn, vec![lsn as u8]);
    }
    horizon.enforce_retention_limits(0);
    assert_eq!(
        horizon
            .slot_snapshots()
            .into_iter()
            .find(|slot| slot.replica_id == "horizon")
            .and_then(|slot| slot.invalidation_reason),
        Some(ReplicationSlotInvalidationCause::Horizon)
    );

    let idle = PrimaryReplication::new_with_config(
        None,
        &crate::replication::ReplicationConfig::primary().with_slot_idle_timeout_ms(10),
    );
    idle.register_replica("idle".to_string());
    idle.touch_slot("idle", 1);
    idle.enforce_retention_limits(12);
    assert_eq!(
        idle.slot_snapshots()
            .into_iter()
            .find(|slot| slot.replica_id == "idle")
            .and_then(|slot| slot.invalidation_reason),
        Some(ReplicationSlotInvalidationCause::IdleTimeout)
    );
}

#[test]
fn wal_buffer_fan_out_shares_refcounted_payload() {
    let buffer = WalBuffer::new(8);
    buffer.append(1, vec![0xDE, 0xAD, 0xBE, 0xEF]);

    let replica_a = buffer.read_since_shared(0, usize::MAX);
    let replica_b = buffer.read_since_shared(0, usize::MAX);
    assert_eq!(replica_a.len(), 1);
    assert_eq!(replica_b.len(), 1);

    assert!(Arc::ptr_eq(&replica_a[0].1, &replica_b[0].1));
    assert_eq!(&*replica_a[0].1, &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert!(Arc::strong_count(&replica_a[0].1) >= 3);

    let owned = buffer.read_since(0, usize::MAX);
    assert_eq!(owned, vec![(1u64, vec![0xDE, 0xAD, 0xBE, 0xEF])]);
}

#[test]
fn spool_seek_index_resume_is_sublinear() {
    let data_path = temp_data_path("seek_index");
    let spool_path = LogicalWalSpool::path_for(&data_path);
    let spool = LogicalWalSpool::open(&data_path).expect("open spool");

    for lsn in 1..=200u64 {
        spool
            .append_with_term_and_timestamp(1, lsn, lsn, &[(lsn % 251) as u8, 0xAB])
            .expect("append");
    }

    assert_eq!(spool.read_since(0, usize::MAX).expect("full").len(), 200);
    assert_eq!(spool.seek_floor_offset(0), 0);

    let resumed = spool.read_since(130, usize::MAX).expect("resume");
    assert_eq!(resumed.first().map(|(lsn, _)| *lsn), Some(131));
    assert_eq!(resumed.last().map(|(lsn, _)| *lsn), Some(200));
    assert_eq!(resumed.len(), 70);
    assert!(spool.seek_floor_offset(130) > 0);

    drop(spool);
    let reopened = LogicalWalSpool::open(&data_path).expect("reopen spool");
    assert!(reopened.seek_floor_offset(130) > 0);
    assert_eq!(
        reopened
            .read_since(130, usize::MAX)
            .expect("resume reopen")
            .len(),
        70
    );

    let _ = fs::remove_file(spool_path);
}

#[test]
fn plan_replica_resume_partial_within_window_full_past_cap() {
    let within = PrimaryReplication::new(None);
    within.register_replica("blip".to_string());
    for lsn in 1..=5 {
        within.wal_buffer.append(lsn, vec![lsn as u8]);
    }
    let before = within.partial_resync_count();
    match within.plan_replica_resume("blip", 2, within.wal_buffer.oldest_lsn()) {
        ResumeMode::PartialResync { resume_lsn } => assert_eq!(resume_lsn, 2),
        other => panic!("brief blip must resume via partial resync, got {other:?}"),
    }
    assert_eq!(within.partial_resync_count(), before + 1);
    assert_eq!(within.full_resync_count(), 0);

    let past_cap = PrimaryReplication::new_with_config(
        None,
        &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
    );
    past_cap.register_replica("slow".to_string());
    for lsn in 1..=6 {
        past_cap.wal_buffer.append(lsn, vec![lsn as u8]);
    }
    past_cap.enforce_retention_limits(0);
    let before_full = past_cap.partial_resync_count();
    let before_full_count = past_cap.full_resync_count();
    match past_cap.plan_replica_resume("slow", 0, past_cap.wal_buffer.oldest_lsn()) {
        ResumeMode::FullRebootstrap { cause } => {
            assert_eq!(cause, ReplicationSlotInvalidationCause::Horizon)
        }
        other => panic!("slot past the cap must re-bootstrap, got {other:?}"),
    }
    assert_eq!(past_cap.partial_resync_count(), before_full);
    assert_eq!(past_cap.full_resync_count(), before_full_count + 1);
}

#[test]
fn ensure_replica_registered_self_registers_then_is_a_noop() {
    let primary = PrimaryReplication::new(None);

    assert!(primary.ensure_replica_registered("r1"));
    assert_eq!(primary.replica_count(), 1);
    let epoch_after_register = primary.topology_epoch();

    primary.note_replica_pull("r1", 7);
    assert_eq!(
        primary
            .replica_snapshots()
            .into_iter()
            .find(|r| r.id == "r1")
            .map(|r| r.last_sent_lsn),
        Some(7)
    );

    assert!(!primary.ensure_replica_registered("r1"));
    assert_eq!(primary.replica_count(), 1);
    assert_eq!(primary.topology_epoch(), epoch_after_register);
    assert_eq!(
        primary
            .replica_snapshots()
            .into_iter()
            .find(|r| r.id == "r1")
            .map(|r| r.last_sent_lsn),
        Some(7)
    );
}

#[test]
fn replication_progress_uses_sent_applied_and_durable_registry_lsns() {
    let now = crate::utils::now_unix_millis() as u128;
    let replicas = vec![
        ReplicaState {
            id: "fast".to_string(),
            last_acked_lsn: 90,
            last_sent_lsn: 120,
            last_durable_lsn: 80,
            apply_error_count: 0,
            divergence_count: 0,
            connected_at_unix_ms: now,
            last_seen_at_unix_ms: now,
            region: None,
            rebootstrapping: false,
        },
        ReplicaState {
            id: "slow".to_string(),
            last_acked_lsn: 70,
            last_sent_lsn: 100,
            last_durable_lsn: 60,
            apply_error_count: 0,
            divergence_count: 0,
            connected_at_unix_ms: now,
            last_seen_at_unix_ms: now,
            region: None,
            rebootstrapping: false,
        },
    ];

    let progress = ReplicationProgress::from_replicas(&replicas).expect("registered replicas");

    assert_eq!(progress.lag_lsn, 50);
    assert_eq!(progress.safe_replay_lsn, 60);
}
