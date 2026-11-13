use super::*;
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};

const REPLICA_LOOP_CRASH_ENV: &str = "REDDB_REPLICA_LOOP_CRASH_AT";
const REPLICA_LOOP_CRASH_LSN_ENV: &str = "REDDB_REPLICA_LOOP_CRASH_LSN";
const REPLICA_LOOP_AFTER_CURSOR_BEFORE_ACK: &str = "after_cursor_before_ack";

fn maybe_crash_replica_loop(point: &str, applied_lsn: u64) {
    let target_matches = std::env::var(REPLICA_LOOP_CRASH_LSN_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .is_none_or(|target| target == applied_lsn);
    if target_matches && std::env::var(REPLICA_LOOP_CRASH_ENV).ok().as_deref() == Some(point) {
        std::process::exit(177);
    }
}

async fn acknowledge_replica_cursor(
    client: &mut crate::grpc::proto::red_db_client::RedDbClient<tonic::transport::Channel>,
    request_auth: &crate::replication::request_auth::ReplicaRequestAuth,
    runtime: &RedDBRuntime,
    replica_id: &str,
    applied_lsn: u64,
) -> Result<(), tonic::Status> {
    let apply_errors = runtime.replica_apply_error_counts();
    let apply_errors_total = apply_errors.iter().map(|(_, count)| *count).sum::<u64>();
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
        replica_id: replica_id.to_string(),
        applied_lsn,
        durable_lsn: applied_lsn,
        apply_errors_total,
        divergence_total,
    };
    let ack_request = request_auth.request(crate::grpc::proto::JsonPayloadRequest {
        payload_json: String::from_utf8(ack_payload.encode_json())
            .unwrap_or_else(|_| "{}".to_string()),
    });
    client.ack_replica_lsn(ack_request).await.map(|_| ())
}

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

fn should_fallback_to_legacy_wal(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::Unimplemented
}

async fn pull_wal_chunk(
    client: &mut crate::grpc::proto::red_db_client::RedDbClient<tonic::transport::Channel>,
    request_auth: &crate::replication::request_auth::ReplicaRequestAuth,
    open: &reddb_wire::replication::WalStreamOpen,
    routing_endpoints: &reddb_wire::routing::RoutingEndpoints,
    binary_stream_supported: &mut Option<bool>,
) -> Result<
    (
        reddb_wire::replication::WalStreamChunk,
        Option<reddb_wire::routing::RoutingEndpoints>,
    ),
    tonic::Status,
