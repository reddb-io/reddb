use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaCatchupMode {
    WalOnly,
    BaseBackupThenWal,
    Reclone,
}

impl ReplicaCatchupMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WalOnly => "wal-only",
            Self::BaseBackupThenWal => "basebackup-then-wal",
            Self::Reclone => "reclone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRetentionPolicy {
    pub min_segments: u64,
    pub max_bytes: u64,
    pub keep_until_replicas_ack: bool,
}

impl Default for WalRetentionPolicy {
    fn default() -> Self {
        Self {
            min_segments: 16,
            max_bytes: 4 * 1024 * 1024 * 1024,
            keep_until_replicas_ack: true,
        }
    }
}

impl WalRetentionPolicy {
    pub fn should_offer_wal_only(&self, available_from_lsn: u64, replica_lsn: u64) -> bool {
        replica_lsn >= available_from_lsn
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRetentionPlan {
    pub oldest_required_lsn: Option<u64>,
    pub retained_bytes_before_prune: u64,
    pub retained_bytes_after_prune: u64,
    pub removable_segments: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalPruneResult {
    pub oldest_required_lsn: Option<u64>,
    pub retained_bytes_before_prune: u64,
    pub retained_bytes_after_prune: u64,
    pub removed_segments: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryReplicaWalRecord {
    pub sequence: u64,
    pub lsn: u64,
    pub payload: Vec<u8>,
}

impl PrimaryReplicaWalRecord {
    pub fn new(sequence: u64, lsn: u64, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            sequence,
            lsn,
            payload: payload.into(),
        }
    }
}

impl PrimaryReplicaFilePlan {
    pub fn append_wal_record(&self, lsn: u64, payload: &[u8]) -> RdbFileResult<PathBuf> {
        let segment_index = self.wal_segment_index(lsn);
        let segment_end = self.wal_segment_end_lsn(segment_index);
        let record_end = lsn
            .checked_add(1)
            .ok_or_else(|| RdbFileError::InvalidOperation("wal record lsn overflow".into()))?;
        if record_end > segment_end {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal record lsn range {lsn}..{record_end} crosses segment boundary {segment_end}"
            )));
        }

        let path = self.wal_segment_path(lsn);
        let mut segment = match PrimaryReplicaWalSegment::read_from_path(&path) {
            Ok(segment) => segment,
            Err(RdbFileError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                PrimaryReplicaWalSegment::new(
                    self.timeline,
                    segment_index,
                    self.wal_segment_start_lsn(segment_index),
                )
            }
            Err(err) => return Err(err),
        };
        if segment.timeline != self.timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal segment timeline {} does not match file plan timeline {}",
                segment.timeline.0, self.timeline.0
            )));
        }
        if segment.segment_index != segment_index {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal segment index {} does not match path index {}",
                segment.segment_index, segment_index
            )));
        }
        let sequence = segment
            .records
            .last()
            .map(|record| record.sequence.saturating_add(1))
            .unwrap_or(0);
        segment.push(PrimaryReplicaWalRecord::new(sequence, lsn, payload))?;
        segment.write_to_path(&path)?;
        Ok(path)
    }

    pub fn catchup_mode(&self, available_from_lsn: u64, replica_lsn: u64) -> ReplicaCatchupMode {
        if self
            .retention
            .should_offer_wal_only(available_from_lsn, replica_lsn)
        {
            ReplicaCatchupMode::WalOnly
        } else {
            ReplicaCatchupMode::BaseBackupThenWal
        }
    }

    pub fn catchup_mode_with_basebackups(
        &self,
        available_from_lsn: u64,
        replica_lsn: u64,
        basebackups: &[PrimaryReplicaBaseBackupManifest],
    ) -> ReplicaCatchupMode {
        if self
            .retention
            .should_offer_wal_only(available_from_lsn, replica_lsn)
        {
            return ReplicaCatchupMode::WalOnly;
        }
        if self
            .select_basebackup_for_catchup(available_from_lsn, basebackups)
            .is_some()
        {
            ReplicaCatchupMode::BaseBackupThenWal
        } else {
            ReplicaCatchupMode::Reclone
        }
    }

    pub fn select_basebackup_for_catchup<'a>(
        &self,
        available_from_lsn: u64,
        basebackups: &'a [PrimaryReplicaBaseBackupManifest],
    ) -> Option<&'a PrimaryReplicaBaseBackupManifest> {
        basebackups
            .iter()
            .filter(|manifest| manifest.timeline == self.timeline)
            .filter(|manifest| manifest.checkpoint_lsn >= available_from_lsn)
            .max_by_key(|manifest| manifest.checkpoint_lsn)
    }

    pub fn plan_wal_retention(
        &self,
        slots: &ReplicationSlotCatalog,
        current_lsn: u64,
    ) -> RdbFileResult<WalRetentionPlan> {
        self.plan_wal_retention_with_fork_lsns(slots, &[], current_lsn)
    }

    pub fn plan_wal_retention_with_fork_lsns(
        &self,
        slots: &ReplicationSlotCatalog,
        fork_lsns: &[u64],
        current_lsn: u64,
    ) -> RdbFileResult<WalRetentionPlan> {
        if slots.timeline != self.timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "slot catalog timeline {} does not match file plan timeline {}",
                slots.timeline.0, self.timeline.0
            )));
        }
        let replica_floor_lsn = slots.retention_floor_lsn();
        let fork_floor_lsn = fork_lsns.iter().copied().min();
        let oldest_required_lsn = min_optional_lsn(replica_floor_lsn, fork_floor_lsn);
        let current_segment = self.wal_segment_index(current_lsn);
        let keep_from_segment =
            current_segment.saturating_sub(self.retention.min_segments.saturating_sub(1));
        let mut retained_bytes_before_prune = 0u64;
        let mut removable_bytes = 0u64;
        let mut removable_segments: Vec<(u64, PathBuf, u64)> = Vec::new();
        let segments = self.existing_wal_segments()?;

        for (_, _, bytes) in &segments {
            retained_bytes_before_prune = retained_bytes_before_prune.saturating_add(*bytes);
        }

        for (segment_index, path, bytes) in &segments {
            if *segment_index >= keep_from_segment {
                continue;
            }
            if !self.wal_segment_released(*segment_index, replica_floor_lsn, fork_floor_lsn) {
                continue;
            }
            removable_bytes = removable_bytes.saturating_add(*bytes);
            removable_segments.push((*segment_index, path.clone(), *bytes));
        }

        if self.retention.max_bytes > 0 {
            let mut retained_after_prune =
                retained_bytes_before_prune.saturating_sub(removable_bytes);
            for (segment_index, path, bytes) in &segments {
                if retained_after_prune <= self.retention.max_bytes {
                    break;
                }
                if *segment_index >= current_segment {
                    continue;
                }
                if removable_segments
                    .iter()
                    .any(|(selected_index, _, _)| selected_index == segment_index)
                {
                    continue;
                }
                if !self.wal_segment_released(*segment_index, replica_floor_lsn, fork_floor_lsn) {
                    continue;
                }
                removable_bytes = removable_bytes.saturating_add(*bytes);
                retained_after_prune = retained_after_prune.saturating_sub(*bytes);
                removable_segments.push((*segment_index, path.clone(), *bytes));
            }
        }
        removable_segments.sort_by_key(|(segment_index, _, _)| *segment_index);
        Ok(WalRetentionPlan {
            oldest_required_lsn,
            retained_bytes_before_prune,
            retained_bytes_after_prune: retained_bytes_before_prune.saturating_sub(removable_bytes),
            removable_segments: removable_segments
                .into_iter()
                .map(|(_, path, _)| path)
                .collect(),
        })
    }

    pub fn prune_wal_segments(
        &self,
        slots: &ReplicationSlotCatalog,
        current_lsn: u64,
    ) -> RdbFileResult<WalPruneResult> {
        self.prune_wal_segments_with_fork_lsns(slots, &[], current_lsn)
    }

    pub fn prune_wal_segments_with_fork_lsns(
        &self,
        slots: &ReplicationSlotCatalog,
        fork_lsns: &[u64],
        current_lsn: u64,
    ) -> RdbFileResult<WalPruneResult> {
        let plan = self.plan_wal_retention_with_fork_lsns(slots, fork_lsns, current_lsn)?;
        let mut removed_segments = Vec::new();
        for path in &plan.removable_segments {
            fs::remove_file(path)?;
            removed_segments.push(path.clone());
        }
        if !removed_segments.is_empty() {
            if let Ok(dir) = File::open(self.wal_dir()) {
                let _ = dir.sync_all();
            }
        }
        Ok(WalPruneResult {
            oldest_required_lsn: plan.oldest_required_lsn,
            retained_bytes_before_prune: plan.retained_bytes_before_prune,
            retained_bytes_after_prune: plan.retained_bytes_after_prune,
            removed_segments,
        })
    }

    fn existing_wal_segments(&self) -> RdbFileResult<Vec<(u64, PathBuf, u64)>> {
        let wal_dir = self.wal_dir();
        let entries = match fs::read_dir(&wal_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut segments = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("redwal") {
                continue;
            }
            let Some(segment_index) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
            else {
                continue;
            };
            let bytes = entry.metadata()?.len();
            segments.push((segment_index, path, bytes));
        }
        segments.sort_by_key(|(segment_index, _, _)| *segment_index);
        Ok(segments)
    }

    fn wal_segment_released(
        &self,
        segment_index: u64,
        replica_floor_lsn: Option<u64>,
        fork_floor_lsn: Option<u64>,
    ) -> bool {
        if fork_floor_lsn
            .map(|floor| self.wal_segment_end_lsn(segment_index) > floor)
            .unwrap_or(false)
        {
            return false;
        }
        if !self.retention.keep_until_replicas_ack {
            return true;
        }
        let Some(floor) = replica_floor_lsn else {
            return false;
        };
        self.wal_segment_end_lsn(segment_index) <= floor
    }
}

