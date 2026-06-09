use super::*;

impl RedDBRuntime {
    /// Get backup scheduler status.
    pub fn backup_status(&self) -> crate::replication::scheduler::BackupStatus {
        self.inner.backup_scheduler.status()
    }

    /// Borrow the runtime's result Blob Cache.
    ///
    /// Wired for the `/admin/blob_cache/sweep` and
    /// `/admin/blob_cache/flush_namespace` HTTP handlers (issue #148
    /// follow-up): both delegate to
    /// `crate::storage::cache::sweeper::BlobCacheSweeper`, which takes a
    /// `&BlobCache`. Also used by `trigger_backup` when
    /// `red.config.backup.include_blob_cache=true` to locate the L2
    /// directory for archival.
    pub fn result_blob_cache(&self) -> &crate::storage::cache::BlobCache {
        &self.inner.result_blob_cache
    }

    /// Current local LSN paired with the LSN of the most recently
    /// archived WAL segment. The difference is the replication /
    /// archive lag operators alert on (PLAN.md Phase 5.1). Returns
    /// `(0, 0)` when neither replication nor archiving is configured.
    pub fn wal_archive_progress(&self) -> (u64, u64) {
        let current_lsn = self
            .inner
            .db
            .replication
            .as_ref()
            .map(|repl| {
                repl.logical_wal_spool
                    .as_ref()
                    .map(|spool| spool.current_lsn())
                    .unwrap_or_else(|| repl.wal_buffer.current_lsn())
            })
            .unwrap_or_else(|| self.inner.cdc.current_lsn());
        let last_archived_lsn = self.config_u64("red.config.timeline.last_archived_lsn", 0);
        (current_lsn, last_archived_lsn)
    }

