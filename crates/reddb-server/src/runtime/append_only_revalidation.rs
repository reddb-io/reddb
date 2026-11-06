//! Resumable proof of retention bounds for one immutable segment.
use std::{collections::BTreeMap, io, sync::Arc};

use crate::storage::unified::manager::SegmentManager;
use crate::storage::{RedDB, UnifiedStore};
use crate::{RedDBError, RedDBResult};
use reddb_file::append_only_segment::streaming::StreamingAppendOnlySegment;
use reddb_file::operational_manifest::{AppendOnlySegmentManifestEntry, IncrementalManifest};
use reddb_file::{OperationalManifest, StdVfs, Vfs};

type SegmentFile = <StdVfs as Vfs>::File;
pub(crate) const FILE_BYTES_PER_TICK: usize = 1024 * 1024;
const PLAIN_BYTES_PER_TICK: usize = 1024 * 1024;
const ROWS_PER_TICK: usize = 1_000;
const MAX_ROW_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct AppendOnlyMaintenance {
    pub(crate) manifest: Option<IncrementalManifest<StdVfs>>,
    pub(crate) job: Option<AppendOnlyRevalidation>,
}

pub(crate) struct AppendOnlyRevalidation {
    pub(crate) entry: AppendOnlySegmentManifestEntry,
    pub(crate) column: String,
    manager: Arc<SegmentManager>,
    schema: Option<Arc<Vec<String>>>,
    reader: StreamingAppendOnlySegment<SegmentFile>,
    bounds: Option<(i64, i64)>,
}

impl AppendOnlyRevalidation {
    pub(crate) fn new(
        manifest: &OperationalManifest,
        entry: AppendOnlySegmentManifestEntry,
        column: String,
        manager: Arc<SegmentManager>,
    ) -> io::Result<Self> {
        let reader = manifest.open_append_only_segment_stream(&entry, MAX_ROW_BYTES)?;
        let schema = manager.column_schema();
        Ok(Self {
            entry,
            column,
            manager,
            schema,
            reader,
            bounds: None,
        })
    }

    pub(crate) fn matches(&self, column: &str, manager: &Arc<SegmentManager>) -> bool {
        self.column == column
            && Arc::ptr_eq(&self.manager, manager)
            && self.schema.as_deref() == manager.column_schema().as_deref()
    }

    pub(crate) fn step(&mut self, db: &RedDB, file_budget: usize) -> io::Result<bool> {
        let collection = &self.entry.collection;
        let column = &self.column;
        let bounds = &mut self.bounds;
        let progress =
            self.reader
                .step(file_budget, PLAIN_BYTES_PER_TICK, ROWS_PER_TICK, |row| {
                    let timestamp = db
                        .revalidate_append_only_row(collection, column, &row)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "ambiguous segment timestamp or identity",
                            )
                        })?;
                    *bounds = Some(bounds.map_or((timestamp, timestamp), |(min, max)| {
                        (min.min(timestamp), max.max(timestamp))
                    }));
                    Ok(())
                })?;
        if progress.complete {
            let metadata = self
                .reader
                .verified_metadata()
                .expect("completed segment has metadata");
            if metadata != (self.entry.codec, self.entry.row_count)
                || self.entry.chunk_size != reddb_file::APPEND_ONLY_SEGMENT_CHUNK_BYTES
                || self.bounds.is_none()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "segment metadata or bounds mismatch",
                ));
            }
        }
        Ok(progress.complete)
    }

    pub(crate) fn bounds(&self) -> Option<(i64, i64)> {
        self.bounds
    }
}