fn min_optional_lsn(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryReplicaWalSegment {
    pub timeline: TimelineId,
    pub segment_index: u64,
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub records: Vec<PrimaryReplicaWalRecord>,
}

impl PrimaryReplicaWalSegment {
    pub fn new(timeline: TimelineId, segment_index: u64, start_lsn: u64) -> Self {
        Self {
            timeline,
            segment_index,
            start_lsn,
            end_lsn: start_lsn,
            records: Vec::new(),
        }
    }

    pub fn push(&mut self, record: PrimaryReplicaWalRecord) -> RdbFileResult<()> {
        let _payload_len = u32::try_from(record.payload.len()).map_err(|_| {
            RdbFileError::InvalidOperation("primary-replica wal payload too large".into())
        })?;
        if record.lsn < self.end_lsn {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal record lsn {} is before segment end {}",
                record.lsn, self.end_lsn
            )));
        }
        if let Some(previous) = self.records.last() {
            if record.sequence != previous.sequence + 1 {
                return Err(RdbFileError::InvalidOperation(format!(
                    "wal record sequence {} does not follow {}",
                    record.sequence, previous.sequence
                )));
            }
        }
        self.end_lsn = record
            .lsn
            .checked_add(1)
            .ok_or_else(|| RdbFileError::InvalidOperation("wal record lsn overflow".into()))?;
        self.records.push(record);
        Ok(())
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode()?)
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> RdbFileResult<Vec<u8>> {
        validate_segment(self)?;
        let mut records = Vec::new();
        let mut previous_frame_crc = 0u32;
        let mut payload_bytes = 0u64;
        for record in &self.records {
            let encoded = encode_wal_record(record, previous_frame_crc)?;
            previous_frame_crc = crc32(&encoded);
            payload_bytes = payload_bytes
                .checked_add(record.payload.len() as u64)
                .ok_or_else(|| {
                    RdbFileError::InvalidOperation("wal segment size overflow".into())
                })?;
            records.extend_from_slice(&encoded);
        }

        let mut out = Vec::new();
        out.extend_from_slice(PRIMARY_WAL_SEGMENT_MAGIC);
        put_u16(&mut out, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u64(&mut out, self.timeline.0);
        put_u64(&mut out, self.segment_index);
        put_u64(&mut out, self.start_lsn);
        put_u64(&mut out, self.end_lsn);
        put_u64(&mut out, self.records.len() as u64);
        put_u64(&mut out, payload_bytes);
        put_u32(&mut out, previous_frame_crc);
        out.extend_from_slice(&records);
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "primary-replica wal segment")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            PRIMARY_WAL_SEGMENT_MAGIC,
            "wal segment",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported primary-replica wal version {version}"
            )));
        }
        let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
        let segment_index = take_u64(bytes, &mut cursor, payload_end)?;
        let start_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let end_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let count = take_u64(bytes, &mut cursor, payload_end)? as usize;
        let payload_bytes = take_u64(bytes, &mut cursor, payload_end)?;
        let expected_tail_crc = take_u32(bytes, &mut cursor, payload_end)?;

        let mut records = Vec::with_capacity(count);
        let mut previous_frame_crc = 0u32;
        let mut actual_payload_bytes = 0u64;
        for _ in 0..count {
            let record_start = cursor;
            let record = decode_wal_record(bytes, &mut cursor, payload_end, previous_frame_crc)?;
            previous_frame_crc = crc32(&bytes[record_start..cursor]);
            actual_payload_bytes = actual_payload_bytes
                .checked_add(record.payload.len() as u64)
                .ok_or_else(|| {
                    RdbFileError::InvalidOperation("wal segment size overflow".into())
                })?;
            records.push(record);
        }
        if cursor != payload_end {
            return Err(RdbFileError::InvalidOperation(
                "primary-replica wal segment has trailing bytes".into(),
            ));
        }
        if previous_frame_crc != expected_tail_crc {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal segment crc chain mismatch: stored {expected_tail_crc:#010x}, computed {previous_frame_crc:#010x}"
            )));
        }
        if payload_bytes != actual_payload_bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal segment payload bytes mismatch: stored {payload_bytes}, computed {actual_payload_bytes}"
            )));
        }
        let segment = Self {
            timeline,
            segment_index,
            start_lsn,
            end_lsn,
            records,
        };
        validate_segment(&segment)?;
        Ok(segment)
    }
}