    /// Trigger an immediate backup.
    pub fn trigger_backup(&self) -> RedDBResult<crate::replication::scheduler::BackupResult> {
        let result = (|| {
            self.check_write(crate::runtime::write_gate::WriteKind::Backup)?;
            // Defense in depth — check_write above already rejects when
            // the lease is NotHeld, but log + audit the lease angle here
            // explicitly so dashboards distinguish "lease lost" from a
            // generic read-only refusal.
            self.assert_remote_write_allowed("admin/backup")?;
            let started = std::time::Instant::now();
            let snapshot = self.create_snapshot()?;
            let mut uploaded = false;

            if let (Some(backend), Some(path)) =
                (&self.inner.db.remote_backend, self.inner.db.path())
            {
                let default_snapshot_prefix = self.inner.db.options().default_snapshot_prefix();
                let default_wal_prefix = self.inner.db.options().default_wal_archive_prefix();
                let default_head_key = self.inner.db.options().default_backup_head_key();
                let snapshot_prefix = self.config_string(
                    "red.config.backup.snapshot_prefix",
                    &default_snapshot_prefix,
                );
                let wal_prefix =
                    self.config_string("red.config.wal.archive.prefix", &default_wal_prefix);
                let head_key = self.config_string("red.config.backup.head_key", &default_head_key);
                let timeline_id = self.config_string("red.config.timeline.id", "main");
                let snapshot_key = crate::storage::wal::archive_snapshot(
                    backend.as_ref(),
                    path,
                    snapshot.snapshot_id,
                    &snapshot_prefix,
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
                let current_lsn = self
                    .inner
                    .db
                    .replication
                    .as_ref()
                    .map(|repl| {
                        repl.logical_wal_spool
                            .as_ref()
                            .map(|spool| spool.current_lsn())
                            .unwrap_or_else(|| repl.wal_buffer.current_lsn())
                    })
                    .unwrap_or_else(|| self.inner.cdc.current_lsn());
                let last_archived_lsn = self.config_u64("red.config.timeline.last_archived_lsn", 0);
                // Hash the local snapshot bytes so the manifest can carry
                // the digest for restore-side verification (PLAN.md
                // Phase 4). Failure to hash is non-fatal — we still
                // publish the manifest, just without a checksum, so a
                // future fix can backfill rather than losing the backup.
                let snapshot_sha256 = reddb_file::SnapshotManifest::compute_snapshot_sha256(path)
                    .map_err(|err| {
                        tracing::warn!(
                            target: "reddb::backup",
                            error = %err,
                            snapshot_id = snapshot.snapshot_id,
                            "snapshot hash failed; manifest will lack checksum"
                        );
                    })
                    .ok();
                let manifest = reddb_file::SnapshotManifest {
                    timeline_id: timeline_id.clone(),
                    snapshot_key: snapshot_key.clone(),
                    snapshot_id: snapshot.snapshot_id,
                    snapshot_time: snapshot.created_at_unix_ms as u64,
                    base_lsn: current_lsn,
                    schema_version: crate::api::REDDB_FORMAT_VERSION,
                    format_version: crate::api::REDDB_FORMAT_VERSION,
                    snapshot_sha256,
                };
                crate::storage::wal::publish_snapshot_manifest(backend.as_ref(), &manifest)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;

                // PLAN.md Phase 11.3 — read the head of the WAL hash chain
                // so the new segment can link back. `None` means we're
                // starting a fresh timeline (after a clean restore or on
                // first archive ever); the segment's `prev_hash` will be
                // `None` and restore-side validation accepts that only for
                // the first segment in `plan.wal_segments`.
                let prev_segment_hash =
                    self.config_string("red.config.timeline.last_segment_hash", "");
                let prev_hash_arg = if prev_segment_hash.is_empty() {
                    None
                } else {
                    Some(prev_segment_hash)
                };

                let archived_lsn = if let Some(primary) = &self.inner.db.replication {
                    let oldest = primary
                        .logical_wal_spool
                        .as_ref()
                        .and_then(|spool| spool.oldest_lsn().ok().flatten())
                        .or_else(|| primary.wal_buffer.oldest_lsn())
                        .unwrap_or(last_archived_lsn);
                    if last_archived_lsn > 0 && last_archived_lsn < oldest.saturating_sub(1) {
                        return Err(RedDBError::Internal(format!(
                            "logical WAL gap detected: last_archived_lsn={last_archived_lsn}, oldest_available_lsn={oldest}"
                        )));
                    }
                    let records = if let Some(spool) = &primary.logical_wal_spool {
                        spool
                            .read_since(last_archived_lsn, usize::MAX)
                            .map_err(|err| RedDBError::Internal(err.to_string()))?
                    } else {
                        primary.wal_buffer.read_since(last_archived_lsn, usize::MAX)
                    };
                    if let Some(meta) = crate::storage::wal::archive_change_records(
                        backend.as_ref(),
                        &wal_prefix,
                        &records,
                        prev_hash_arg,
                    )
                    .map_err(|err| RedDBError::Internal(err.to_string()))?
                    {
                        let _ = primary.prune_retained_wal_through(meta.lsn_end);
                        if let Err(err) = self.prune_primary_replica_wal_segments() {
                            tracing::warn!(
                                error = %err,
                                "failed to prune primary-replica WAL segments"
                            );
                        }
                        // Advance the chain head so the next archive call
                        // links to this segment's hash. If the segment has
                        // no sha256 (legacy / hashing failed) we leave the
                        // head as-is — the next segment then carries the
                        // prior chain head, preserving continuity.
                        if let Some(sha) = &meta.sha256 {
                            self.inner.db.store().set_config_tree(
                                "red.config.timeline",
                                &crate::json!({ "last_segment_hash": sha }),
                            );
                        }
                        meta.lsn_end
                    } else {
                        last_archived_lsn
                    }
                } else {
                    last_archived_lsn
                };

                let head = reddb_file::BackupHead {
                    timeline_id,
                    snapshot_key,
                    snapshot_id: snapshot.snapshot_id,
                    snapshot_time: snapshot.created_at_unix_ms as u64,
                    current_lsn,
                    last_archived_lsn: archived_lsn,
                    wal_prefix,
                };
                crate::storage::wal::publish_backup_head(backend.as_ref(), &head_key, &head)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                self.inner.db.store().set_config_tree(
                    "red.config.timeline",
                    &crate::json!({
                        "last_archived_lsn": archived_lsn,
                        "id": head.timeline_id
                    }),
                );

                // PLAN.md Phase 2.4 — refresh the unified `MANIFEST.json`
                // at the prefix root so external tooling sees a single
                // catalog of every snapshot + WAL segment with their
                // checksums. Best-effort: a manifest publish failure
                // doesn't fail the backup (the per-artifact sidecars
                // already give restore-side integrity), but it does log
                // so dashboards can flag stale catalogs.
                if let Err(err) = crate::storage::wal::publish_unified_manifest_for_prefix(
                    backend.as_ref(),
                    &snapshot_prefix,
                ) {
                    tracing::warn!(
                        target: "reddb::backup",
                        error = %err,
                        snapshot_prefix = %snapshot_prefix,
                        "unified MANIFEST.json refresh failed; per-artifact sidecars unaffected"
                    );
                }

                // PLAN.md Phase 11.4 — when the operator picked a
                // commit policy that demands replica durability, block
                // until the configured count of replicas has acked the
                // archived LSN (or the timeout fires). For backup the
                // policy decides the *DR posture* — `local` returns
                // immediately, `ack_n` ensures at least N replicas saw
                // the new tail before we report success to the
                // operator. A `TimedOut` is logged but does NOT fail
                // the backup: the local WAL + remote upload are durable
                // regardless; the missing acks are reported via
                // /metrics and /admin/status so the operator can decide.
                match self.commit_policy() {
                    crate::replication::CommitPolicy::AckN(n) if n > 0 => {
                        let timeout = std::env::var("RED_REPLICATION_ACK_TIMEOUT_MS")
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(5_000);
                        let outcome = self.await_replica_acks(
                            archived_lsn,
                            n,
                            std::time::Duration::from_millis(timeout),
                        );
                        match outcome {
                            crate::replication::AwaitOutcome::Reached(count) => {
                                tracing::debug!(
                                    target: "reddb::backup",
                                    archived_lsn,
                                    n,
                                    count,
                                    "ack_n: replicas synced before backup return"
                                );
                            }
                            crate::replication::AwaitOutcome::TimedOut { observed, required } => {
                                tracing::warn!(
                                    target: "reddb::backup",
                                    archived_lsn,
                                    observed,
                                    required,
                                    timeout_ms = timeout,
                                    "ack_n: timed out waiting for replicas; backup uploaded but DR posture degraded"
                                );
                            }
                            crate::replication::AwaitOutcome::NotRequired => {}
                        }
                    }
                    _ => {} // Local / RemoteWal / Quorum: no blocking yet
                }

                // Issue #148 follow-up — opt-in archive of the L2 Blob Cache
                // directory tree. Default off so a standard backup stays
                // small; flip via `red.config.backup.include_blob_cache=true`
                // when warm-cache restore is required (per
                // docs/operations/blob-cache-backup-restore.md §1).
                //
                // The L2 tree is *derived* state (ADR 0006) — its absence
                // never causes data loss; it only affects post-restore
                // p99 latency until the cache re-warms. We therefore log
                // (not fail) on per-file upload errors so a partial L2
                // upload never aborts a healthy snapshot+WAL backup.
                if self.config_bool("red.config.backup.include_blob_cache", false) {
                    let blob_cache_prefix = self.config_string(
                        "red.config.backup.blob_cache_prefix",
                        &format!("{snapshot_prefix}blob_cache/"),
                    );
                    if let Some(l2_path) = self.inner.result_blob_cache.l2_path() {
                        match crate::storage::cache::archive_blob_cache_l2(
                            backend.as_ref(),
                            l2_path,
                            &blob_cache_prefix,
                        ) {
                            Ok(count) => {
                                tracing::info!(
                                    target: "reddb::backup",
                                    files_uploaded = count,
                                    blob_cache_prefix = %blob_cache_prefix,
                                    "include_blob_cache: archived L2 directory"
                                );
                            }
                            Err(err) => {
                                tracing::warn!(
                                    target: "reddb::backup",
                                    error = %err,
                                    blob_cache_prefix = %blob_cache_prefix,
                                    "include_blob_cache: L2 archive failed; backup proceeding (cache is derived state)"
                                );
                            }
                        }
                    } else {
                        tracing::debug!(
                            target: "reddb::backup",
                            "include_blob_cache=true but no L2 path configured; nothing to archive"
                        );
                    }
                }

                uploaded = true;
            }

            Ok(crate::replication::scheduler::BackupResult {
                snapshot_id: snapshot.snapshot_id,
                uploaded,
                duration_ms: started.elapsed().as_millis() as u64,
                timestamp: snapshot.created_at_unix_ms as u64,
            })
        })();

        use crate::runtime::control_events::{EventKind, Outcome, Sensitivity};
        let (current_lsn, last_archived_lsn) = self.wal_archive_progress();
        let mut fields = vec![
            (
                "current_lsn".to_string(),
                Sensitivity::raw(current_lsn.to_string()),
            ),
            (
                "last_archived_lsn".to_string(),
                Sensitivity::raw(last_archived_lsn.to_string()),
            ),
        ];
        if let Ok(backup) = &result {
            fields.push((
                "snapshot_id".to_string(),
                Sensitivity::raw(backup.snapshot_id.to_string()),
            ));
            fields.push((
                "uploaded".to_string(),
                Sensitivity::raw(backup.uploaded.to_string()),
            ));
            fields.push((
                "duration_ms".to_string(),
                Sensitivity::raw(backup.duration_ms.to_string()),
            ));
            fields.push((
                "snapshot_time".to_string(),
                Sensitivity::raw(backup.timestamp.to_string()),
            ));
        }
        let outcome = match &result {
            Ok(_) => Outcome::Allowed,
            Err(err) => crate::runtime::impl_core::control_event_outcome_for_error(err),
        };
        let reason = result.as_ref().err().map(|err| err.to_string());
        self.emit_control_event(
            EventKind::BackupRun,
            outcome,
            "backup_trigger",
            Some("backup:trigger".to_string()),
            reason,
            fields,
        )?;
        result
    }
}
