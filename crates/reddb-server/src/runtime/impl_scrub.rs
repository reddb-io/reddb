use super::*;

const SCRUB_COLUMNS: [&str; 15] = [
    "row_kind",
    "zone_kind",
    "physical_identity",
    "collection",
    "expected_checksum",
    "actual_checksum",
    "fault_class",
    "objects_verified",
    "superblock_verified",
    "manifest_verified",
    "wal_verified",
    "page_verified",
    "segment_chunk_verified",
    "bytes_read",
    "duration_ms",
];

impl RedDBRuntime {
    pub(crate) fn execute_scrub_query(
        &self,
        query: &str,
        mode: QueryMode,
        background: bool,
        budget: Option<u64>,
    ) -> RedDBResult<RuntimeQueryResult> {
        let Some(path) = self.inner.db.path() else {
            return Err(RedDBError::InvalidOperation(
                "SCRUB requires a persistent store".to_string(),
            ));
        };
        let max_objects = if background {
            budget.unwrap_or(1).max(1) as usize
        } else {
            usize::MAX
        };
        let start_cursor = if background {
            self.inner.scrub_state.lock().background_cursor
        } else {
            0
        };
        let report = reddb_file::scrub_embedded_store(path, start_cursor, max_objects)
            .map_err(|err| RedDBError::Internal(format!("SCRUB failed: {err}")))?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        {
            let mut state = self.inner.scrub_state.lock();
            state.last_run_unix_ms = now_ms;
            state.last_findings_count = report.findings.len() as u64;
            state.verified = report.verified.clone();
            if background {
                state.background_cursor = if report.complete {
                    0
                } else {
                    report.next_cursor
                };
                state.background_status = if report.complete {
                    "complete".to_string()
                } else {
                    "running".to_string()
                };
                state.background_verified_objects = if report.complete {
                    report.total_objects
                } else {
                    report.next_cursor as u64
                };
                state.background_total_objects = report.total_objects;
            }
        }

        let result = scrub_result_rows(&report);
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode,
            statement: if background {
                "scrub_background"
            } else {
                "scrub"
            },
            engine: "runtime-scrub",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    pub(crate) fn scrub_stats_snapshot(&self) -> ScrubStatsSnapshot {
        self.inner.scrub_state.lock().snapshot()
    }
}

fn scrub_result_rows(report: &reddb_file::StorageScrubReport) -> UnifiedResult {
    let mut result =
        UnifiedResult::with_columns(SCRUB_COLUMNS.iter().map(|s| s.to_string()).collect());
    for finding in &report.findings {
        let mut record = UnifiedRecord::new();
        record.set("row_kind", Value::text("finding"));
        record.set("zone_kind", Value::text(finding.zone_kind.as_str()));
        record.set(
            "physical_identity",
            Value::text(finding.physical_identity.as_str()),
        );
        record.set(
            "collection",
            finding
                .collection
                .as_ref()
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
        );
        record.set(
            "expected_checksum",
            finding
                .expected_checksum
                .as_ref()
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
        );
        record.set(
            "actual_checksum",
            finding
                .actual_checksum
                .as_ref()
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
        );
        record.set(
            "fault_class",
            finding
                .fault_class
                .as_ref()
                .map(|value| Value::text(value.as_str()))
                .unwrap_or(Value::Null),
        );
        record.set(
            "objects_verified",
            Value::UnsignedInteger(report.objects_verified),
        );
        set_verified_counter_fields(&mut record, &report.verified);
        record.set("bytes_read", Value::UnsignedInteger(report.bytes_read));
        record.set("duration_ms", Value::UnsignedInteger(report.duration_ms));
        result.push(record);
    }

    let mut summary = UnifiedRecord::new();
    summary.set("row_kind", Value::text("summary"));
    summary.set("zone_kind", Value::text("summary"));
    summary.set("physical_identity", Value::text("store"));
    summary.set("collection", Value::Null);
    summary.set("expected_checksum", Value::Null);
    summary.set("actual_checksum", Value::Null);
    summary.set(
        "fault_class",
        if report.complete {
            Value::Null
        } else {
            Value::text("in-progress")
        },
    );
    summary.set(
        "objects_verified",
        Value::UnsignedInteger(report.objects_verified),
    );
    set_verified_counter_fields(&mut summary, &report.verified);
    summary.set("bytes_read", Value::UnsignedInteger(report.bytes_read));
    summary.set("duration_ms", Value::UnsignedInteger(report.duration_ms));
    result.push(summary);
    result
}

fn set_verified_counter_fields(
    record: &mut UnifiedRecord,
    verified: &reddb_file::StorageScrubVerifiedCounters,
) {
    record.set(
        "superblock_verified",
        Value::UnsignedInteger(verified.superblock),
    );
    record.set(
        "manifest_verified",
        Value::UnsignedInteger(verified.manifest),
    );
    record.set("wal_verified", Value::UnsignedInteger(verified.wal));
    record.set("page_verified", Value::UnsignedInteger(verified.page));
    record.set(
        "segment_chunk_verified",
        Value::UnsignedInteger(verified.segment_chunk),
    );
}
