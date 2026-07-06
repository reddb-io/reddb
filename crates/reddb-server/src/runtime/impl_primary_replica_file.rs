use super::*;

impl RedDBRuntime {
    pub fn replica_relay_manifest_path(&self, replica_id: &str) -> Option<std::path::PathBuf> {
        let plan = self.primary_replica_file_plan()?;
        Some(plan.relay_manifest_path(replica_id))
    }

    pub fn primary_replica_file_plan(&self) -> Option<reddb_file::PrimaryReplicaFilePlan> {
        self.primary_replica_file_plan_result().ok().flatten()
    }

    fn primary_replica_root(&self) -> Option<std::path::PathBuf> {
        let data_path = self.inner.db.options().data_path.as_ref()?;
        Some(crate::replication::primary::PrimaryReplication::primary_replica_root_for(data_path))
    }

    pub fn primary_replica_file_plan_result(
        &self,
    ) -> RedDBResult<Option<reddb_file::PrimaryReplicaFilePlan>> {
        let Some(root) = self.primary_replica_root() else {
            return Ok(None);
        };
        let timeline = self.primary_replica_current_timeline(&root)?;
        Ok(Some(reddb_file::PrimaryReplicaFilePlan::new(
            root, timeline,
        )))
    }

    fn primary_replica_current_timeline(
        &self,
        root: &std::path::Path,
    ) -> RedDBResult<reddb_file::TimelineId> {
        let path = reddb_file::PrimaryReplicaFilePlan::new(root, reddb_file::TimelineId::initial())
            .timeline_history_path();
        match reddb_file::TimelineHistory::read_from_path(&path) {
            Ok(history) => Ok(history
                .current()
                .unwrap_or_else(reddb_file::TimelineId::initial)),
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(reddb_file::TimelineId::initial())
            }
            Err(err) => Err(RedDBError::Internal(err.to_string())),
        }
    }

    pub fn create_primary_replica_basebackup(
        &self,
        chunk_bytes: usize,
    ) -> RedDBResult<Option<reddb_file::PrimaryReplicaBaseBackupManifest>> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(None);
        };
        self.flush()?;
        let checkpoint_lsn = self.primary_logical_head_lsn().max(self.cdc_current_lsn());
        let snapshot = self.inner.db.store().to_binary_dump_bytes();
        let backup = reddb_file::BaseBackupPlan::new(plan.timeline, 0, checkpoint_lsn);
        let manifest = plan
            .write_basebackup_snapshot_parts(backup, &snapshot, chunk_bytes)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        manifest
            .write_to_path(plan.basebackup_path(&backup))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(Some(manifest))
    }

    pub fn materialize_primary_replica_basebackup_snapshot(
        &self,
        manifest: &reddb_file::PrimaryReplicaBaseBackupManifest,
        parts_root: impl AsRef<std::path::Path>,
        destination: impl AsRef<std::path::Path>,
    ) -> RedDBResult<u64> {
        let snapshot = manifest
            .read_snapshot_parts(parts_root)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        let loaded = crate::storage::unified::UnifiedStore::load_from_bytes_with_config(
            &snapshot,
            crate::storage::unified::UnifiedStoreConfig::default(),
        )
        .map_err(|err| RedDBError::Internal(format!("validate basebackup snapshot: {err}")))?;
        loaded.set_config_tree(
            "red.replication",
            &crate::json!({
                "last_applied_lsn": manifest.checkpoint_lsn,
                "state": "healthy",
                "last_error": "",
            }),
        );
        let snapshot = loaded.to_binary_dump_bytes();
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::storage::EmbeddedRdbArtifact::create_with_snapshot(destination, &snapshot)?;
        Ok(manifest.checkpoint_lsn)
    }

    pub fn replica_rebootstrap_staging_root(&self) -> Option<std::path::PathBuf> {
        let data_path = self.inner.db.options().data_path.as_ref()?;
        Some(reddb_file::layout::rebootstrap_staging_root(data_path))
    }

    pub fn replica_rebootstrap_pending_path(&self) -> Option<std::path::PathBuf> {
        let data_path = self.inner.db.options().data_path.as_ref()?;
        Some(reddb_file::layout::rebootstrap_pending_path(data_path))
    }

    pub(crate) async fn stage_primary_replica_rebootstrap_from_snapshot(
        &self,
        client: &mut crate::grpc::proto::red_db_client::RedDbClient<tonic::transport::Channel>,
        chunk_bytes: usize,
    ) -> RedDBResult<Option<u64>> {
        let Some(parts_root) = self.replica_rebootstrap_staging_root() else {
            return Ok(None);
        };
        let Some(pending_path) = self.replica_rebootstrap_pending_path() else {
            return Ok(None);
        };
        let data_path = self
            .inner
            .db
            .options()
            .data_path
            .as_ref()
            .ok_or_else(|| RedDBError::Internal("replica data path unavailable".into()))?;
        let intent_log_path = reddb_file::layout::rebootstrap_intent_log_path(data_path);
        let intent_log = crate::telemetry::admin_intent_log::AdminIntentLog::open(intent_log_path)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        let replica_id = self.resolve_replica_id();
        let bootstrapper = crate::replication::replica::ReplicaBootstrapper::new(replica_id);
        let source_lsn = self.config_u64("red.replication.last_applied_lsn", 0);
        let _ = std::fs::remove_file(&pending_path);

        let chunk_bytes = chunk_bytes.max(1);
        let (resume, mut bootstrap_handle) = match bootstrapper.resume(&intent_log) {
            Some((resume, handle)) => (Some(resume), handle),
            None => (
                None,
                bootstrapper
                    .begin(&intent_log, source_lsn, 0)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?,
            ),
        };
        let mut token: Option<String> = resume
            .as_ref()
            .and_then(|resume| resume.snapshot_token.clone());
        let mut manifest: Option<reddb_file::PrimaryReplicaBaseBackupManifest> = None;
        let mut written = std::collections::BTreeSet::new();
        let mut offset = resume
            .as_ref()
            .map(|resume| resume.snapshot_offset)
            .unwrap_or(0);

        loop {
            let mut request = tonic::Request::new(crate::grpc::proto::Empty {});
            request.metadata_mut().insert(
                "x-reddb-snapshot-max-bytes",
                chunk_bytes
                    .to_string()
                    .parse()
                    .map_err(|err| RedDBError::Internal(format!("snapshot max bytes: {err}")))?,
            );
            request.metadata_mut().insert(
                "x-reddb-snapshot-offset",
                offset
                    .to_string()
                    .parse()
                    .map_err(|err| RedDBError::Internal(format!("snapshot offset: {err}")))?,
            );
            if let Some(token) = &token {
                request.metadata_mut().insert(
                    "x-reddb-snapshot-token",
                    token.parse().map_err(|err| {
                        RedDBError::Internal(format!("snapshot token metadata: {err}"))
                    })?,
                );
            }

            let response = client
                .replication_snapshot(request)
                .await
                .map_err(|err| RedDBError::Internal(format!("replication snapshot: {err}")))?;
            let payload = reddb_wire::replication::BaseBackupChunk::decode_json(
                response.into_inner().payload.as_bytes(),
            )
            .map_err(|err| RedDBError::Internal(format!("parse replication snapshot: {err}")))?;
            if token.is_none() {
                token = payload.snapshot_token.clone();
            }
            let staged =
                crate::replication::replica::stage_basebackup_snapshot_chunk(&payload, &parts_root)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?
                    .ok_or_else(|| {
                        RedDBError::Internal(
                            "replication snapshot did not include basebackup payload".into(),
                        )
                    })?;
            if let Some(existing) = &manifest {
                if existing != &staged.manifest {
                    return Err(RedDBError::Internal(
                        "replication snapshot basebackup manifest changed while downloading".into(),
                    ));
                }
            } else {
                manifest = Some(staged.manifest.clone());
                written.extend(
                    crate::replication::replica::recover_staged_basebackup_chunks(
                        &staged.manifest,
                        &parts_root,
                    )
                    .map_err(|err| RedDBError::Internal(err.to_string()))?,
                );
            }
            if let Some(ordinal) = staged.chunk_ordinal {
                written.insert(ordinal);
            }
            let current = manifest
                .as_ref()
                .expect("manifest set after staging basebackup chunk");
            if current
                .chunks
                .iter()
                .all(|chunk| written.contains(&chunk.ordinal))
            {
                current
                    .verify_snapshot_parts(&parts_root)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                let checkpoint_lsn = self.materialize_primary_replica_basebackup_snapshot(
                    current,
                    &parts_root,
                    &pending_path,
                )?;
                self.inner.db.store().set_config_tree(
                    "red.replication",
                    &crate::json!({
                        "state": "rebootstrap_ready",
                        "rebootstrap_pending_path": pending_path.display().to_string(),
                        "rebootstrap_checkpoint_lsn": checkpoint_lsn,
                        "rebootstrap_timeline": current.timeline.0,
                    }),
                );
                let data_path =
                    self.inner.db.options().data_path.as_ref().ok_or_else(|| {
                        RedDBError::Internal("replica data path unavailable".into())
                    })?;
                reddb_file::write_rebootstrap_ready_marker(
                    data_path,
                    &reddb_file::ReplicaRebootstrapReadyMarker {
                        pending_path: pending_path.clone(),
                        checkpoint_lsn,
                        timeline: current.timeline,
                    },
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
                bootstrap_handle
                    .complete(current.chunks.len() as u64, 0)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                return Ok(Some(checkpoint_lsn));
            }
            let next = current
                .chunks
                .iter()
                .find(|chunk| !written.contains(&chunk.ordinal))
                .ok_or_else(|| RedDBError::Internal("basebackup chunk tracking stalled".into()))?;
            offset = next.snapshot_offset;
            if let Some(snapshot_token) = token.as_deref() {
                bootstrap_handle
                    .checkpoint_snapshot_transfer(
                        snapshot_token,
                        offset,
                        source_lsn,
                        written.len() as u64,
                    )
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }
        }
    }

    pub fn primary_replica_slot_catalog(
        &self,
    ) -> RedDBResult<Option<reddb_file::ReplicationSlotCatalog>> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(None);
        };
        match reddb_file::ReplicationSlotCatalog::read_from_path(plan.slots_path()) {
            Ok(catalog) => Ok(Some(catalog)),
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(err) => Err(RedDBError::Internal(err.to_string())),
        }
    }

    fn primary_replica_fork_lsns(&self) -> RedDBResult<Vec<u64>> {
        let Some(data_path) = self.inner.db.options().data_path.as_ref() else {
            return Ok(Vec::new());
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(data_path)
            .list_forks()
            .map(|forks| forks.into_iter().map(|fork| fork.fork_lsn).collect())
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn primary_replica_wal_retention_plan(
        &self,
    ) -> RedDBResult<Option<reddb_file::WalRetentionPlan>> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(None);
        };
        let Some(catalog) = self.primary_replica_slot_catalog()? else {
            return Ok(None);
        };
        let current_lsn = self.primary_logical_head_lsn().max(self.cdc_current_lsn());
        let fork_lsns = self.primary_replica_fork_lsns()?;
        Ok(Some(
            plan.plan_wal_retention_with_fork_lsns(&catalog, &fork_lsns, current_lsn)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        ))
    }

    pub fn primary_replica_catchup_mode(
        &self,
        available_from_lsn: u64,
        replica_lsn: u64,
    ) -> RedDBResult<Option<reddb_file::ReplicaCatchupMode>> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(None);
        };
        let basebackups = plan
            .list_basebackups()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(Some(plan.catchup_mode_with_basebackups(
            available_from_lsn,
            replica_lsn,
            &basebackups,
        )))
    }

    pub fn primary_replica_timeline_history_path(&self) -> Option<std::path::PathBuf> {
        let root = self.primary_replica_root()?;
        Some(
            reddb_file::PrimaryReplicaFilePlan::new(root, reddb_file::TimelineId::initial())
                .timeline_history_path(),
        )
    }

    pub fn primary_replica_rejoin_decision(
        &self,
        node_timeline: reddb_file::TimelineId,
        node_flushed_lsn: u64,
        available_from_lsn: u64,
    ) -> RedDBResult<Option<reddb_file::RejoinDecision>> {
        let Some(path) = self.primary_replica_timeline_history_path() else {
            return Ok(None);
        };
        let history = match reddb_file::TimelineHistory::read_from_path(&path) {
            Ok(history) => history,
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                reddb_file::TimelineHistory::new(crate::utils::now_unix_millis())
            }
            Err(err) => return Err(RedDBError::Internal(err.to_string())),
        };
        Ok(Some(history.rejoin_decision(
            node_timeline,
            node_flushed_lsn,
            available_from_lsn,
        )))
    }

    pub fn persist_primary_replica_rejoin_plan(
        &self,
        node_timeline: reddb_file::TimelineId,
        node_flushed_lsn: u64,
        available_from_lsn: u64,
    ) -> RedDBResult<Option<reddb_file::RejoinDecision>> {
        let Some(decision) = self.primary_replica_rejoin_decision(
            node_timeline,
            node_flushed_lsn,
            available_from_lsn,
        )?
        else {
            return Ok(None);
        };

        let (state, target_timeline, rewind_to_lsn, start_lsn) = match decision {
            reddb_file::RejoinDecision::AlreadyCurrent => {
                ("timeline_current", node_timeline.0, 0, node_flushed_lsn)
            }
            reddb_file::RejoinDecision::FollowNewTimeline {
                target_timeline,
                start_lsn,
            } => ("rejoin_follow_wal", target_timeline.0, 0, start_lsn),
            reddb_file::RejoinDecision::Rewind {
                target_timeline,
                rewind_to_lsn,
            } => (
                "rejoin_rewind_required",
                target_timeline.0,
                rewind_to_lsn,
                0,
            ),
            reddb_file::RejoinDecision::Reclone => ("reclone_required", 0, 0, 0),
        };
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": state,
                "rejoin_node_timeline": node_timeline.0,
                "rejoin_node_flushed_lsn": node_flushed_lsn,
                "rejoin_available_from_lsn": available_from_lsn,
                "rejoin_target_timeline": target_timeline,
                "rejoin_rewind_to_lsn": rewind_to_lsn,
                "rejoin_start_lsn": start_lsn,
                "rejoin_rewind_confirmed_timeline": 0,
                "rejoin_rewind_confirmed_lsn": 0,
            }),
        );

        Ok(Some(decision))
    }

    pub fn prune_primary_replica_wal_segments(
        &self,
    ) -> RedDBResult<Option<reddb_file::WalPruneResult>> {
        let current_lsn = self.primary_logical_head_lsn().max(self.cdc_current_lsn());
        self.prune_primary_replica_wal_segments_at(current_lsn)
    }

    fn prune_primary_replica_wal_segments_at(
        &self,
        current_lsn: u64,
    ) -> RedDBResult<Option<reddb_file::WalPruneResult>> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(None);
        };
        let Some(catalog) = self.primary_replica_slot_catalog()? else {
            return Ok(None);
        };
        let fork_lsns = self.primary_replica_fork_lsns()?;
        Ok(Some(
            plan.prune_wal_segments_with_fork_lsns(&catalog, &fork_lsns, current_lsn)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        ))
    }

    pub fn ack_primary_replica_lsn_and_prune(
        &self,
        replica_id: &str,
        applied_lsn: u64,
        durable_lsn: u64,
        apply_errors_total: u64,
        divergence_total: u64,
    ) -> RedDBResult<Option<reddb_file::WalPruneResult>> {
        let Some(repl) = self.inner.db.replication.as_ref() else {
            return Ok(None);
        };
        repl.ack_replica_lsn_with_observability(
            replica_id,
            applied_lsn,
            durable_lsn,
            apply_errors_total,
            divergence_total,
        );
        self.refresh_replication_flow_control();
        let current_lsn = self
            .primary_logical_head_lsn()
            .max(self.cdc_current_lsn())
            .max(applied_lsn)
            .max(durable_lsn);
        self.prune_primary_replica_wal_segments_at(current_lsn)
    }

    pub fn record_failover_timeline_promotion(
        &self,
        replica_id: &str,
        applied_lsn: u64,
    ) -> RedDBResult<reddb_file::TimelineHistory> {
        let Some(path) = self.primary_replica_timeline_history_path() else {
            return Ok(reddb_file::TimelineHistory::new(
                crate::utils::now_unix_millis(),
            ));
        };
        let now_ms = crate::utils::now_unix_millis();
        let history = match reddb_file::TimelineHistory::read_from_path(&path) {
            Ok(history) => history,
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                reddb_file::TimelineHistory::new(now_ms)
            }
            Err(err) => return Err(RedDBError::Internal(err.to_string())),
        };
        let parent = history
            .current()
            .unwrap_or_else(reddb_file::TimelineId::initial);
        let candidate = reddb_file::PromotionCandidate {
            replica_id: replica_id.to_string(),
            timeline: parent,
            received_lsn: applied_lsn,
            flushed_lsn: applied_lsn,
            applied_lsn,
        };
        let promoted = history
            .promotion_history(&candidate, parent.next(), now_ms)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        promoted
            .write_to_path(path)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(promoted)
    }

    pub fn record_replica_relay_batch(
        &self,
        replica_id: &str,
        records: &[(u64, Vec<u8>)],
        applied_lsn: u64,
    ) -> RedDBResult<()> {
        let Some(plan) = self.primary_replica_file_plan_result()? else {
            return Ok(());
        };
        let path = plan.relay_manifest_path(replica_id);
        let Some((first_lsn, _)) = records.first() else {
            return Ok(());
        };
        let end_lsn = records
            .iter()
            .map(|(lsn, _)| *lsn)
            .max()
            .unwrap_or(*first_lsn);
        let mut manifest = match reddb_file::ReplicaRelayLogManifest::read_from_path(&path) {
            Ok(manifest) => {
                let relay_dir = path.parent().ok_or_else(|| {
                    RedDBError::Internal("relay manifest path has no parent".into())
                })?;
                manifest
                    .validate_segments(relay_dir)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                manifest
            }
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                reddb_file::ReplicaRelayLogManifest::new(
                    replica_id,
                    reddb_file::TimelineId::initial(),
                )
            }
            Err(err) => return Err(RedDBError::Internal(err.to_string())),
        };
        if manifest.replica_id != replica_id {
            return Err(RedDBError::Internal(format!(
                "relay manifest replica_id {} does not match {}",
                manifest.replica_id, replica_id
            )));
        }
        if manifest.timeline != reddb_file::TimelineId::initial() {
            return Err(RedDBError::Internal(format!(
                "relay manifest timeline {} is not current timeline {}",
                manifest.timeline.0,
                reddb_file::TimelineId::initial().0
            )));
        }

        if end_lsn > manifest.flushed_lsn {
            let new_records = records
                .iter()
                .filter(|(lsn, _)| *lsn > manifest.flushed_lsn)
                .map(|(lsn, payload)| reddb_file::ReplicaRelayLogRecord::new(*lsn, payload.clone()))
                .collect::<Vec<_>>();
            let segment = reddb_file::ReplicaRelayLogSegment::from_records(
                reddb_file::TimelineId::initial(),
                new_records,
            )
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
            let start_lsn = segment.start_lsn;
            let end_lsn = segment.end_lsn;
            let relative_path = reddb_file::layout::relay_segment_relative_path(start_lsn, end_lsn);
            let segment_path = path
                .parent()
                .ok_or_else(|| RedDBError::Internal("relay manifest path has no parent".into()))?
                .join(&relative_path);
            segment
                .write_to_path(&segment_path)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            manifest
                .push_segment(
                    reddb_file::RelayLogSegmentRef::new(
                        relative_path,
                        start_lsn,
                        end_lsn,
                        segment
                            .checksum()
                            .map_err(|err| RedDBError::Internal(err.to_string()))?,
                    )
                    .map_err(|err| RedDBError::Internal(err.to_string()))?,
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        manifest
            .mark_applied(applied_lsn.min(manifest.flushed_lsn))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        manifest
            .write_to_path(path)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }
}
