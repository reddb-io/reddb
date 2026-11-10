use reddb_server::replication::{AwaitOutcome, CommitPolicy, ReplicationConfig};
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::runtime::query_request::{PreparedRegistry, QueryRequest, QueryRequestExecutor};
use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime};
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

struct EnvGuard(&'static str, Option<String>);
impl EnvGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let old = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self(key, old)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.1 {
            Some(value) => std::env::set_var(self.0, value),
            None => std::env::remove_var(self.0),
        }
        clear_current_connection_id();
    }
}

fn primary() -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .unwrap();
    runtime
        .execute_query("CREATE TABLE commits (id INT)")
        .unwrap();
    runtime
}

#[test]
fn ack_timeout_is_uncertain_even_without_the_old_opt_in_flag() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let _timeout = EnvGuard::set("RED_REPLICATION_ACK_TIMEOUT_MS", Some("1"));
    for flag in [None, Some("false"), Some("true")] {
        let _flag = EnvGuard::set("RED_COMMIT_FAIL_ON_TIMEOUT", flag);
        let runtime = primary();
        runtime
            .execute_query("INSERT INTO commits (id) VALUES (1)")
            .unwrap();
        let lsn = runtime.primary_logical_head_lsn();
        let error = runtime
            .enforce_commit_policy_for_request(lsn, Some(CommitPolicy::AckN(1)))
            .unwrap_err();
        assert!(matches!(error, RedDBError::CommitUncertain { lsn: actual, .. } if actual == lsn));
        assert!(error.to_string().contains("local commit is durable"));
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM commits")
                .unwrap()
                .result
                .records
                .len(),
            1
        );
    }
}

#[test]
fn distinct_replicas_must_ack_the_actual_commit_position() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let _timeout = EnvGuard::set("RED_REPLICATION_ACK_TIMEOUT_MS", Some("1"));
    let runtime = primary();
    runtime
        .execute_query("INSERT INTO commits (id) VALUES (1)")
        .unwrap();
    runtime
        .execute_query("INSERT INTO commits (id) VALUES (2)")
        .unwrap();
    let lsn = runtime.primary_logical_head_lsn();
    runtime
        .ack_primary_replica_lsn_and_prune("replica-a", lsn, lsn, 0, 0)
        .unwrap();
    runtime
        .ack_primary_replica_lsn_and_prune("replica-a", lsn, lsn, 0, 0)
        .unwrap();
    runtime
        .ack_primary_replica_lsn_and_prune("replica-b", lsn - 1, lsn - 1, 0, 0)
        .unwrap();
    assert!(runtime
        .enforce_commit_policy_for_request(lsn, Some(CommitPolicy::AckN(2)))
        .is_err());
    runtime
        .ack_primary_replica_lsn_and_prune("replica-b", lsn, lsn, 0, 0)
        .unwrap();
    assert!(matches!(
        runtime
            .enforce_commit_policy_for_request(lsn, Some(CommitPolicy::AckN(2)))
            .unwrap(),
        AwaitOutcome::Reached(2)
    ));
}

#[test]
fn explicit_transaction_defers_ack_and_retains_its_strongest_policy_until_commit() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let _timeout = EnvGuard::set("RED_REPLICATION_ACK_TIMEOUT_MS", Some("1"));
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    set_current_connection_id(9101);
    executor
        .execute(QueryRequest::sql("BEGIN", vec![]))
        .unwrap();
    let staged = executor
        .execute(
            QueryRequest::sql("INSERT INTO commits (id) VALUES (1), (2), (3)", vec![])
                .with_commit_policy(CommitPolicy::AckN(1)),
        )
        .unwrap();
    assert!(staged.bookmark.is_none());
    assert_eq!(runtime.primary_logical_head_lsn(), 0);
    let error = executor
        .execute(QueryRequest::sql("COMMIT", vec![]))
        .unwrap_err();
    assert!(matches!(error, RedDBError::CommitUncertain { lsn: 1, .. }));
    assert!(!runtime.connection_in_transaction(9101));
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM commits")
            .unwrap()
            .result
            .records
            .len(),
        3
    );
    // The completed transaction's override must not leak to the next write.
    executor
        .execute(QueryRequest::sql(
            "INSERT INTO commits (id) VALUES (4)",
            vec![],
        ))
        .unwrap();
}

#[test]
fn rollback_discards_the_transaction_ack_requirement() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    set_current_connection_id(9102);
    executor
        .execute(QueryRequest::sql("BEGIN", vec![]))
        .unwrap();
    executor
        .execute(
            QueryRequest::sql("INSERT INTO commits (id) VALUES (1)", vec![])
                .with_commit_policy(CommitPolicy::AckN(1)),
        )
        .unwrap();
    executor
        .execute(QueryRequest::sql("ROLLBACK", vec![]))
        .unwrap();
    executor
        .execute(QueryRequest::sql(
            "INSERT INTO commits (id) VALUES (2)",
            vec![],
        ))
        .unwrap();
    assert_eq!(runtime.primary_logical_head_lsn(), 1);
}