> {
    if *binary_stream_supported != Some(false) {
        let request = crate::grpc::proto::WalStreamRequest {
            version: 1,
            since_lsn: open.since_lsn,
            max_count: u32::try_from(open.max_count).unwrap_or(u32::MAX),
            replica_id: open.replica_id.clone(),
            term: open.term,
            await_data: open.await_data,
            await_timeout_ms: open.await_timeout_ms,
            routing_endpoints: routing_endpoints
                .iter()
                .map(|(protocol, address)| (protocol.clone(), address.clone()))
                .collect(),
            max_chunk_bytes: 1024 * 1024,
            installed_snapshot_token: open.installed_snapshot_token.clone(),
        };
        match client
            .stream_wal_records(request_auth.request(request))
            .await
        {
            Ok(response) => {
                *binary_stream_supported = Some(true);
                let mut stream = response.into_inner();
                // Consume one bounded chunk per apply/relay/cursor/ACK cycle.
                // Dropping the remaining stream applies backpressure to the
                // capacity-one producer; the next request resumes from the
                // newly durable LSN instead of accumulating a whole catch-up
                // response in replica memory.
                let chunk = stream.message().await?.ok_or_else(|| {
                    tonic::Status::data_loss("primary returned an empty WAL stream")
                })?;
                if chunk.version != 1 {
                    return Err(tonic::Status::data_loss(format!(
                        "primary returned unsupported WAL stream version {}",
                        chunk.version
                    )));
                }
                let endpoints = crate::grpc::validated_routing_endpoints(chunk.primary_endpoints);
                let endpoints = (!endpoints.is_empty()).then_some(endpoints);
                return Ok((
                    reddb_wire::replication::WalStreamChunk {
                        records: chunk
                            .records
                            .into_iter()
                            .map(|record| reddb_wire::replication::WalStreamRecord {
                                lsn: record.lsn,
                                data: record.data.to_vec(),
                            })
                            .collect(),
                        current_lsn: chunk.current_lsn,
                        oldest_available_lsn: chunk.oldest_available_lsn,
                        partial_resync: chunk.partial_resync,
                        partial_resync_count: chunk.partial_resync_count,
                        needs_rebootstrap: chunk.needs_rebootstrap,
                        invalidation_reason: chunk.invalidation_reason,
                        catchup: crate::grpc::wire_wal_catchup(chunk.catchup)?,
                    },
                    endpoints,
                ));
            }
            Err(status) if should_fallback_to_legacy_wal(&status) => {
                *binary_stream_supported = Some(false);
            }
            Err(status) => return Err(status),
        }
    }

    let request = request_auth.request(crate::grpc::proto::JsonPayloadRequest {
        payload_json: String::from_utf8(open.encode_json_with_endpoints(Some(routing_endpoints)))
            .expect("invariant: WAL open encoder emits UTF-8 JSON"),
    });
    let response = client.pull_wal_records(request).await?;
    reddb_wire::replication::WalStreamChunk::decode_json_with_endpoints(
        response.into_inner().payload.as_bytes(),
    )
    .map_err(|error| tonic::Status::data_loss(error.to_string()))
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
    fn legacy_wal_fallback_is_only_for_old_servers() {
        assert!(should_fallback_to_legacy_wal(
            &tonic::Status::unimplemented("old primary")
        ));
        for status in [
            tonic::Status::unauthenticated("bad credential"),
            tonic::Status::permission_denied("missing capability"),
            tonic::Status::resource_exhausted("record too large"),
            tonic::Status::data_loss("invalid chunk"),
        ] {
            assert!(
                !should_fallback_to_legacy_wal(&status),
                "must not downgrade after {status}"
            );
        }
    }

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
            runtime.config_string("replication.state", ""),
            "reclone_required"
        );
        assert_eq!(
            runtime.config_string("replication.catchup_mode", ""),
            "reclone"
        );
        assert_eq!(
            runtime.config_string("replication.last_error", ""),
            "no usable basebackup is available"
        );
    }

    #[test]
    fn repeated_replication_health_does_not_persist_a_new_timestamp() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.persist_replication_health("connecting", "primary unavailable", None, None);
        runtime
            .inner
            .db
            .store()
            .set_config_tree("replication", &crate::json!({ "updated_at_unix_ms": 7 }));

        runtime.persist_replication_health("connecting", "primary unavailable", None, None);

        assert_eq!(
            runtime.config_u64("replication.updated_at_unix_ms", 0),
            7,
            "an unchanged retry must not create durable storage churn"
        );
        runtime.persist_replication_health("healthy", "", Some(3), Some(1));
        assert_ne!(
            runtime.config_u64("replication.updated_at_unix_ms", 0),
            7,
            "a real health transition must still be persisted"
        );
    }

    #[test]
    fn reconnecting_replica_retains_the_last_observed_primary_frontier() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.persist_replication_health("healthy", "", Some(42), Some(7));

        runtime.persist_replication_health("connecting", "primary unavailable", None, None);

        assert_eq!(
            runtime.config_u64("replication.last_seen_primary_lsn", 0),
            42
        );
        assert_eq!(runtime.config_u64("replication.last_seen_oldest_lsn", 0), 7);
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
            "replication",
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
        assert_eq!(runtime.config_u64("replication.last_applied_lsn", 0), 950);
        assert_eq!(runtime.config_string("replication.state", ""), "rejoining");
    }

    #[test]
    fn rejoin_startup_blocks_until_rewind_or_reclone_is_executed() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "replication",
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
            "replication",
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
            runtime.config_string("replication.state", ""),
            "rejoin_rewind_required"
        );
    }

    #[test]
    fn rejoin_startup_consumes_rewind_plan_after_confirmed_physical_rewind() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "replication",
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
        assert_eq!(runtime.config_string("replication.state", ""), "rejoining");
        assert_eq!(runtime.config_u64("replication.last_applied_lsn", 0), 42);
    }

    #[test]
    fn rejoin_startup_schedules_reclone_without_wal_apply() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
        runtime.inner.db.store().set_config_tree(
            "replication",
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
            runtime.config_string("replication.state", ""),
            "reclone_required"
        );
        assert_eq!(runtime.config_u64("replication.last_applied_lsn", 0), 77);
    }
}