fn validate_segment(segment: &PrimaryReplicaWalSegment) -> RdbFileResult<()> {
    let mut expected_lsn = segment.start_lsn;
    for (index, record) in segment.records.iter().enumerate() {
        if record.lsn < expected_lsn {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal record lsn {} overlaps previous end {}",
                record.lsn, expected_lsn
            )));
        }
        if index > 0 && record.sequence != segment.records[index - 1].sequence + 1 {
            return Err(RdbFileError::InvalidOperation(format!(
                "wal record sequence {} does not follow {}",
                record.sequence,
                segment.records[index - 1].sequence
            )));
        }
        expected_lsn = record
            .lsn
            .checked_add(1)
            .ok_or_else(|| RdbFileError::InvalidOperation("wal record lsn overflow".into()))?;
    }
    if expected_lsn != segment.end_lsn {
        return Err(RdbFileError::InvalidOperation(format!(
            "wal segment end lsn mismatch: stored {}, computed {}",
            segment.end_lsn, expected_lsn
        )));
    }
    Ok(())
}

fn encode_wal_record(
    record: &PrimaryReplicaWalRecord,
    previous_frame_crc: u32,
) -> RdbFileResult<Vec<u8>> {
    let payload_len = u32::try_from(record.payload.len()).map_err(|_| {
        RdbFileError::InvalidOperation("primary-replica wal payload too large".into())
    })?;
    let mut header = Vec::with_capacity(WAL_RECORD_HEADER_BYTES);
    header.extend_from_slice(WAL_RECORD_MAGIC);
    put_u16(&mut header, PRIMARY_REPLICA_ARTIFACT_VERSION);
    put_u16(&mut header, 0);
    put_u64(&mut header, record.sequence);
    put_u64(&mut header, record.lsn);
    put_u32(&mut header, payload_len);
    put_u32(&mut header, crc32(&record.payload));
    put_u32(&mut header, previous_frame_crc);
    let header_crc = crc32(&header);
    put_u32(&mut header, header_crc);
    let mut out = header;
    out.extend_from_slice(&record.payload);
    Ok(out)
}