impl super::RedDBRuntime {
    /// The caller holds the publication fence across identity checks and writes.
    pub(super) fn maintain_append_only_tick(
        &self,
        manifest: &OperationalManifest,
        store: &UnifiedStore,
        cutoffs: &BTreeMap<String, (i64, String)>,
        limit: usize,
    ) -> RedDBResult<Vec<(String, u64)>> {
        let mut state = self.inner.append_only_revalidation.lock();
        if state.manifest.is_none() {
            state.manifest = manifest
                .begin_incremental_read()
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
        }
        let Some(snapshot) = state.manifest.as_mut() else {
            return Ok(Vec::new());
        };
        match snapshot.is_current() {
            Ok(true) => {}
            Ok(false) => {
                state.manifest = None;
                return Ok(Vec::new());
            }
            Err(error) => {
                state.manifest = None;
                return Err(RedDBError::Internal(error.to_string()));
            }
        }
        let progress = match snapshot.step(FILE_BYTES_PER_TICK) {
            Ok(progress) => progress,
            Err(error) => {
                state.manifest = None;
                return Err(RedDBError::Internal(error.to_string()));
            }
        };
        if !progress.complete {
            return Ok(Vec::new());
        }
        let file_budget = FILE_BYTES_PER_TICK - progress.bytes_read;
        let AppendOnlyMaintenance {
            manifest: snapshot,
            job,
        } = &mut *state;
        let snapshot = snapshot
            .as_mut()
            .expect("completed manifest snapshot exists");
        if job.as_ref().is_some_and(|current| {
            cutoffs
                .get(&current.entry.collection)
                .and_then(|(_, column)| {
                    store
                        .get_collection(&current.entry.collection)
                        .map(|manager| current.matches(column, &manager))
                })
                != Some(true)
        }) {
            *job = None;
        }
        if let Some(current) = job.as_ref() {
            if !snapshot
                .contains_append_only_segment(&current.entry)
                .map_err(|error| RedDBError::Internal(error.to_string()))?
            {
                *job = None;
            }
        }
        if job.is_none() && !cutoffs.is_empty() {
            let after = self
                .inner
                .retention_sweeper
                .read()
                .append_only_revalidation_after()
                .map(|(collection, id)| (collection.to_owned(), id));
            let entry = snapshot
                .next_unverified_append_only_segment(
                    cutoffs,
                    after
                        .as_ref()
                        .map(|(collection, id)| (collection.as_str(), *id)),
                )
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
            if let Some(entry) = entry {
                self.inner
                    .retention_sweeper
                    .write()
                    .advance_append_only_revalidation(entry.collection.clone(), entry.segment_id);
                if let Some(manager) = store.get_collection(&entry.collection) {
                    let column = cutoffs[&entry.collection].1.clone();
                    match AppendOnlyRevalidation::new(manifest, entry, column, manager) {
                        Ok(current) => *job = Some(current),
                        Err(error) => {
                            tracing::warn!(error = %error, "append-only segment could not be opened for revalidation")
                        }
                    }
                }
            }
        }
        if let Some(current) = job.as_mut() {
            match current.step(&self.inner.db, file_budget) {
                Ok(true) => {
                    let completed = job.take().expect("revalidation job exists");
                    let bounds = completed.bounds().expect("verified segment has bounds");
                    let result = snapshot.publish_revalidated_append_only_bounds(
                        &completed.entry,
                        &completed.column,
                        bounds,
                    );
                    state.manifest = None;
                    result.map_err(|error| RedDBError::Internal(error.to_string()))?;
                    return Ok(Vec::new());
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(collection = %current.entry.collection, segment_id = current.entry.segment_id, error = %error, "append-only segment could not be revalidated");
                    *job = None;
                }
            }
        }
        let result = snapshot.retire_append_only_retention_batch(cutoffs, limit);
        if !matches!(&result, Ok(entries) if entries.is_empty()) {
            state.manifest = None;
        }
        result.map_err(|error| RedDBError::Internal(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EntityId, UnifiedEntity};
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;

    #[test]
    fn append_only_retention_respects_tick_budget_across_cold_boot() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("append_only_retention_budget.rdb");
        let options = RedDBOptions::persistent(&db_path);
        let manifest = OperationalManifest::for_db_path(&db_path);
        {
            let rt = RedDBRuntime::with_options(options.clone()).unwrap();
            rt.background_tasks.as_ref().unwrap().shutdown();
            rt.execute_query("CREATE TABLE events (id INT) WITH timestamps = true APPEND ONLY")
                .unwrap();
            rt.execute_query("CREATE TABLE logs (id INT) WITH timestamps = true APPEND ONLY")
                .unwrap();
            for (collection, id) in [("events", 1), ("events", 2), ("logs", 3)] {
                rt.execute_query(&format!("INSERT INTO {collection} (id) VALUES ({id})"))
                    .unwrap();
                rt.flush().unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(2_100));
            rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
                .unwrap();
            rt.execute_query("ALTER COLLECTION logs SET RETENTION 1 s")
                .unwrap();
            rt.sweep_retention_tick(1);
            assert_eq!(manifest.append_only_segments_for_test().unwrap().len(), 2);
        }
        let rt = RedDBRuntime::with_options(options).unwrap();
        rt.background_tasks.as_ref().unwrap().shutdown();
        assert_eq!(manifest.append_only_segments_for_test().unwrap().len(), 2);
        rt.sweep_retention_tick(1);
        assert_eq!(manifest.append_only_segments_for_test().unwrap().len(), 1);
        rt.sweep_retention_tick(1);
        assert!(manifest.append_only_segments_for_test().unwrap().is_empty());
        rt.sweep_retention_tick(1);
        assert!(manifest.append_only_segments_for_test().unwrap().is_empty());
    }

    #[test]
    fn large_manifest_restarts_after_replacement_and_cold_boot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-manifest.rdb");
        let options = RedDBOptions::persistent(&path);
        let manifest = OperationalManifest::for_db_path(&path);
        let segment_path;
        {
            let rt = RedDBRuntime::with_options(options.clone()).unwrap();
            rt.background_tasks.as_ref().unwrap().shutdown();
            rt.execute_query("CREATE TABLE events (id INTEGER) WITH timestamps = true APPEND ONLY")
                .unwrap();
            let mut entity = UnifiedEntity::table_row(EntityId::new(1), "events", 1, vec![]);
            entity.created_at = 1;
            let rows = [reddb_file::AppendOnlySegmentRow {
                primary_key: 1_u64.to_be_bytes().to_vec(),
                payload: crate::storage::unified::store::UnifiedStore::serialize_entity_record(
                    &entity,
                    None,
                    reddb_file::STORE_VERSION_V10,
                ),
            }];
            let bytes = reddb_file::encode_append_only_segment(
                reddb_file::AppendOnlySegmentCodec::Zstd,
                &rows,
            )
            .unwrap();
            let entry = manifest
                .publish_append_only_segment_with_retention_bounds(
                    "events",
                    1,
                    reddb_file::AppendOnlySegmentCodec::Zstd,
                    &bytes,
                    Some(1),
                    Some(1),
                )
                .unwrap();
            segment_path = manifest.append_only_segment_path_for_test(&entry.path);
            rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
                .unwrap();
            let manifest_path = manifest.current_manifest_path_for_test();
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
            value["append_only_segments"][0]["primary_bloom"]["bits_hex"] =
                serde_json::Value::String("ab".repeat(600_000));
            value.as_object_mut().unwrap().remove("checksum");
            let checksum = crc32fast::hash(&serde_json::to_vec(&value).unwrap());
            value["checksum"] = serde_json::Value::String(format!("{checksum:08x}"));
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            assert!(std::fs::metadata(&manifest_path).unwrap().len() > FILE_BYTES_PER_TICK as u64);

            rt.sweep_retention_tick(1);
            assert!(
                segment_path.exists(),
                "a partially read manifest cannot authorize deletion"
            );
            assert!(manifest.append_only_segments().unwrap()[0]
                .retention_timestamp_column
                .is_none());
            // Replacing the manifest between ticks must invalidate the pinned
            // generation; neither its old metadata nor its partial read may be used.
            manifest.create_collection("other").unwrap();
            rt.sweep_retention_tick(1);
            assert!(segment_path.exists());
            assert!(manifest.append_only_segments().unwrap()[0]
                .retention_timestamp_column
                .is_none());
            rt.sweep_retention_tick(1);
            assert!(segment_path.exists());
        }
        {
            let rt = RedDBRuntime::with_options(options.clone()).unwrap();
            rt.background_tasks.as_ref().unwrap().shutdown();
            for _ in 0..12 {
                rt.sweep_retention_tick(1);
                if !segment_path.exists() {
                    break;
                }
            }
            assert!(
                !segment_path.exists(),
                "validation and retirement must resume after cold boot"
            );
            assert_eq!(manifest.append_only_published_rows("events").unwrap(), 1);
        }
        let rt = RedDBRuntime::with_options(options).unwrap();
        rt.background_tasks.as_ref().unwrap().shutdown();
        assert!(!segment_path.exists());
        assert_eq!(manifest.append_only_published_rows("events").unwrap(), 1);
    }