impl RedDBRuntime {
    fn persist_replica_lsn(&self, lsn: u64) -> RedDBResult<()> {
        self.inner.db.store().try_set_config_tree(
            "replication",
            &crate::json!({
                "last_applied_lsn": lsn
            }),
        )?;
        Ok(())
    }

    /// Resolve this replica's stable identity (issue #812). The primary keys
    /// per-replica progress off this id, so it MUST be stable across reboots
    /// — a changing id would make the primary treat every restart as a brand
    /// new replica. Honours an operator-configured `replication.replica_id`
    /// first; otherwise generates one once and persists it so the next boot
    /// reuses the same value.
    pub(crate) fn resolve_replica_id(&self) -> String {
        let configured = self.config_string("replication.replica_id", "");
        if !configured.is_empty() {
            return configured;
        }
        let generated = crate::crypto::uuid::Uuid::new_v4().to_string();
        self.inner.db.store().set_config_tree(
            "replication",
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
        let catchup_mode = catchup_mode
            .map(reddb_file::ReplicaCatchupMode::as_str)
            .unwrap_or("");
        // A connection failure carries no new frontier. Preserve the last
        // observed values so failover can distinguish a never-synced replica
        // from one that was caught up immediately before the primary vanished.
        let primary_lsn =
            primary_lsn.unwrap_or_else(|| self.config_u64("replication.last_seen_primary_lsn", 0));
        let oldest_available_lsn = oldest_available_lsn
            .unwrap_or_else(|| self.config_u64("replication.last_seen_oldest_lsn", 0));
        // Issue #1243 — observe every persisted health transition for
        // reconnect telemetry. This is the single chokepoint the replica
        // loop routes all state changes through, so the counter sees each
        // link drop (`connecting`) and restore (`healthy`) exactly once.
        self.inner.replica_link_metrics.observe_state(state);
        if self.config_string("replication.state", "") == state
            && self.config_string("replication.last_error", "") == last_error
            && self.config_string("replication.catchup_mode", "") == catchup_mode
            && self.config_u64("replication.last_seen_primary_lsn", 0) == primary_lsn
            && self.config_u64("replication.last_seen_oldest_lsn", 0) == oldest_available_lsn
        {
            return;
        }
        self.inner.db.store().set_config_tree(
            "replication",
            &crate::json!({
                "state": state,
                "last_error": last_error,
                "catchup_mode": catchup_mode,
                "last_seen_primary_lsn": primary_lsn,
                "last_seen_oldest_lsn": oldest_available_lsn,
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
            "replication",
            &crate::json!({
                "rejoin_rewind_confirmed_timeline": target_timeline,
                "rejoin_rewind_confirmed_lsn": rewind_to_lsn,
                "last_applied_lsn": rewind_to_lsn,
            }),
        );
    }

    fn replica_rejoin_startup(&self, since_lsn: u64) -> ReplicaRejoinStartup {
        match self.config_string("replication.state", "").as_str() {
            "rejoin_follow_wal" => {
                let start_lsn = self.config_u64("replication.rejoin_start_lsn", since_lsn);
                self.inner.db.store().set_config_tree(
                    "replication",
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
                let rewind_to_lsn = self.config_u64("replication.rejoin_rewind_to_lsn", 0);
                let target_timeline = self.config_u64("replication.rejoin_target_timeline", 0);
                let confirmed_lsn = self.config_u64("replication.rejoin_rewind_confirmed_lsn", 0);
                let confirmed_timeline =
                    self.config_u64("replication.rejoin_rewind_confirmed_timeline", 0);
                if confirmed_lsn == rewind_to_lsn
                    && confirmed_timeline == target_timeline
                    && rewind_to_lsn > 0
                {
                    self.inner.db.store().set_config_tree(
                        "replication",
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
        let mut since_lsn = self.config_u64("replication.last_applied_lsn", 0);
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
        // can self-register this replica and attribute pulls to it. When the
        // primary authenticates replication, its principal is also the slot
        // identity; allowing those names to drift would let this process forge
        // acknowledgements for another replica.
        let request_auth = match crate::replication::request_auth::ReplicaRequestAuth::from_env() {
            Ok(auth) => auth,
            Err(message) => {
                self.persist_replication_health("auth_error", message, None, None);
                return;
            }
        };
        let replica_id = if let Some(principal) = request_auth.principal() {
            let configured = self.config_string("replication.replica_id", "");
            if !configured.is_empty() && configured != principal {
                self.persist_replication_health(
                    "auth_error",
                    "replica identity differs from replication credential principal",
                    None,
                    None,
                );
                return;
            }
            self.inner
                .db
                .store()
                .set_config_tree("replication", &crate::json!({ "replica_id": principal }));
            principal.to_string()
        } else {
            self.resolve_replica_id()
        };
        let installed_snapshot_token =
            self.config_string("replication.rebootstrap_snapshot_token", "");
        let mut installed_snapshot_token =
            (!installed_snapshot_token.is_empty()).then_some(installed_snapshot_token);

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };

        runtime.block_on(async move {
            use crate::grpc::proto::red_db_client::RedDbClient;

            let mut client = loop {
                match RedDbClient::connect(endpoint.clone()).await {
                    Ok(client) => {
                        self.persist_replication_health("connecting", "", None, None);
                        let max_message_bytes = crate::grpc::RedDBGrpcServer::max_message_bytes();
                        break client
                            .max_decoding_message_size(max_message_bytes)
                            .max_encoding_message_size(max_message_bytes);
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
            // Register the replica and publish a healthy link state before
            // entering the 30-second tail-follow wait on an empty primary.
            // Every subsequent pull keeps the efficient await-data behavior.
            let mut await_data = false;
            let mut binary_stream_supported = None;
            // A crash or transport failure can leave the local durable cursor
            // ahead of the primary slot. Reconcile it before awaiting or
            // applying more WAL, and retain it until the primary accepts the ACK.
            let mut pending_ack_lsn = (since_lsn > 0).then_some(since_lsn);

            loop {
                let open = reddb_wire::replication::WalStreamOpen {
                    since_lsn,
                    max_count,
                    replica_id: Some(replica_id.clone()),
                    installed_snapshot_token: installed_snapshot_token.clone(),
                    term: self.current_replication_term(),
                    await_data: await_data && pending_ack_lsn.is_none(),
                    await_timeout_ms: 30_000,
                };
                let routing_endpoints = self.routing_endpoints();
                let pull_result = pull_wal_chunk(
                    &mut client,
                    &request_auth,
                    &open,
                    &routing_endpoints,
                    &mut binary_stream_supported,
                )
                .await;
                if let Err(error) = &pull_result {
                    self.persist_replication_health(
                        "connecting",
                        &format!("primary WAL stream request failed: {error}"),
                        None,
                        None,
                    );
                }
                match pull_result {
                    Ok((chunk, endpoints)) => {
                        await_data = true;
                        self.cache_routing_primary_endpoints(endpoints);
                        let current_lsn = Some(chunk.current_lsn);
                        let oldest_available_lsn = chunk.oldest_available_lsn;
                        if !chunk.needs_rebootstrap && installed_snapshot_token.is_some() {
                            if let Err(error) = self.inner.db.store().try_set_config_tree(
                                "replication",
                                &crate::json!({
                                    "rebootstrap_snapshot_token": "",
                                }),
                            ) {
                                self.persist_replication_health(
                                    "cursor_error",
                                    &format!(
                                        "failed to retire installed snapshot proof: {error}"
                                    ),
                                    current_lsn,
                                    oldest_available_lsn,
                                );
                                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                                continue;
                            }
                            installed_snapshot_token = None;
                        }
                        if chunk.needs_rebootstrap {
                            let reason = chunk.invalidation_reason.as_deref().unwrap_or("unknown");
                            let catchup_mode = catchup_mode_from_wire(chunk.catchup.as_ref());
                            if self.config_string("replication.state", "")
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
                            if self.config_string("replication.state", "")
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
                        if let Some(applied_lsn) = pending_ack_lsn {
                            if applied_lsn > chunk.current_lsn {
                                self.persist_replication_health(
                                    "rejoin_primary_behind",
                                    &format!(
                                        "local durable cursor {applied_lsn} is ahead of primary head {}",
                                        chunk.current_lsn
                                    ),
                                    current_lsn,
                                    oldest_available_lsn,
                                );
                                std::thread::sleep(std::time::Duration::from_millis(
                                    poll_ms.max(250),
                                ));
                                continue;
                            }
                            if acknowledge_replica_cursor(
                                &mut client,
                                &request_auth,
                                self,
                                &replica_id,
                                applied_lsn,
                            )
                            .await
                            .is_err()
                            {
                                self.persist_replication_health(
                                    "ack_error",
                                    "primary ack_replica_lsn request failed",
                                    current_lsn,
                                    oldest_available_lsn,
                                );
                                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                                continue;
                            }
                            pending_ack_lsn = None;
                        }
                        {
                            let mut batch_applied_lsn = None;
                            let mut applied_relay_records: Vec<(u64, &[u8])> = Vec::new();
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
                                let range_authority =
                                    self.inner.write_gate.primary_replica_range_authority();
                                match applier.apply_fenced(
                                    self.inner.db.as_ref(),
                                    &change,
                                    ApplyMode::Replica,
                                    range_authority.as_ref(),
                                ) {
                                    Ok(crate::replication::logical::ApplyOutcome::Applied) => {
                                        self.invalidate_result_cache_for_table(&change.collection);
                                        since_lsn = since_lsn.max(change.lsn);
                                        batch_applied_lsn = Some(since_lsn);
                                        applied_relay_records.push((change.lsn, &record.data));
                                    }
                                    Ok(
                                        crate::replication::logical::ApplyOutcome::Idempotent
                                        | crate::replication::logical::ApplyOutcome::Skipped,
                                    ) => {
                                        // The local cursor can be ahead of the primary slot when
                                        // a process stops after cursor persistence but before ACK.
                                        // Re-run the relay write idempotently so it validates the
                                        // already-durable segment, then repeat cursor + ACK.
                                        since_lsn = since_lsn.max(applier.last_applied_lsn());
                                        batch_applied_lsn = Some(since_lsn);
                                        applied_relay_records.push((change.lsn, &record.data));
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
                                            crate::replication::logical::LogicalApplyError::RangeFenced { range_id, lsn, .. } => {
                                                if let Some(authority) = range_authority {
                                                    crate::telemetry::operator_event::OperatorEvent::ReplicaAuthorityDivergence {
                                                        range_identity: format!("system.global/{range_id}"),
                                                        expected_term: authority.min_term,
                                                        expected_ownership_epoch: authority.min_ownership_epoch,
                                                        observed_term: change.term,
                                                        observed_ownership_epoch: change.ownership_epoch.unwrap_or(0),
                                                        lsn: *lsn,
                                                    }
                                                    .emit_global();
                                                }
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
                                            crate::replication::logical::LogicalApplyError::RangeFenced { .. } => "authority_divergence",
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
                                    if let Err(err) = self.persist_replica_lsn(applied_lsn) {
                                        ack_failed = true;
                                        self.persist_replication_health(
                                            "cursor_error",
                                            &format!(
                                                "failed to persist replica apply cursor: {err}"
                                            ),
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                    }
                                }
                                if !ack_failed {
                                    pending_ack_lsn = Some(applied_lsn);
                                    maybe_crash_replica_loop(
                                        REPLICA_LOOP_AFTER_CURSOR_BEFORE_ACK,
                                        applied_lsn,
                                    );
                                    if acknowledge_replica_cursor(
                                        &mut client,
                                        &request_auth,
                                        self,
                                        &replica_id,
                                        applied_lsn,
                                    )
                                    .await
                                    .is_err()
                                    {
                                        ack_failed = true;
                                        self.persist_replication_health(
                                            "ack_error",
                                            "primary ack_replica_lsn request failed",
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                    } else {
                                        pending_ack_lsn = None;
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
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                    }
                }
            }
        });
    }
}
