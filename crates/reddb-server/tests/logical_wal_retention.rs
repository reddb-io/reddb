//! A replica must be able to replay every acknowledged record above its retention pin.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use reddb_server::replication::cdc::{change_record_from_entity, ChangeOperation, ChangeRecord};
use reddb_server::replication::logical::{ApplyMode, LogicalChangeApplier};
use reddb_server::replication::primary::LogicalWalSpool;
use reddb_server::storage::{EntityId, UnifiedEntity};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

const CHILD: &str = "REDDB_RETENTION_REPLAY_CHILD";
const TEST: &str = "concurrent_retention_keeps_acknowledged_rows_replayable_after_restart";

fn payload(lsn: u64) -> Vec<u8> {
    let entity = UnifiedEntity::table_row(
        EntityId::new(lsn),
        "retained_rows",
        lsn,
        vec![
            Value::Integer(lsn as i64),
            Value::text(format!("ack-{lsn}")),
        ],
    );
    change_record_from_entity(
        lsn,
        lsn,
        ChangeOperation::Insert,
        "retained_rows",
        "row",
        &entity,
        reddb_server::api::REDDB_FORMAT_VERSION,
        None,
    )
    .encode()
}

fn retention_replay_journey(directory: &Path) {
    let payloads: Vec<_> = (0..=193).map(payload).collect();
    let mut recovered_records = Vec::new();
    for round in 0..4 {
        eprintln!("retention replay: concurrent round {round}");
        let path = directory.join(format!("primary-{round}.rdb"));
        let spool = Arc::new(LogicalWalSpool::open(&path).expect("durable spool"));
        for lsn in 1..=128 {
            spool.append(lsn, &payloads[lsn as usize]).expect("seed");
        }
        let start = Barrier::new(3);
        let acknowledged = AtomicU64::new(128);
        std::thread::scope(|threads| {
            let writer = &spool;
            let start = &start;
            let acknowledged = &acknowledged;
            let payloads = &payloads;
            let append = threads.spawn(move || {
                start.wait();
                for lsn in 129..=192 {
                    writer
                        .append(lsn, &payloads[lsn as usize])
                        .expect("acknowledged append");
                    acknowledged.store(lsn, Ordering::Release);
                }
            });
            let reader = &spool;
            let pull = threads.spawn(move || {
                start.wait();
                for _ in 0..64 {
                    let durable = acknowledged.load(Ordering::Acquire);
                    let records = reader.read_since(63, 512).expect("indexed replica pull");
                    assert!(records.len() >= (durable - 63) as usize);
                    for (offset, (lsn, bytes)) in records.iter().enumerate() {
                        assert_eq!(*lsn, 64 + offset as u64, "contiguous replica prefix");
                        assert_eq!(bytes, &payloads[*lsn as usize]);
                    }
                    let bounded = reader
                        .read_since_bounded(95, 8, usize::MAX)
                        .expect("bounded indexed replica pull");
                    assert_eq!(bounded.len(), 8);
                    for (offset, (lsn, bytes)) in bounded.iter().enumerate() {
                        assert_eq!(*lsn, 96 + offset as u64);
                        assert_eq!(bytes, &payloads[*lsn as usize]);
                    }
                    assert!(matches!(
                        reader.oldest_lsn().expect("oldest record"),
                        Some(1 | 2)
                    ));
                }
            });
            start.wait();
            for _ in 0..16 {
                spool
                    .prune_through(1)
                    .expect("retain everything above the replica pin");
            }
            append.join().expect("append progress");
            pull.join().expect("replica pull progress");
        });
        assert_eq!(spool.current_lsn(), 192);
        drop(spool);

        let cold = LogicalWalSpool::open(&path).expect("cold spool recovery");
        let retained = cold.read_since(1, 512).expect("cold retained stream");
        assert_eq!(
            retained.iter().map(|(lsn, _)| *lsn).collect::<Vec<_>>(),
            (2..=192).collect::<Vec<_>>(),
            "every acknowledged record above the pin must survive"
        );
        for (lsn, bytes) in &retained {
            assert_eq!(bytes, &payloads[*lsn as usize]);
        }
        cold.append(193, &payloads[193])
            .expect("write after recovery");
        drop(cold);
        let cold = LogicalWalSpool::open(&path).expect("second cold spool recovery");
        assert_eq!(cold.current_lsn(), 193);
        assert_eq!(cold.oldest_lsn().expect("retained floor"), Some(2));
        assert_eq!(
            cold.read_since(192, 8).expect("new record"),
            vec![(193, payloads[193].clone())]
        );
        recovered_records = cold.read_since(1, 512).expect("replica catch-up stream");
    }

    eprintln!("retention replay: checkpointing replica at its retention pin");
    let replica_path = directory.join("replica.rdb");
    let options = RedDBOptions::persistent(&replica_path);
    {
        let replica = RedDBRuntime::with_options(options.clone()).expect("replica");
        replica
            .execute_query("CREATE TABLE retained_rows (id INT PRIMARY KEY, label TEXT)")
            .expect("replicated schema");
        LogicalChangeApplier::new(0)
            .apply(
                replica.db().as_ref(),
                &ChangeRecord::decode(&payloads[1]).expect("checkpoint row"),
                ApplyMode::Replica,
            )
            .expect("apply checkpoint row");
        replica.flush().expect("durable replica checkpoint");
    }
    {
        let replica = RedDBRuntime::with_options(options.clone()).expect("restart replica");
        let applier = LogicalChangeApplier::new(1);
        for (lsn, bytes) in recovered_records {
            let record = ChangeRecord::decode(&bytes).expect("decode retained record");
            assert_eq!(record.lsn, lsn);
            applier
                .apply(replica.db().as_ref(), &record, ApplyMode::Replica)
                .expect("catch up");
        }
        assert_eq!(applier.last_applied_lsn(), 193);
        replica.flush().expect("checkpoint after catch-up");
    }
    eprintln!("retention replay: checking cold index reads before scan");
    let replica = RedDBRuntime::with_options(options).expect("cold replica reopen");
    for id in [1, 2, 64, 128, 192, 193] {
        let result = replica
            .execute_query(&format!("SELECT label FROM retained_rows WHERE id = {id}"))
            .expect("cold indexed row");
        assert_eq!(result.result.records.len(), 1, "acknowledged row {id}");
        assert_eq!(
            result.result.records[0]
                .get("label")
                .expect("label")
                .display_string(),
            format!("ack-{id}")
        );
    }
    let rows = replica
        .execute_query("SELECT id, label FROM retained_rows ORDER BY id")
        .expect("authoritative replica scan")
        .result
        .records;
    assert_eq!(rows.len(), 193);
    for (offset, row) in rows.iter().enumerate() {
        let id = offset + 1;
        assert_eq!(row.get("id").expect("id").display_string(), id.to_string());
        assert_eq!(
            row.get("label").expect("label").display_string(),
            format!("ack-{id}")
        );
    }
}

#[test]
fn concurrent_retention_keeps_acknowledged_rows_replayable_after_restart() {
    if let Some(directory) = std::env::var_os(CHILD) {
        retention_replay_journey(Path::new(&directory));
        return;
    }
    let directory = tempfile::tempdir().expect("isolated retention journey");
    let stdout = directory.path().join("stdout.log");
    let stderr = directory.path().join("stderr.log");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, directory.path())
        .stdout(std::fs::File::create(&stdout).expect("child stdout"))
        .stderr(std::fs::File::create(&stderr).expect("child stderr"))
        .spawn()
        .expect("retention child");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().expect("stop stalled journey");
            child.wait().expect("join stopped child");
            panic!(
                "retention journey exceeded 60s: {}",
                std::fs::read_to_string(&stderr).expect("child phases")
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "{}\n{}",
        std::fs::read_to_string(&stdout).expect("stdout"),
        std::fs::read_to_string(&stderr).expect("stderr")
    );
}