#[test]
fn unsupported_or_invalid_durability_is_rejected_before_mutation() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    for policy in [
        CommitPolicy::RemoteWal,
        CommitPolicy::Quorum,
        CommitPolicy::AckN(u32::MAX),
    ] {
        assert!(executor
            .execute(
                QueryRequest::sql("INSERT INTO commits (id) VALUES (1)", vec![])
                    .with_commit_policy(policy)
            )
            .is_err());
    }
    let _invalid = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", Some("ak_n=1"));
    assert!(executor
        .execute(QueryRequest::sql(
            "INSERT INTO commits (id) VALUES (1)",
            vec![]
        ))
        .is_err());
    assert!(runtime
        .execute_query("SELECT id FROM commits")
        .unwrap()
        .result
        .records
        .is_empty());
    assert_eq!(runtime.primary_logical_head_lsn(), 0);
}

#[test]
fn sql_prefixes_and_prepared_writes_cannot_skip_ack_policy() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let _timeout = EnvGuard::set("RED_REPLICATION_ACK_TIMEOUT_MS", Some("1"));
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    for sql in [
        "WITHIN TENANT 'acme' INSERT INTO commits (id) VALUES (1)",
        "WITHIN TENANT 'acme' INSERT INTO commits (id) VALUES (2)",
    ] {
        let error = executor
            .execute(QueryRequest::sql(sql, vec![]).with_commit_policy(CommitPolicy::AckN(1)))
            .unwrap_err();
        assert!(
            matches!(error, RedDBError::CommitUncertain { .. }),
            "{error}"
        );
        assert!(executor
            .execute(QueryRequest::sql(sql, vec![]).with_commit_policy(CommitPolicy::RemoteWal))
            .is_err());
    }
    let prepared = registry
        .prepare(&runtime, "INSERT INTO commits (id) VALUES ($1)")
        .unwrap();
    let error = executor
        .execute(
            QueryRequest::prepared(
                prepared.id,
                vec![reddb_server::runtime::query_request::ParamValue::Int64(3)],
            )
            .with_commit_policy(CommitPolicy::AckN(1)),
        )
        .unwrap_err();
    assert!(
        matches!(error, RedDBError::CommitUncertain { .. }),
        "{error}"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM commits")
            .unwrap()
            .result
            .records
            .len(),
        3
    );
}

#[test]
fn commit_cannot_downgrade_the_transaction_policy() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    set_current_connection_id(9103);
    executor
        .execute(QueryRequest::sql("BEGIN", vec![]))
        .unwrap();
    executor
        .execute(
            QueryRequest::sql(
                "WITHIN TENANT 'acme' INSERT INTO commits (id) VALUES (1)",
                vec![],
            )
            .with_commit_policy(CommitPolicy::AckN(1)),
        )
        .unwrap();
    assert!(executor
        .execute(QueryRequest::sql("COMMIT", vec![]).with_commit_policy(CommitPolicy::Local))
        .is_err());
    assert!(runtime.connection_in_transaction(9103));
    assert_eq!(runtime.primary_logical_head_lsn(), 0);
    executor
        .execute(QueryRequest::sql("ROLLBACK", vec![]))
        .unwrap();
}

#[test]
fn transaction_cannot_replace_ack_count_with_a_smaller_quorum() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let _timeout = EnvGuard::set("RED_REPLICATION_ACK_TIMEOUT_MS", Some("1"));
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory().with_replication(
        ReplicationConfig::primary().with_quorum(reddb_server::replication::QuorumConfig::sync(1)),
    ))
    .unwrap();
    runtime
        .execute_query("CREATE TABLE commits (id INT)")
        .unwrap();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    set_current_connection_id(9104);
    executor
        .execute(QueryRequest::sql("BEGIN", vec![]))
        .unwrap();
    executor
        .execute(
            QueryRequest::sql("INSERT INTO commits (id) VALUES (1)", vec![])
                .with_commit_policy(CommitPolicy::AckN(2)),
        )
        .unwrap();
    for sql in ["INSERT INTO commits (id) VALUES (2)", "COMMIT"] {
        let error = executor
            .execute(QueryRequest::sql(sql, vec![]).with_commit_policy(CommitPolicy::Quorum))
            .unwrap_err();
        assert!(error.to_string().contains("cannot mix"), "{error}");
        assert!(runtime.connection_in_transaction(9104));
        assert_eq!(runtime.primary_logical_head_lsn(), 0);
    }
    // One durable replica cannot satisfy the retained AckN(2) requirement.
    runtime
        .ack_primary_replica_lsn_and_prune("replica-a", 1, 1, 0, 0)
        .unwrap();
    let error = executor
        .execute(QueryRequest::sql("WITHIN TENANT 'acme' COMMIT", vec![]))
        .unwrap_err();
    assert!(
        matches!(error, RedDBError::CommitUncertain { lsn: 1, .. }),
        "{error}"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM commits")
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn cte_reads_do_not_acquire_a_write_commit_policy() {
    let _lock = ENV.lock().unwrap();
    let _policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", None);
    let runtime = primary();
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);
    let _strict = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", Some("ack_n=1"));
    let result = executor
        .execute(
            QueryRequest::sql(
                "WITH q AS (SELECT id FROM commits) SELECT id FROM q",
                vec![],
            )
            .with_commit_policy(CommitPolicy::Local),
        )
        .unwrap();
    assert!(result.result.records.is_empty());
    assert_eq!(runtime.primary_logical_head_lsn(), 0);
}
