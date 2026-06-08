use super::*;
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};

fn catchup_mode_from_wire(
    catchup: Option<&reddb_wire::replication::CatchupModeReply>,
) -> reddb_file::ReplicaCatchupMode {
    match catchup.map(|reply| reply.mode) {
        Some(reddb_wire::replication::CatchupMode::Reclone) => {
            reddb_file::ReplicaCatchupMode::Reclone
        }
        Some(reddb_wire::replication::CatchupMode::Wal) => reddb_file::ReplicaCatchupMode::WalOnly,
        _ => reddb_file::ReplicaCatchupMode::BaseBackupThenWal,
    }
}

fn rebootstrap_ready_message(
    catchup_mode: reddb_file::ReplicaCatchupMode,
    checkpoint_lsn: u64,
) -> String {
    format!(
        "{} staged at checkpoint_lsn={checkpoint_lsn}; restart or store swap required",
        if catchup_mode == reddb_file::ReplicaCatchupMode::Reclone {
            "reclone basebackup"
        } else {
            "basebackup"
        }
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplicaRejoinStartup {
    Continue {
        since_lsn: u64,
        force_reclone: bool,
    },
    Blocked {
        state: &'static str,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedDBOptions;

    #[test]
    fn catchup_mode_from_wire_defaults_to_basebackup_then_wal() {
        assert_eq!(
            catchup_mode_from_wire(None),
            reddb_file::ReplicaCatchupMode::BaseBackupThenWal
        );
        assert_eq!(
            catchup_mode_from_wire(Some(&reddb_wire::replication::CatchupModeReply {
                mode: reddb_wire::replication::CatchupMode::BaseBackupThenWal,
                available_from_lsn: Some(10),
                replica_lsn: Some(3),
                reason: None,
            })),
            reddb_file::ReplicaCatchupMode::BaseBackupThenWal
        );
        assert_eq!(
            catchup_mode_from_wire(Some(&reddb_wire::replication::CatchupModeReply {
                mode: reddb_wire::replication::CatchupMode::Reclone,
                available_from_lsn: None,
                replica_lsn: None,
                reason: Some("slot-invalidated".to_string()),
            })),
            reddb_file::ReplicaCatchupMode::Reclone
        );
    }

    #[test]
    fn replication_health_persists_catchup_mode_for_operator_state() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.persist_replication_health_with_catchup_mode(
            "reclone_required",
            "no usable basebackup is available",
            Some(10),
            Some(3),
            Some(reddb_file::ReplicaCatchupMode::Reclone),
        );

        assert_eq!(
            runtime.config_string("red.replication.state", ""),
            "reclone_required"
        );
        assert_eq!(
            runtime.config_string("red.replication.catchup_mode", ""),
            "reclone"
        );
        assert_eq!(
            runtime.config_string("red.replication.last_error", ""),
            "no usable basebackup is available"
        );
    }

    #[test]
    fn rebootstrap_ready_message_treats_reclone_as_executed_reclone_basebackup() {
        assert_eq!(
            rebootstrap_ready_message(reddb_file::ReplicaCatchupMode::Reclone, 42),
            "reclone basebackup staged at checkpoint_lsn=42; restart or store swap required"
        );
        assert_eq!(
            rebootstrap_ready_message(reddb_file::ReplicaCatchupMode::BaseBackupThenWal, 42),
            "basebackup staged at checkpoint_lsn=42; restart or store swap required"
        );
    }

    #[test]
    fn rejoin_startup_consumes_follow_wal_plan() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "rejoin_follow_wal",
                "rejoin_start_lsn": 950,
            }),
        );

        assert_eq!(
            runtime.replica_rejoin_startup(1_200),
            ReplicaRejoinStartup::Continue {
                since_lsn: 950,
                force_reclone: false,
            }
        );
        assert_eq!(
            runtime.config_u64("red.replication.last_applied_lsn", 0),
            950
        );
        assert_eq!(
            runtime.config_string("red.replication.state", ""),
            "rejoining"
        );
    }

    #[test]
    fn rejoin_startup_blocks_until_rewind_or_reclone_is_executed() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "rejoin_rewind_required",
                "rejoin_target_timeline": 3,
                "rejoin_rewind_to_lsn": 42,
            }),
        );

        assert_eq!(
            runtime.replica_rejoin_startup(77),
            ReplicaRejoinStartup::Blocked {
                state: "rejoin_rewind_required",
                message:
                    "timeline rejoin requires confirmed physical rewind to timeline 3 at LSN 42 before WAL apply can resume"
                        .to_string(),
            }
        );
    }

    #[test]
    fn rejoin_startup_does_not_trust_last_applied_lsn_without_rewind_confirmation() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "rejoin_rewind_required",
                "rejoin_target_timeline": 3,
                "rejoin_rewind_to_lsn": 42,
                "last_applied_lsn": 42,
            }),
        );

        assert_eq!(
            runtime.replica_rejoin_startup(77),
            ReplicaRejoinStartup::Blocked {
                state: "rejoin_rewind_required",
                message:
                    "timeline rejoin requires confirmed physical rewind to timeline 3 at LSN 42 before WAL apply can resume"
                        .to_string(),
            }
        );
        assert_eq!(
            runtime.config_string("red.replication.state", ""),
            "rejoin_rewind_required"
        );
    }

    #[test]
    fn rejoin_startup_consumes_rewind_plan_after_confirmed_physical_rewind() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "rejoin_rewind_required",
                "rejoin_target_timeline": 3,
                "rejoin_rewind_to_lsn": 42,
                "last_applied_lsn": 77,
            }),
        );
        runtime.mark_replica_rejoin_rewind_confirmed(3, 42);

        assert_eq!(
            runtime.replica_rejoin_startup(77),
            ReplicaRejoinStartup::Continue {
                since_lsn: 42,
                force_reclone: false,
            }
        );
        assert_eq!(
            runtime.config_string("red.replication.state", ""),
            "rejoining"
        );
        assert_eq!(
            runtime.config_u64("red.replication.last_applied_lsn", 0),
            42
        );
    }

    #[test]
    fn rejoin_startup_schedules_reclone_without_wal_apply() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "reclone_required",
                "last_applied_lsn": 77,
            }),
        );

        assert_eq!(
            runtime.replica_rejoin_startup(77),
            ReplicaRejoinStartup::Continue {
                since_lsn: 0,
                force_reclone: true,
            }
        );
        assert_eq!(
            runtime.config_string("red.replication.state", ""),
            "reclone_required"
        );
        assert_eq!(
            runtime.config_u64("red.replication.last_applied_lsn", 0),
            77
        );
    }
}