fn decode_wal_record(
    bytes: &[u8],
    cursor: &mut usize,
    payload_end: usize,
    expected_previous_crc: u32,
) -> RdbFileResult<PrimaryReplicaWalRecord> {
    let header_start = *cursor;
    expect_magic(bytes, cursor, payload_end, WAL_RECORD_MAGIC, "wal record")?;
    let version = take_u16(bytes, cursor, payload_end)?;
    if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
        return Err(RdbFileError::InvalidOperation(format!(
            "unsupported primary-replica wal record version {version}"
        )));
    }
    let _flags = take_u16(bytes, cursor, payload_end)?;
    let sequence = take_u64(bytes, cursor, payload_end)?;
    let lsn = take_u64(bytes, cursor, payload_end)?;
    let payload_len = take_u32(bytes, cursor, payload_end)? as usize;
    let payload_crc = take_u32(bytes, cursor, payload_end)?;
    let previous_frame_crc = take_u32(bytes, cursor, payload_end)?;
    let header_crc = take_u32(bytes, cursor, payload_end)?;
    let computed_header_crc =
        crc32(&bytes[header_start..header_start + WAL_RECORD_HEADER_BYTES - 4]);
    if header_crc != computed_header_crc {
        return Err(RdbFileError::InvalidOperation(format!(
            "wal record header checksum mismatch: stored {header_crc:#010x}, computed {computed_header_crc:#010x}"
        )));
    }
    if previous_frame_crc != expected_previous_crc {
        return Err(RdbFileError::InvalidOperation(format!(
            "wal record previous crc mismatch: stored {previous_frame_crc:#010x}, expected {expected_previous_crc:#010x}"
        )));
    }
    let payload = take_bytes(bytes, cursor, payload_end, payload_len)?.to_vec();
    let computed_payload_crc = crc32(&payload);
    if payload_crc != computed_payload_crc {
        return Err(RdbFileError::InvalidOperation(format!(
            "wal record payload checksum mismatch: stored {payload_crc:#010x}, computed {computed_payload_crc:#010x}"
        )));
    }
    Ok(PrimaryReplicaWalRecord {
        sequence,
        lsn,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "reddb_file_primary_replica_wal_{name}_{}_{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn wal_retention_floor_accounts_for_live_fork_lsns() {
        let root = temp_root("fork_floor");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1))
            .with_segment_bytes(1024 * 1024)
            .with_retention(WalRetentionPolicy {
                min_segments: 1,
                max_bytes: 1024 * 1024 * 1024,
                keep_until_replicas_ack: false,
            });
        for index in 0..5 {
            let path = plan.wal_segment_path(index * plan.segment_bytes);
            write_bytes_atomically(&path, &[index as u8]).expect("write fake redwal");
        }
        let catalog = ReplicationSlotCatalog::new(TimelineId(1));

        let retention = plan
            .plan_wal_retention_with_fork_lsns(
                &catalog,
                &[2 * plan.segment_bytes],
                5 * plan.segment_bytes,
            )
            .expect("plan retention");

        assert_eq!(retention.oldest_required_lsn, Some(2 * plan.segment_bytes));
        assert_eq!(retention.retained_bytes_before_prune, 5);
        assert_eq!(retention.retained_bytes_after_prune, 3);
        assert_eq!(retention.removable_segments.len(), 2);
        assert_eq!(retention.removable_segments[0], plan.wal_segment_path(0));
        assert_eq!(
            retention.removable_segments[1],
            plan.wal_segment_path(plan.segment_bytes)
        );

        let _ = fs::remove_dir_all(root);
    }
}
