use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaRelayLogRecord {
    pub lsn: u64,
    pub payload: Vec<u8>,
}

impl ReplicaRelayLogRecord {
    pub fn new(lsn: u64, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            lsn,
            payload: payload.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaRelayLogSegment {
    pub timeline: TimelineId,
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub records: Vec<ReplicaRelayLogRecord>,
}

impl ReplicaRelayLogSegment {
    pub fn from_records(
        timeline: TimelineId,
        records: Vec<ReplicaRelayLogRecord>,
    ) -> RdbFileResult<Self> {
        let first = records.first().ok_or_else(|| {
            RdbFileError::InvalidOperation("relay segment cannot be empty".into())
        })?;
        let end_lsn = records
            .iter()
            .map(|record| record.lsn)
            .max()
            .unwrap_or(first.lsn);
        let segment = Self {
            timeline,
            start_lsn: first.lsn,
            end_lsn,
            records,
        };
        segment.validate()?;
        Ok(segment)
    }

    pub fn checksum(&self) -> RdbFileResult<u32> {
        Ok(crc32(&self.encode()?))
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode()?)
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> RdbFileResult<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(RELAY_LOG_SEGMENT_MAGIC);
        put_u16(&mut out, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u64(&mut out, self.timeline.0);
        put_u64(&mut out, self.start_lsn);
        put_u64(&mut out, self.end_lsn);
        put_u64(&mut out, self.records.len() as u64);
        for record in &self.records {
            put_u64(&mut out, record.lsn);
            let payload_len = u32::try_from(record.payload.len()).map_err(|_| {
                RdbFileError::InvalidOperation("relay record payload too large".into())
            })?;
            put_u32(&mut out, payload_len);
            out.extend_from_slice(&record.payload);
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "replica relay log segment")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            RELAY_LOG_SEGMENT_MAGIC,
            "replica relay log segment",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported relay log segment version {version}"
            )));
        }
        let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
        let start_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let end_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let count = take_u64(bytes, &mut cursor, payload_end)?;
        let mut records = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let payload_len = take_u32(bytes, &mut cursor, payload_end)? as usize;
            let payload = take_bytes(bytes, &mut cursor, payload_end, payload_len)?.to_vec();
            records.push(ReplicaRelayLogRecord { lsn, payload });
        }
        reject_trailing_bytes(bytes, cursor, payload_end, "replica relay log segment")?;
        let segment = Self {
            timeline,
            start_lsn,
            end_lsn,
            records,
        };
        segment.validate()?;
        Ok(segment)
    }

    fn validate(&self) -> RdbFileResult<()> {
        if self.records.is_empty() {
            return Err(RdbFileError::InvalidOperation(
                "relay segment cannot be empty".into(),
            ));
        }
        if self.end_lsn < self.start_lsn {
            return Err(RdbFileError::InvalidOperation(
                "relay segment end lsn is before start lsn".into(),
            ));
        }
        let mut previous = None;
        for record in &self.records {
            if record.lsn < self.start_lsn || record.lsn > self.end_lsn {
                return Err(RdbFileError::InvalidOperation(format!(
                    "relay record lsn {} is outside segment range {}..={}",
                    record.lsn, self.start_lsn, self.end_lsn
                )));
            }
            if previous.map(|lsn| record.lsn <= lsn).unwrap_or(false) {
                return Err(RdbFileError::InvalidOperation(
                    "relay segment records are not strictly ordered".into(),
                ));
            }
            previous = Some(record.lsn);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLogSegmentRef {
    pub relative_path: PathBuf,
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub checksum: u32,
}

impl RelayLogSegmentRef {
    pub fn new(
        relative_path: impl Into<PathBuf>,
        start_lsn: u64,
        end_lsn: u64,
        checksum: u32,
    ) -> RdbFileResult<Self> {
        let relative_path = relative_path.into();
        validate_relay_relative_path(&relative_path)?;
        if end_lsn < start_lsn {
            return Err(RdbFileError::InvalidOperation(
                "relay segment end lsn is before start lsn".into(),
            ));
        }
        Ok(Self {
            relative_path,
            start_lsn,
            end_lsn,
            checksum,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaRelayLogManifest {
    pub replica_id: String,
    pub timeline: TimelineId,
    pub received_lsn: u64,
    pub flushed_lsn: u64,
    pub applied_lsn: u64,
    pub segments: Vec<RelayLogSegmentRef>,
}

impl ReplicaRelayLogManifest {
    pub fn new(replica_id: impl Into<String>, timeline: TimelineId) -> Self {
        Self {
            replica_id: replica_id.into(),
            timeline,
            received_lsn: 0,
            flushed_lsn: 0,
            applied_lsn: 0,
            segments: Vec::new(),
        }
    }

    pub fn push_segment(&mut self, segment: RelayLogSegmentRef) -> RdbFileResult<()> {
        if let Some(previous) = self.segments.last() {
            if segment.start_lsn < previous.end_lsn {
                return Err(RdbFileError::InvalidOperation(
                    "relay segment overlaps previous segment".into(),
                ));
            }
        }
        self.received_lsn = self.received_lsn.max(segment.end_lsn);
        self.flushed_lsn = self.flushed_lsn.max(segment.end_lsn);
        self.segments.push(segment);
        Ok(())
    }

    pub fn mark_applied(&mut self, applied_lsn: u64) -> RdbFileResult<()> {
        if applied_lsn > self.flushed_lsn {
            return Err(RdbFileError::InvalidOperation(format!(
                "relay applied lsn {applied_lsn} is beyond flushed lsn {}",
                self.flushed_lsn
            )));
        }
        self.applied_lsn = self.applied_lsn.max(applied_lsn);
        Ok(())
    }

    pub fn ack(&self) -> RdbFileResult<ReplicaAck> {
        ReplicaAck::with_positions(
            &self.replica_id,
            self.timeline,
            self.received_lsn,
            self.flushed_lsn,
            self.flushed_lsn,
            self.applied_lsn,
        )
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode()?)
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn validate_segments(&self, relay_dir: impl AsRef<Path>) -> RdbFileResult<()> {
        let relay_dir = relay_dir.as_ref();
        for segment_ref in &self.segments {
            validate_relay_relative_path(&segment_ref.relative_path)?;
            let segment =
                ReplicaRelayLogSegment::read_from_path(relay_dir.join(&segment_ref.relative_path))?;
            if segment.timeline != self.timeline {
                return Err(RdbFileError::InvalidOperation(format!(
                    "relay segment timeline {} does not match manifest timeline {}",
                    segment.timeline.0, self.timeline.0
                )));
            }
            if segment.start_lsn != segment_ref.start_lsn || segment.end_lsn != segment_ref.end_lsn
            {
                return Err(RdbFileError::InvalidOperation(format!(
                    "relay segment range {}..{} does not match manifest range {}..{}",
                    segment.start_lsn, segment.end_lsn, segment_ref.start_lsn, segment_ref.end_lsn
                )));
            }
            let checksum = segment.checksum()?;
            if checksum != segment_ref.checksum {
                return Err(RdbFileError::InvalidOperation(format!(
                    "relay segment checksum mismatch: stored {:#010x}, computed {:#010x}",
                    segment_ref.checksum, checksum
                )));
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> RdbFileResult<Vec<u8>> {
        if self.applied_lsn > self.flushed_lsn || self.flushed_lsn > self.received_lsn {
            return Err(RdbFileError::InvalidOperation(
                "relay manifest positions are not ordered".into(),
            ));
        }
        let mut out = Vec::new();
        out.extend_from_slice(RELAY_LOG_MANIFEST_MAGIC);
        put_u16(&mut out, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_string(&mut out, &self.replica_id);
        put_u64(&mut out, self.timeline.0);
        put_u64(&mut out, self.received_lsn);
        put_u64(&mut out, self.flushed_lsn);
        put_u64(&mut out, self.applied_lsn);
        put_u32(&mut out, self.segments.len() as u32);
        for segment in &self.segments {
            put_string(&mut out, &segment.relative_path.to_string_lossy());
            put_u64(&mut out, segment.start_lsn);
            put_u64(&mut out, segment.end_lsn);
            put_u32(&mut out, segment.checksum);
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "replica relay log manifest")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            RELAY_LOG_MANIFEST_MAGIC,
            "replica relay log manifest",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported relay log manifest version {version}"
            )));
        }
        let replica_id = take_string(bytes, &mut cursor, payload_end)?;
        let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
        let received_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let flushed_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let applied_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let count = take_u32(bytes, &mut cursor, payload_end)? as usize;
        let mut manifest = Self {
            replica_id,
            timeline,
            received_lsn,
            flushed_lsn,
            applied_lsn,
            segments: Vec::with_capacity(count),
        };
        for _ in 0..count {
            let relative_path = take_string(bytes, &mut cursor, payload_end)?;
            let start_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let end_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let checksum = take_u32(bytes, &mut cursor, payload_end)?;
            manifest.segments.push(RelayLogSegmentRef::new(
                relative_path,
                start_lsn,
                end_lsn,
                checksum,
            )?);
        }
        reject_trailing_bytes(bytes, cursor, payload_end, "replica relay log manifest")?;
        if manifest.applied_lsn > manifest.flushed_lsn
            || manifest.flushed_lsn > manifest.received_lsn
        {
            return Err(RdbFileError::InvalidOperation(
                "relay manifest positions are not ordered".into(),
            ));
        }
        Ok(manifest)
    }
}

fn validate_relay_relative_path(path: &Path) -> RdbFileResult<()> {
    if path.is_absolute() {
        return Err(RdbFileError::InvalidOperation(
            "relay segment path must be relative".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RdbFileError::InvalidOperation(
            "relay segment path must not escape relay directory".into(),
        ));
    }
    Ok(())
}