impl RedDBRuntime {
    fn persist_replica_lsn(&self, lsn: u64) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "last_applied_lsn": lsn
            }),
        );
    }

    /// Resolve this replica's stable identity (issue #812). The primary keys
    /// per-replica progress off this id, so it MUST be stable across reboots
    /// — a changing id would make the primary treat every restart as a brand
    /// new replica. Honours an operator-configured `red.replication.replica_id`
    /// first; otherwise generates one once and persists it so the next boot
    /// reuses the same value.
    pub(crate) fn resolve_replica_id(&self) -> String {
        let configured = self.config_string("red.replication.replica_id", "");
        if !configured.is_empty() {
            return configured;
        }
        let generated = crate::crypto::uuid::Uuid::new_v4().to_string();
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "replica_id": generated.clone()
            }),
        );
        generated
    }

    fn persist_replication_health(
        &self,
        state: &str,
        last_error: &str,
        primary_lsn: Option<u64>,
        oldest_available_lsn: Option<u64>,
    ) {
        self.persist_replication_health_with_catchup_mode(
            state,
            last_error,
            primary_lsn,
            oldest_available_lsn,
            None,
        );
    }

    fn persist_replication_health_with_catchup_mode(
        &self,
        state: &str,
        last_error: &str,
        primary_lsn: Option<u64>,
        oldest_available_lsn: Option<u64>,
        catchup_mode: Option<reddb_file::ReplicaCatchupMode>,
    ) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": state,
                "last_error": last_error,
                "catchup_mode": catchup_mode
                    .map(reddb_file::ReplicaCatchupMode::as_str)
                    .unwrap_or(""),
                "last_seen_primary_lsn": primary_lsn.unwrap_or(0),
                "last_seen_oldest_lsn": oldest_available_lsn.unwrap_or(0),
                "updated_at_unix_ms": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
        );
    }

    pub(crate) fn mark_replica_rejoin_rewind_confirmed(
        &self,
        target_timeline: u64,
        rewind_to_lsn: u64,
    ) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "rejoin_rewind_confirmed_timeline": target_timeline,
                "rejoin_rewind_confirmed_lsn": rewind_to_lsn,
                "last_applied_lsn": rewind_to_lsn,
            }),
        );
    }

    fn replica_rejoin_startup(&self, since_lsn: u64) -> ReplicaRejoinStartup {
        match self.config_string("red.replication.state", "").as_str() {
            "rejoin_follow_wal" => {
                let start_lsn = self.config_u64("red.replication.rejoin_start_lsn", since_lsn);
                self.inner.db.store().set_config_tree(
                    "red.replication",
                    &crate::json!({
                        "state": "rejoining",
                        "last_applied_lsn": start_lsn,
                    }),
                );
                ReplicaRejoinStartup::Continue {
                    since_lsn: start_lsn,
                    force_reclone: false,
                }
            }
            "rejoin_rewind_required" => {
                let rewind_to_lsn = self.config_u64("red.replication.rejoin_rewind_to_lsn", 0);
                let target_timeline = self.config_u64("red.replication.rejoin_target_timeline", 0);
                let confirmed_lsn =
                    self.config_u64("red.replication.rejoin_rewind_confirmed_lsn", 0);
                let confirmed_timeline =
                    self.config_u64("red.replication.rejoin_rewind_confirmed_timeline", 0);
                if confirmed_lsn == rewind_to_lsn
                    && confirmed_timeline == target_timeline
                    && rewind_to_lsn > 0
                {
                    self.inner.db.store().set_config_tree(
                        "red.replication",
                        &crate::json!({
                            "state": "rejoining",
                            "last_applied_lsn": rewind_to_lsn,
                        }),
                    );
                    return ReplicaRejoinStartup::Continue {
                        since_lsn: rewind_to_lsn,
                        force_reclone: false,
                    };
                }
                ReplicaRejoinStartup::Blocked {
                    state: "rejoin_rewind_required",
                    message: format!(
                        "timeline rejoin requires confirmed physical rewind to timeline {target_timeline} at LSN {rewind_to_lsn} before WAL apply can resume"
                    ),
                }
            }
            "reclone_required" => ReplicaRejoinStartup::Continue {
                since_lsn: 0,
                force_reclone: true,
            },
            _ => ReplicaRejoinStartup::Continue {
                since_lsn,
                force_reclone: false,
            },
        }
    }

    pub(crate) fn run_replica_loop(&self, primary_addr: String) {
        let endpoint = if primary_addr.starts_with("http") {
            primary_addr
        } else {
            format!("http://{primary_addr}")
        };
        let poll_ms = self.inner.db.options().replication.poll_interval_ms;
        let max_count = self.inner.db.options().replication.max_batch_size;
        let mut since_lsn = self.config_u64("red.replication.last_applied_lsn", 0);
        let force_reclone;
        match self.replica_rejoin_startup(since_lsn) {
            ReplicaRejoinStartup::Continue {
                since_lsn: planned,
                force_reclone: planned_reclone,
            } => {
                since_lsn = planned;
                force_reclone = planned_reclone;
            }
            ReplicaRejoinStartup::Blocked { state, message } => {
                self.persist_replication_health(state, &message, None, None);
                return;
            }
        }
        // Issue #812 — stable identity sent on every WAL pull so the primary
        // can self-register this replica and attribute pulls to it.
        let replica_id = self.resolve_replica_id();

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };

        runtime.block_on(async move {
            use crate::grpc::proto::red_db_client::RedDbClient;
            use crate::grpc::proto::JsonPayloadRequest;

            let mut client = loop {
                match RedDbClient::connect(endpoint.clone()).await {
                    Ok(client) => {
                        self.persist_replication_health("connecting", "", None, None);
                        break client;
                    }
                    Err(_) => {
                        self.persist_replication_health(
                            "connecting",
                            "waiting for primary connection",
                            None,
                            None,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)))
                    }
                }
            };

            if force_reclone {
                match self
                    .stage_primary_replica_rebootstrap_from_snapshot(&mut client, 4 * 1024 * 1024)
                    .await
                {
                    Ok(Some(checkpoint_lsn)) => {
                        self.persist_replication_health_with_catchup_mode(
                            "rebootstrap_ready",
                            &rebootstrap_ready_message(
                                reddb_file::ReplicaCatchupMode::Reclone,
                                checkpoint_lsn,
                            ),
                            None,
                            None,
                            Some(reddb_file::ReplicaCatchupMode::Reclone),
                        );
                    }
                    Ok(None) => {
                        self.persist_replication_health_with_catchup_mode(
                            "reclone_required",
                            "timeline rejoin requires reclone but no primary snapshot is available",
                            None,
                            None,
                            Some(reddb_file::ReplicaCatchupMode::Reclone),
                        );
                    }
                    Err(err) => {
                        self.persist_replication_health_with_catchup_mode(
                            "rebootstrap_error",
                            &format!("failed to stage reclone basebackup: {err}"),
                            None,
                            None,
                            Some(reddb_file::ReplicaCatchupMode::Reclone),
                        );
                    }
                }
                return;
            }

            // PLAN.md Phase 11.5 — stateful applier guards LSN
            // monotonicity across pulls. Seed with the persisted
            // `last_applied_lsn` so reboots don't lose the chain
            // pointer.
            let applier = LogicalChangeApplier::with_metrics(
                since_lsn,
                self.inner.replica_apply_metrics.clone(),
            );

            loop {
                let payload = reddb_wire::replication::WalStreamOpen {
                    since_lsn,
                    max_count,
                    replica_id: Some(replica_id.clone()),
                    await_data: true,
                    await_timeout_ms: 30_000,
                };
                let request = tonic::Request::new(JsonPayloadRequest {
                    payload_json: String::from_utf8(payload.encode_json())
                        .unwrap_or_else(|_| "{}".to_string()),
                });

                if let Ok(response) = client.pull_wal_records(request).await {
                    if let Ok(chunk) = reddb_wire::replication::WalStreamChunk::decode_json(
                        response.into_inner().payload.as_bytes(),
                    )
                    {
                        let current_lsn = Some(chunk.current_lsn);
                        let oldest_available_lsn = chunk.oldest_available_lsn;
                        if chunk.needs_rebootstrap {
                            let reason = chunk.invalidation_reason.as_deref().unwrap_or("unknown");
                            let catchup_mode = catchup_mode_from_wire(chunk.catchup.as_ref());
                            if self.config_string("red.replication.state", "")
                                == "rebootstrap_ready"
                            {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    poll_ms.max(250),
                                ));
                                continue;
                            }
                            {
                                match self
                                    .stage_primary_replica_rebootstrap_from_snapshot(
                                        &mut client,
                                        4 * 1024 * 1024,
                                    )
                                    .await
                                {
                                    Ok(Some(checkpoint_lsn)) => {
                                        self.persist_replication_health_with_catchup_mode(
                                            "rebootstrap_ready",
                                            &rebootstrap_ready_message(
                                                catchup_mode,
                                                checkpoint_lsn,
                                            ),
                                            current_lsn,
                                            oldest_available_lsn,
                                            Some(catchup_mode),
                                        );
                                        std::thread::sleep(std::time::Duration::from_millis(
                                            poll_ms.max(250),
                                        ));
                                        continue;
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        self.persist_replication_health_with_catchup_mode(
                                            "rebootstrap_error",
                                            &format!("failed to stage basebackup rebootstrap: {err}"),
                                            current_lsn,
                                            oldest_available_lsn,
                                            Some(catchup_mode),
                                        );
                                        std::thread::sleep(std::time::Duration::from_millis(
                                            poll_ms.max(250),
                                        ));
                                        continue;
                                    }
                                }
                            }
                            self.persist_replication_health_with_catchup_mode(
                                "rebootstrap_required",
                                &format!("replication slot invalidated ({reason}); re-bootstrap required"),
                                current_lsn,
                                oldest_available_lsn,
                                Some(catchup_mode),
                            );
                            std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                            continue;
                        }
                        if since_lsn > 0
                            && oldest_available_lsn
                                .map(|oldest| oldest > since_lsn.saturating_add(1))
                                .unwrap_or(false)
                        {
                            if self.config_string("red.replication.state", "")
                                == "rebootstrap_ready"
                            {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    poll_ms.max(250),
                                ));
                                continue;
                            }
                            {
                                match self
                                    .stage_primary_replica_rebootstrap_from_snapshot(
                                        &mut client,
                                        4 * 1024 * 1024,
                                    )
                                    .await
                                {
                                    Ok(Some(checkpoint_lsn)) => {
                                        self.persist_replication_health(
                                            "rebootstrap_ready",
                                            &format!(
                                                "basebackup staged at checkpoint_lsn={checkpoint_lsn}; restart or store swap required"
                                            ),
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                        std::thread::sleep(std::time::Duration::from_millis(
                                            poll_ms.max(250),
                                        ));
                                        continue;
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        self.persist_replication_health(
                                            "rebootstrap_error",
                                            &format!("failed to stage basebackup rebootstrap: {err}"),
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                        std::thread::sleep(std::time::Duration::from_millis(
                                            poll_ms.max(250),
                                        ));
                                        continue;
                                    }
                                }
                            }
                            self.persist_replication_health(
                                "rebootstrap_required",
                                "replica is behind the oldest logical WAL available on primary; re-bootstrap required",
                                current_lsn,
                                oldest_available_lsn,
                            );
                            std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                            continue;
                        }
                        {
                            let mut batch_applied_lsn = None;
                            let mut applied_relay_records: Vec<(u64, Vec<u8>)> = Vec::new();
                            let mut ack_failed = false;
                            for record in &chunk.records {
                                let Ok(change) = ChangeRecord::decode(&record.data) else {
                                    self.inner.replica_apply_metrics.record(
                                        crate::replication::logical::ApplyErrorKind::Decode,
                                    );
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode logical WAL record",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                match applier.apply(
                                    self.inner.db.as_ref(),
                                    &change,
                                    ApplyMode::Replica,
                                ) {
                                    Ok(crate::replication::logical::ApplyOutcome::Applied) => {
                                        self.invalidate_result_cache_for_table(&change.collection);
                                        since_lsn = since_lsn.max(change.lsn);
                                        self.persist_replica_lsn(since_lsn);
                                        batch_applied_lsn = Some(since_lsn);
                                        applied_relay_records.push((change.lsn, record.data.clone()));
                                    }
                                    Ok(_) => {
                                        // Idempotent / Skipped: no advance, no error.
                                    }
                                    Err(err) => {
                                        self.inner.replica_apply_metrics.record(err.kind());
                                        // Issue #205 — emit operator-grade event
                                        // for the two replication-fatal kinds. `Gap`
                                        // / `Apply` / `Decode` already persist via
                                        // `persist_replication_health`; the
                                        // OperatorEvent variants only cover the
                                        // two "stream is broken" / "follower
                                        // diverged" conditions an operator must act
                                        // on out-of-band.
                                        match &err {
                                            crate::replication::logical::LogicalApplyError::Divergence { lsn, expected: _, got: _, .. } => {
                                                crate::telemetry::operator_event::OperatorEvent::Divergence {
                                                    peer: "primary".to_string(),
                                                    leader_lsn: *lsn,
                                                    follower_lsn: since_lsn,
                                                }
                                                .emit_global();
                                            }
                                            crate::replication::logical::LogicalApplyError::Gap { last, next } => {
                                                crate::telemetry::operator_event::OperatorEvent::ReplicationBroken {
                                                    peer: "primary".to_string(),
                                                    reason: format!("stalled gap last={last} next={next}"),
                                                }
                                                .emit_global();
                                            }
                                            _ => {}
                                        }
                                        let kind = match &err {
                                            crate::replication::logical::LogicalApplyError::Gap { .. } => "stalled_gap",
                                            crate::replication::logical::LogicalApplyError::Divergence { .. } => "divergence",
                                            // Issue #835 — a stale-term record from a
                                            // returning ex-primary was fenced. The
                                            // replica stays put (no apply, no watermark
                                            // advance) until the legitimate primary's
                                            // current-term stream resumes.
                                            crate::replication::logical::LogicalApplyError::StaleTermFenced { .. } => "stale_term_fenced",
                                            _ => "apply_error",
                                        };
                                        self.persist_replication_health(
                                            kind,
                                            &format!("replica apply rejected: {err}"),
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                        // Stop applying this batch. The
                                        // outer loop will retry on next
                                        // pull, which on a real Gap will
                                        // not magically heal — operator
                                        // must rebootstrap. For
                                        // Divergence, we explicitly do
                                        // not advance; this keeps the
                                        // replica visibly unhealthy
                                        // instead of silently swallowing
                                        // corruption.
                                        break;
                                    }
                                }
                            }
                            if let Some(applied_lsn) = batch_applied_lsn {
                                if let Err(err) = self.record_replica_relay_batch(
                                    &replica_id,
                                    &applied_relay_records,
                                    applied_lsn,
                                ) {
                                    ack_failed = true;
                                    self.persist_replication_health(
                                        "relay_error",
                                        &format!("failed to persist replica relay manifest: {err}"),
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                }
                                if !ack_failed {
                                    let apply_errors = self.replica_apply_error_counts();
                                    let apply_errors_total =
                                        apply_errors.iter().map(|(_, count)| *count).sum::<u64>();
                                    let divergence_total = apply_errors
                                        .iter()
                                        .find(|(kind, _)| {
                                            matches!(
                                                kind,
                                                crate::replication::logical::ApplyErrorKind::Divergence
                                            )
                                        })
                                        .map(|(_, count)| *count)
                                        .unwrap_or(0);
                                    let ack_payload = reddb_wire::replication::WalStreamAck {
                                        replica_id: replica_id.clone(),
                                        applied_lsn,
                                        durable_lsn: applied_lsn,
                                        apply_errors_total,
                                        divergence_total,
                                    };
                                    let ack_request = tonic::Request::new(JsonPayloadRequest {
                                        payload_json: String::from_utf8(ack_payload.encode_json())
                                            .unwrap_or_else(|_| "{}".to_string()),
                                    });
                                    if client.ack_replica_lsn(ack_request).await.is_err() {
                                        ack_failed = true;
                                        self.persist_replication_health(
                                            "ack_error",
                                            "primary ack_replica_lsn request failed",
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                    }
                                }
                            }
                            if ack_failed {
                                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                                continue;
                            }
                        }
                        self.persist_replication_health(
                            "healthy",
                            "",
                            current_lsn,
                            oldest_available_lsn,
                        );
                    } else {
                        self.persist_replication_health(
                            "apply_error",
                            "failed to parse pull_wal_records response",
                            None,
                            None,
                        );
                    }
                } else {
                    self.persist_replication_health(
                        "connecting",
                        "primary pull_wal_records request failed",
                        None,
                        None,
                    );
                    std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                }
            }
        });
    }
}
