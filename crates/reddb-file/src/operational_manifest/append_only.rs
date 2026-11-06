//! Append-only segment publication, validation, and retention.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::{
    active_append_only_segments, append_only_published_rows, empty_manifest, invalid_data,
    next_append_only_segment_id, record_retired_append_only_segment,
    AppendOnlySegmentManifestEntry, AppendOnlySegmentState, Manifest, OperationalManifest,
};
use crate::append_only_segment::{
    append_only_segment_chunk_checksums, decode_append_only_segment,
    decode_append_only_segment_with_limit, AppendOnlySegment, AppendOnlySegmentCodec,
    APPEND_ONLY_SEGMENT_CHUNK_BYTES,
};
use crate::{OpenMode, Vfs, VfsFile};

impl<V: Vfs> OperationalManifest<V> {
    pub fn publish_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
    ) -> io::Result<AppendOnlySegmentManifestEntry> {
        self.publish_append_only_segment_with_retention_bounds(
            collection, segment_id, codec, bytes, None, None,
        )
    }

    pub fn publish_append_only_segment_with_retention_bounds(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
        retention_min_ms: Option<i64>,
        retention_max_ms: Option<i64>,
    ) -> io::Result<AppendOnlySegmentManifestEntry> {
        self.publish_append_only_segment_with_retention_column(
            collection,
            segment_id,
            codec,
            bytes,
            (retention_min_ms, retention_max_ms),
            None,
        )
    }

    /// The caller must derive both bounds from every row using `timestamp_column`.
    pub fn publish_append_only_segment_with_retention_column(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
        retention_bounds: (Option<i64>, Option<i64>),
        timestamp_column: Option<&str>,
    ) -> io::Result<AppendOnlySegmentManifestEntry> {
        let (retention_min_ms, retention_max_ms) = retention_bounds;
        self.ensure_dirs()?;
        let decoded = decode_append_only_segment(bytes).map_err(invalid_data)?;
        if decoded.codec != codec {
            return Err(invalid_data("append-only segment codec metadata mismatch"));
        }
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        if manifest
            .append_only_segments
            .iter()
            .any(|entry| entry.collection == collection && entry.segment_id == segment_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("append-only segment already published: {collection}/{segment_id}"),
            ));
        }

        let path = self.append_only_segment_file_name(collection, segment_id);
        let segment_path = self.append_only_segments_dir().join(&path);
        if self.vfs.exists(&segment_path) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("closed append-only segment already exists: {path}"),
            ));
        }
        {
            let mut file = self.vfs.open(&segment_path, OpenMode::create_new())?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        self.vfs.sync_dir(&self.append_only_segments_dir())?;

        let entry = AppendOnlySegmentManifestEntry {
            collection: collection.to_string(),
            segment_id,
            state: AppendOnlySegmentState::Active,
            path,
            codec,
            chunk_size: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
            row_count: decoded.rows.len() as u64,
            retention_min_ms,
            retention_max_ms,
            retention_timestamp_column: timestamp_column
                .filter(|_| retention_min_ms.is_some() && retention_max_ms.is_some())
                .map(str::to_owned),
            primary_min: decoded.primary_min.as_ref().map(hex::encode),
            primary_max: decoded.primary_max.as_ref().map(hex::encode),
            primary_bloom: decoded.primary_bloom.clone(),
            chunk_checksums: append_only_segment_chunk_checksums(bytes),
        };
        manifest.append_only_segments.push(entry.clone());
        manifest
            .append_only_segments
            .sort_by(|a, b| (&a.collection, a.segment_id).cmp(&(&b.collection, b.segment_id)));
        let retired = manifest
            .append_only_retired
            .entry(collection.to_string())
            .or_default();
        retired.segment_high_water = retired.segment_high_water.max(segment_id);
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(entry)
    }

    pub fn begin_retire_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
    ) -> io::Result<bool> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(false),
        };
        let Some(entry) = manifest
            .append_only_segments
            .iter_mut()
            .find(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        if entry.state == AppendOnlySegmentState::PendingDrop {
            return Ok(true);
        }
        entry.state = AppendOnlySegmentState::PendingDrop;
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(true)
    }

    pub fn finish_retire_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
    ) -> io::Result<bool> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(false),
        };
        let Some(index) = manifest
            .append_only_segments
            .iter()
            .position(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        if manifest.append_only_segments[index].state != AppendOnlySegmentState::PendingDrop {
            manifest.append_only_segments[index].state = AppendOnlySegmentState::PendingDrop;
            manifest.generation += 1;
            self.publish(&manifest)?;
            manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        }
        let Some(index) = manifest
            .append_only_segments
            .iter()
            .position(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        self.remove_append_only_segment_file(&manifest.append_only_segments[index])?;
        self.vfs.sync_dir(&self.append_only_segments_dir())?;
        let entry = manifest.append_only_segments.remove(index);
        record_retired_append_only_segment(&mut manifest, &entry);
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(true)
    }

    pub(super) fn remove_append_only_segment_file(
        &self,
        entry: &AppendOnlySegmentManifestEntry,
    ) -> io::Result<()> {
        match self
            .vfs
            .remove_file(&self.append_only_segments_dir().join(&entry.path))
        {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Retire a bounded batch using one pending and one final manifest publish.
    /// The caller must serialize this operation with all manifest writers.
    pub fn retire_append_only_retention_batch(
        &self,
        cutoffs: &BTreeMap<String, (i64, String)>,
        limit: usize,
    ) -> io::Result<Vec<(String, u64)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(mut manifest) = self.load_current()? else {
            return Ok(Vec::new());
        };
        self.retire_append_only_retention_loaded(&mut manifest, cutoffs, limit)
    }

    pub(super) fn retire_append_only_retention_loaded(
        &self,
        manifest: &mut Manifest,
        cutoffs: &BTreeMap<String, (i64, String)>,
        limit: usize,
    ) -> io::Result<Vec<(String, u64)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut selected = Vec::new();
        let mut changed = false;
        for entry in &mut manifest.append_only_segments {
            let eligible = entry.state == AppendOnlySegmentState::PendingDrop
                || cutoffs
                    .get(&entry.collection)
                    .is_some_and(|(cutoff, column)| {
                        entry.retention_timestamp_column.as_ref() == Some(column)
                            && entry.retention_max_ms.is_some_and(|max| max < *cutoff)
                    });
            if !eligible {
                continue;
            }
            if entry.state == AppendOnlySegmentState::Active {
                entry.state = AppendOnlySegmentState::PendingDrop;
                changed = true;
            }
            selected.push((entry.collection.clone(), entry.segment_id));
            if selected.len() == limit {
                break;
            }
        }
        if selected.is_empty() {
            return Ok(selected);
        }
        if changed {
            manifest.generation += 1;
            self.publish(manifest)?;
        }
        let mut selected_set: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
        for (collection, segment_id) in &selected {
            selected_set
                .entry(collection)
                .or_default()
                .insert(*segment_id);
        }
        let is_selected = |entry: &AppendOnlySegmentManifestEntry| {
            selected_set
                .get(entry.collection.as_str())
                .is_some_and(|ids| ids.contains(&entry.segment_id))
        };
        for entry in &manifest.append_only_segments {
            if is_selected(entry) {
                self.remove_append_only_segment_file(entry)?;
            }
        }
        self.vfs.sync_dir(&self.append_only_segments_dir())?;
        let removed = manifest
            .append_only_segments
            .iter()
            .filter(|entry| is_selected(entry))
            .cloned()
            .collect::<Vec<_>>();
        manifest
            .append_only_segments
            .retain(|entry| !is_selected(entry));
        for entry in &removed {
            record_retired_append_only_segment(manifest, entry);
        }
        manifest.generation += 1;
        self.publish(manifest)?;
        Ok(selected)
    }

    pub fn append_only_segments_for_test(&self) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        self.append_only_segments()
    }

    pub fn append_only_segments(&self) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        Ok(self
            .load_current()?
            .map(|manifest| active_append_only_segments(&manifest))
            .unwrap_or_default())
    }

    /// Select one segment whose bounds were not proven for the current column.
    /// The cursor rotates over candidates, including ones that cannot be decoded.
    pub fn next_unverified_append_only_segment(
        &self,
        columns: &BTreeMap<String, (i64, String)>,
        after: Option<(&str, u64)>,
        max_manifest_bytes: usize,
    ) -> io::Result<Option<AppendOnlySegmentManifestEntry>> {
        let Some(manifest) = self.load_current_bounded(max_manifest_bytes)? else {
            return Ok(None);
        };
        Ok(next_unverified_from_manifest(&manifest, columns, after))
    }

    /// Read one immutable segment within explicit compressed and decoded limits.
    /// Returns `None` when the file exceeds the compressed-byte limit. A body
    /// over the decoded-byte limit is rejected without publishing bounds.
    pub fn read_append_only_segment_bounded(
        &self,
        entry: &AppendOnlySegmentManifestEntry,
        max_file_bytes: usize,
        max_uncompressed_bytes: usize,
    ) -> io::Result<Option<AppendOnlySegment>> {
        let path = self.append_only_segments_dir().join(&entry.path);
        let mut file = self.vfs.open(&path, OpenMode::read_only())?;
        let len = file.file_len()?;
        if len > max_file_bytes as u64 {
            return Ok(None);
        }
        let len = usize::try_from(len).map_err(invalid_data)?;
        let mut bytes = vec![0; len];
        file.read_exact(&mut bytes)?;
        if file.file_len()? != len as u64
            || append_only_segment_chunk_checksums(&bytes) != entry.chunk_checksums
        {
            return Err(invalid_data("append-only segment differs from manifest"));
        }
        let decoded = decode_append_only_segment_with_limit(&bytes, max_uncompressed_bytes)
            .map_err(invalid_data)?;
        if decoded.codec != entry.codec || decoded.rows.len() as u64 != entry.row_count {
            return Err(invalid_data("append-only segment metadata mismatch"));
        }
        Ok(Some(decoded))
    }

    /// Open an immutable artifact for resumable, checksum-verified reading.
    pub fn open_append_only_segment_stream(
        &self,
        entry: &AppendOnlySegmentManifestEntry,
        max_row_bytes: usize,
    ) -> io::Result<crate::append_only_segment::streaming::StreamingAppendOnlySegment<V::File>>
    {
        let path = self.append_only_segments_dir().join(&entry.path);
        let file = self.vfs.open(&path, OpenMode::read_only())?;
        crate::append_only_segment::streaming::StreamingAppendOnlySegment::new(
            file,
            entry.chunk_checksums.clone(),
            max_row_bytes,
        )
    }

    /// Publish bounds derived from every row of this exact immutable entry.
    /// The caller must fence manifest writers while validating and publishing.
    pub fn publish_revalidated_append_only_bounds(
        &self,
        expected: &AppendOnlySegmentManifestEntry,
        column: &str,
        bounds: (i64, i64),
        max_manifest_bytes: usize,
    ) -> io::Result<bool> {
        if bounds.0 > bounds.1 {
            return Err(invalid_data("invalid retention bounds"));
        }
        let Some(mut manifest) = self.load_current_bounded(max_manifest_bytes)? else {
            return Ok(false);
        };
        self.publish_revalidated_append_only_bounds_loaded(&mut manifest, expected, column, bounds)
    }

    pub(super) fn publish_revalidated_append_only_bounds_loaded(
        &self,
        manifest: &mut Manifest,
        expected: &AppendOnlySegmentManifestEntry,
        column: &str,
        bounds: (i64, i64),
    ) -> io::Result<bool> {
        if bounds.0 > bounds.1 {
            return Err(invalid_data("invalid retention bounds"));
        }
        let Some(entry) = manifest
            .append_only_segments
            .iter_mut()
            .find(|entry| *entry == expected)
        else {
            return Ok(false);
        };
        if entry.state != AppendOnlySegmentState::Active {
            return Ok(false);
        }
        entry.retention_min_ms = Some(bounds.0);
        entry.retention_max_ms = Some(bounds.1);
        entry.retention_timestamp_column = Some(column.to_owned());
        manifest.generation += 1;
        self.publish(manifest)?;
        Ok(true)
    }

    /// Select bounded retention candidates without cloning segment checksums,
    /// bloom filters or other payload-sized metadata into the runtime.
    pub fn append_only_retention_candidates(
        &self,
        cutoffs: &BTreeMap<String, (i64, String)>,
        limit: usize,
    ) -> io::Result<Vec<(String, u64)>> {
        if limit == 0 || cutoffs.is_empty() {
            return Ok(Vec::new());
        }
        let Some(manifest) = self.load_current()? else {
            return Ok(Vec::new());
        };
        Ok(manifest
            .append_only_segments
            .iter()
            .filter(|segment| segment.state == AppendOnlySegmentState::Active)
            .filter(|segment| {
                cutoffs
                    .get(&segment.collection)
                    .is_some_and(|(cutoff, column)| {
                        segment.retention_timestamp_column.as_ref() == Some(column)
                            && segment.retention_max_ms.is_some_and(|max| max < *cutoff)
                    })
            })
            .take(limit)
            .map(|segment| (segment.collection.clone(), segment.segment_id))
            .collect())
    }

    pub fn append_only_segments_with_pending_for_test(
        &self,
    ) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        Ok(self
            .load_current()?
            .map(|manifest| manifest.append_only_segments)
            .unwrap_or_default())
    }

    pub fn append_only_published_rows(&self, collection: &str) -> io::Result<u64> {
        Ok(self
            .load_current()?
            .map(|manifest| append_only_published_rows(&manifest, collection))
            .unwrap_or_default())
    }

    pub fn append_only_published_rows_for_test(&self, collection: &str) -> io::Result<u64> {
        self.append_only_published_rows(collection)
    }

    pub fn next_append_only_segment_id(&self, collection: &str) -> io::Result<u64> {
        Ok(self
            .load_current()?
            .map(|manifest| next_append_only_segment_id(&manifest, collection))
            .unwrap_or(1))
    }
}

pub(super) fn next_unverified_from_manifest(
    manifest: &Manifest,
    columns: &BTreeMap<String, (i64, String)>,
    after: Option<(&str, u64)>,
) -> Option<AppendOnlySegmentManifestEntry> {
    let eligible = |entry: &&AppendOnlySegmentManifestEntry| {
        entry.state == AppendOnlySegmentState::Active
            && columns.get(&entry.collection).is_some_and(|(_, column)| {
                entry.retention_timestamp_column.as_ref() != Some(column)
            })
    };
    let mut entries = manifest.append_only_segments.iter().filter(eligible);
    let first = entries.next()?;
    let mut earliest = first;
    let mut next = after
        .filter(|cursor| (first.collection.as_str(), first.segment_id) > *cursor)
        .map(|_| first);
    for entry in entries {
        let key = (entry.collection.as_str(), entry.segment_id);
        if key < (earliest.collection.as_str(), earliest.segment_id) {
            earliest = entry;
        }
        if after.is_some_and(|cursor| key > cursor)
            && next.is_none_or(|current| key < (current.collection.as_str(), current.segment_id))
        {
            next = Some(entry);
        }
    }
    Some(next.unwrap_or(earliest).clone())
}
