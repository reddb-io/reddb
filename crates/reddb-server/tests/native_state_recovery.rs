//! Native identities and queue ownership survive reopen and offline restore.
//! ADR 0020 retains unexpired locks and reclaims expired deliveries lazily.

use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::path::Path;

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .expect("persistent synthetic runtime")
}

fn query(
    runtime: &RedDBRuntime,
    sql: &str,
) -> reddb_server::storage::query::unified::UnifiedResult {
    runtime
        .execute_query(sql)
        .expect("native queue query")
        .result
}

fn message_id(
    result: &reddb_server::storage::query::unified::UnifiedResult,
    index: usize,
) -> String {
    match result.records[index].get("message_id") {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected message identity, got {other:?}"),
    }
}

fn pending(runtime: &RedDBRuntime) -> Vec<[Option<Value>; 4]> {
    query(runtime, "QUEUE PENDING restore_jobs GROUP workers")
        .records
        .iter()
        .map(|record| {
            [
                record.get("message_id").cloned(),
                record.get("consumer").cloned(),
                record.get("delivered_at_ns").cloned(),
                record.get("delivery_count").cloned(),
            ]
        })
        .collect()
}

fn copy_directory(source: &Path, destination: &Path) {
    std::fs::create_dir(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "synthetic store cannot contain links");
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            assert!(kind.is_file());
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn unexpired_pending_delivery_and_ack_survive_reopen_and_offline_restore() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let path = source.join("data.rdb");
    let expected;
    let pending_id;
    {
        let runtime = open(&path);
        query(
            &runtime,
            "CREATE QUEUE restore_jobs LOCK_DEADLINE_MS 600000",
        );
        query(&runtime, "QUEUE GROUP CREATE restore_jobs workers");
        for payload in ["acked", "pending", "waiting"] {
            query(&runtime, &format!("QUEUE PUSH restore_jobs '{payload}'"));
        }
        let delivered = query(
            &runtime,
            "QUEUE READ restore_jobs GROUP workers CONSUMER original COUNT 2",
        );
        assert_eq!(delivered.len(), 2);
        query(
            &runtime,
            &format!(
                "QUEUE ACK restore_jobs GROUP workers '{}'",
                message_id(&delivered, 0)
            ),
        );
        pending_id = message_id(&delivered, 1);
        expected = pending(&runtime);
        assert_eq!(expected.len(), 1);
    }
    {
        let reopened = open(&path);
        assert_eq!(
            pending(&reopened),
            expected,
            "reopen must preserve pending ownership and delivery metadata"
        );
    }
    let destination = directory.path().join("destination");
    copy_directory(&source, &destination);
    let restored_path = destination.join("data.rdb");
    {
        let restored = open(&restored_path);
        assert_eq!(
            pending(&restored),
            expected,
            "offline copy must preserve pending ownership"
        );
        let available = query(
            &restored,
            "QUEUE READ restore_jobs GROUP workers CONSUMER restored COUNT 10",
        );
        assert_eq!(
            available.len(),
            1,
            "acked and still-locked work must not redeliver"
        );
        query(
            &restored,
            &format!("QUEUE ACK restore_jobs GROUP workers '{pending_id}'"),
        );
        query(
            &restored,
            &format!(
                "QUEUE ACK restore_jobs GROUP workers '{}'",
                message_id(&available, 0)
            ),
        );
        assert!(pending(&restored).is_empty());
        assert!(query(
            &restored,
            "QUEUE READ restore_jobs GROUP workers CONSUMER restored COUNT 10"
        )
        .is_empty());
    }
    let restarted = open(&restored_path);
    assert!(pending(&restarted).is_empty());
    assert!(query(
        &restarted,
        "QUEUE READ restore_jobs GROUP workers CONSUMER restored COUNT 10"
    )
    .is_empty());
}

#[test]
fn expired_pending_delivery_remains_reclaimable_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    let original_id;
    {
        let runtime = open(&path);
        query(&runtime, "CREATE QUEUE restore_jobs LOCK_DEADLINE_MS 1");
        query(&runtime, "QUEUE GROUP CREATE restore_jobs workers");
        query(&runtime, "QUEUE PUSH restore_jobs 'retry'");
        let delivered = query(
            &runtime,
            "QUEUE READ restore_jobs GROUP workers CONSUMER original COUNT 1",
        );
        original_id = message_id(&delivered, 0);
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    let reopened = open(&path);
    let redelivered = query(
        &reopened,
        "QUEUE READ restore_jobs GROUP workers CONSUMER replacement COUNT 1",
    );
    assert_eq!(
        redelivered.len(),
        1,
        "expired ownership must not strand a message"
    );
    assert_eq!(message_id(&redelivered, 0), original_id);
    assert_eq!(
        redelivered.records[0].get("delivery_count"),
        Some(&Value::UnsignedInteger(2))
    );
}

#[test]
fn native_vector_sequence_identity_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    let expected;
    let identities = |runtime: &RedDBRuntime| {
        query(
            runtime,
            "SELECT content,rid,red_sequence_id FROM restore_vectors ORDER BY content",
        )
        .records
        .iter()
        .map(|record| {
            assert!(
                record.get("red_sequence_id").is_some(),
                "sequence must be exposed by the query seam"
            );
            [
                record.get("content").cloned(),
                record.get("rid").cloned(),
                record.get("red_sequence_id").cloned(),
            ]
        })
        .collect::<Vec<_>>()
    };
    {
        let runtime = open(&path);
        for id in 0..32 {
            query(&runtime, &format!("INSERT INTO restore_vectors VECTOR (dense,content) VALUES ([1.0,0.0],'vector-{id:02}')"));
        }
        expected = identities(&runtime);
        assert_eq!(expected.len(), 32);
    }
    let reopened = open(&path);
    assert_eq!(
        identities(&reopened),
        expected,
        "snapshot iteration order must not assign new sequence identities"
    );
}