    #[test]
    fn large_legacy_segment_resumes_safely_after_cold_boot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-legacy.rdb");
        let options = RedDBOptions::persistent(&path);
        let manifest = OperationalManifest::for_db_path(&path);
        {
            let rt = RedDBRuntime::with_options(options.clone()).unwrap();
            rt.background_tasks.as_ref().unwrap().shutdown();
            rt.execute_query("CREATE TABLE events (id INTEGER) WITH timestamps = true APPEND ONLY")
                .unwrap();
            let mut random = 0x1234_5678_9abc_def0_u64;
            let rows = (1_u64..=16_000)
                .map(|id| {
                    let data = (0..256)
                        .map(|_| {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            random as u8
                        })
                        .collect::<Vec<_>>();
                    let mut entity = UnifiedEntity::table_row(
                        EntityId::new(id),
                        "events",
                        id,
                        vec![Value::Blob(data)],
                    );
                    entity.created_at = 1;
                    reddb_file::AppendOnlySegmentRow {
                        primary_key: id.to_be_bytes().to_vec(),
                        payload:
                            crate::storage::unified::store::UnifiedStore::serialize_entity_record(
                                &entity,
                                None,
                                reddb_file::STORE_VERSION_V10,
                            ),
                    }
                })
                .collect::<Vec<_>>();
            let bytes = reddb_file::encode_append_only_segment(
                reddb_file::AppendOnlySegmentCodec::Zstd,
                &rows,
            )
            .unwrap();
            assert!(bytes.len() > 1024 * 1024);
            let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
            let header: serde_json::Value =
                serde_json::from_slice(&bytes[28..28 + header_len]).unwrap();
            assert!(header["uncompressed_len"].as_u64().unwrap() > 8 * 1024 * 1024);
            let entry = manifest
                .publish_append_only_segment_with_retention_bounds(
                    "events",
                    1,
                    reddb_file::AppendOnlySegmentCodec::Zstd,
                    &bytes,
                    Some(1),
                    Some(1),
                )
                .unwrap();
            rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
                .unwrap();
            rt.sweep_retention_tick(1);
            let entries = manifest.append_only_segments().unwrap();
            assert_eq!(entries.len(), 1);
            assert!(
                entries[0].retention_timestamp_column.is_none(),
                "partial proof must never authorize deletion"
            );
            assert!(manifest
                .append_only_segment_path_for_test(&entry.path)
                .exists());
            rt.execute_query("CREATE TABLE concurrent_collection (id INTEGER)")
                .unwrap();
            rt.sweep_retention_tick(1);
            assert!(manifest.append_only_segments().unwrap()[0]
                .retention_timestamp_column
                .is_none());
        }
        {
            let rt = RedDBRuntime::with_options(options.clone()).unwrap();
            rt.background_tasks.as_ref().unwrap().shutdown();
            for _ in 0..80 {
                rt.sweep_retention_tick(1);
                if manifest.append_only_segments().unwrap().is_empty() {
                    break;
                }
            }
            assert!(
                manifest.append_only_segments().unwrap().is_empty(),
                "the restarted validation must finish"
            );
            assert_eq!(
                manifest.append_only_published_rows("events").unwrap(),
                16_000
            );
        }
        let rt = RedDBRuntime::with_options(options).unwrap();
        rt.background_tasks.as_ref().unwrap().shutdown();
        assert!(manifest.append_only_segments().unwrap().is_empty());
        assert_eq!(
            manifest.append_only_published_rows("events").unwrap(),
            16_000
        );
    }
}
