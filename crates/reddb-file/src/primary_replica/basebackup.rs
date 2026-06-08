use super::*;

const MAX_BASEBACKUP_CHUNKS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseBackupPlan {
    pub timeline: TimelineId,
    pub start_lsn: u64,
    pub checkpoint_lsn: u64,
}

impl BaseBackupPlan {
    pub fn new(timeline: TimelineId, start_lsn: u64, checkpoint_lsn: u64) -> Self {
        Self {
            timeline,
            start_lsn,
            checkpoint_lsn,
        }
    }

    pub fn is_valid(self) -> bool {
        self.checkpoint_lsn >= self.start_lsn
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryReplicaBaseBackupManifest {
    pub timeline: TimelineId,
    pub start_lsn: u64,
    pub checkpoint_lsn: u64,
    pub snapshot_relative_path: PathBuf,
    pub snapshot_bytes: u64,
    pub snapshot_checksum: u32,
    pub chunks: Vec<BaseBackupChunkRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBackupChunkRef {
    pub ordinal: u32,
    pub snapshot_offset: u64,
    pub bytes: u64,
    pub checksum: u32,
    pub relative_path: PathBuf,
}

impl BaseBackupChunkRef {
    pub fn new(
        ordinal: u32,
        snapshot_offset: u64,
        bytes: u64,
        checksum: u32,
        relative_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            ordinal,
            snapshot_offset,
            bytes,
            checksum,
            relative_path: relative_path.into(),
        }
    }
}

impl PrimaryReplicaBaseBackupManifest {
    pub fn new(
        backup: BaseBackupPlan,
        snapshot_relative_path: impl Into<PathBuf>,
        snapshot_bytes: u64,
        snapshot_checksum: u32,
    ) -> RdbFileResult<Self> {
        let snapshot_relative_path = snapshot_relative_path.into();
        let chunk = BaseBackupChunkRef::new(
            0,
            0,
            snapshot_bytes,
            snapshot_checksum,
            snapshot_relative_path.clone(),
        );
        Self::incremental(
            backup,
            snapshot_relative_path,
            snapshot_bytes,
            snapshot_checksum,
            vec![chunk],
        )
    }

    pub fn incremental(
        backup: BaseBackupPlan,
        snapshot_relative_path: impl Into<PathBuf>,
        snapshot_bytes: u64,
        snapshot_checksum: u32,
        chunks: Vec<BaseBackupChunkRef>,
    ) -> RdbFileResult<Self> {
        if !backup.is_valid() {
            return Err(RdbFileError::InvalidOperation(
                "base backup checkpoint lsn is before start lsn".into(),
            ));
        }
        let manifest = Self {
            timeline: backup.timeline,
            start_lsn: backup.start_lsn,
            checkpoint_lsn: backup.checkpoint_lsn,
            snapshot_relative_path: snapshot_relative_path.into(),
            snapshot_bytes,
            snapshot_checksum,
            chunks,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> RdbFileResult<()> {
        validate_relative_path(&self.snapshot_relative_path, "base backup snapshot")?;
        if self.chunks.is_empty() {
            return Err(RdbFileError::InvalidOperation(
                "base backup manifest must contain at least one chunk".into(),
            ));
        }
        let mut expected_offset = 0u64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.ordinal as usize != index {
                return Err(RdbFileError::InvalidOperation(format!(
                    "base backup chunk ordinal {} does not match position {index}",
                    chunk.ordinal
                )));
            }
            if chunk.snapshot_offset != expected_offset {
                return Err(RdbFileError::InvalidOperation(format!(
                    "base backup chunk {} starts at {}, expected {expected_offset}",
                    chunk.ordinal, chunk.snapshot_offset
                )));
            }
            validate_relative_path(&chunk.relative_path, "base backup chunk")?;
            expected_offset = expected_offset.saturating_add(chunk.bytes);
        }
        if expected_offset != self.snapshot_bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup chunks cover {expected_offset} bytes, expected {}",
                self.snapshot_bytes
            )));
        }
        Ok(())
    }

    pub fn verify_chunk_bytes(
        &self,
        chunk: &BaseBackupChunkRef,
        bytes: &[u8],
    ) -> RdbFileResult<()> {
        if bytes.len() as u64 != chunk.bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup chunk {} has {} bytes, expected {}",
                chunk.ordinal,
                bytes.len(),
                chunk.bytes
            )));
        }
        let computed = crc32(bytes);
        if computed != chunk.checksum {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup chunk {} checksum mismatch: stored {:#010x}, computed {computed:#010x}",
                chunk.ordinal, chunk.checksum
            )));
        }
        Ok(())
    }

    pub fn verify_snapshot_parts(&self, root: impl AsRef<Path>) -> RdbFileResult<()> {
        self.read_snapshot_parts(root).map(|_| ())
    }

    pub fn stage_chunk_part(
        &self,
        root: impl AsRef<Path>,
        ordinal: u32,
        bytes: &[u8],
    ) -> RdbFileResult<()> {
        let chunk = self
            .chunks
            .iter()
            .find(|chunk| chunk.ordinal == ordinal)
            .ok_or_else(|| {
                RdbFileError::InvalidOperation(format!(
                    "base backup chunk ordinal {ordinal} not found in manifest"
                ))
            })?;
        self.verify_chunk_bytes(chunk, bytes)?;
        write_bytes_atomically(&root.as_ref().join(&chunk.relative_path), bytes)
    }

    pub fn recover_staged_chunk_parts(
        &self,
        root: impl AsRef<Path>,
    ) -> RdbFileResult<std::collections::BTreeSet<u32>> {
        self.validate()?;
        let root = root.as_ref();
        let mut recovered = std::collections::BTreeSet::new();
        for chunk in &self.chunks {
            let path = root.join(&chunk.relative_path);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            if self.verify_chunk_bytes(chunk, &bytes).is_ok() {
                recovered.insert(chunk.ordinal);
            } else {
                let _ = fs::remove_file(path);
            }
        }
        Ok(recovered)
    }

    pub fn read_snapshot_parts(&self, root: impl AsRef<Path>) -> RdbFileResult<Vec<u8>> {
        self.validate()?;
        let root = root.as_ref();
        let capacity = usize::try_from(self.snapshot_bytes).map_err(|_| {
            RdbFileError::InvalidOperation("base backup snapshot too large for memory".into())
        })?;
        let mut snapshot = Vec::with_capacity(capacity);
        for chunk in &self.chunks {
            let bytes = fs::read(root.join(&chunk.relative_path))?;
            self.verify_chunk_bytes(chunk, &bytes)?;
            snapshot.extend_from_slice(&bytes);
        }
        if snapshot.len() as u64 != self.snapshot_bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup snapshot has {} bytes, expected {}",
                snapshot.len(),
                self.snapshot_bytes
            )));
        }
        let computed = crc32(&snapshot);
        if computed != self.snapshot_checksum {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup snapshot checksum mismatch: stored {:#010x}, computed {computed:#010x}",
                self.snapshot_checksum
            )));
        }
        Ok(snapshot)
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(PRIMARY_BASEBACKUP_MAGIC);
        put_u16(&mut out, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u64(&mut out, self.timeline.0);
        put_u64(&mut out, self.start_lsn);
        put_u64(&mut out, self.checkpoint_lsn);
        put_string(&mut out, &self.snapshot_relative_path.to_string_lossy());
        put_u64(&mut out, self.snapshot_bytes);
        put_u32(&mut out, self.snapshot_checksum);
        put_u32(&mut out, self.chunks.len() as u32);
        for chunk in &self.chunks {
            put_u32(&mut out, chunk.ordinal);
            put_u64(&mut out, chunk.snapshot_offset);
            put_u64(&mut out, chunk.bytes);
            put_u32(&mut out, chunk.checksum);
            put_string(&mut out, &chunk.relative_path.to_string_lossy());
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "primary-replica base backup manifest")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            PRIMARY_BASEBACKUP_MAGIC,
            "base backup manifest",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported primary-replica base backup version {version}"
            )));
        }
        let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
        let start_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let checkpoint_lsn = take_u64(bytes, &mut cursor, payload_end)?;
        let snapshot_relative_path = PathBuf::from(take_string(bytes, &mut cursor, payload_end)?);
        let snapshot_bytes = take_u64(bytes, &mut cursor, payload_end)?;
        let snapshot_checksum = take_u32(bytes, &mut cursor, payload_end)?;
        if cursor == payload_end {
            return Self::new(
                BaseBackupPlan::new(timeline, start_lsn, checkpoint_lsn),
                snapshot_relative_path,
                snapshot_bytes,
                snapshot_checksum,
            );
        }
        let chunk_count = take_u32(bytes, &mut cursor, payload_end)?;
        if chunk_count > MAX_BASEBACKUP_CHUNKS {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup manifest has too many chunks: {chunk_count}"
            )));
        }
        let mut chunks = Vec::new();
        for _ in 0..chunk_count {
            let ordinal = take_u32(bytes, &mut cursor, payload_end)?;
            let snapshot_offset = take_u64(bytes, &mut cursor, payload_end)?;
            let chunk_bytes = take_u64(bytes, &mut cursor, payload_end)?;
            let checksum = take_u32(bytes, &mut cursor, payload_end)?;
            let relative_path = PathBuf::from(take_string(bytes, &mut cursor, payload_end)?);
            chunks.push(BaseBackupChunkRef::new(
                ordinal,
                snapshot_offset,
                chunk_bytes,
                checksum,
                relative_path,
            ));
        }
        if cursor != payload_end {
            return Err(RdbFileError::InvalidOperation(
                "primary-replica base backup manifest has trailing bytes".into(),
            ));
        }
        Self::incremental(
            BaseBackupPlan::new(timeline, start_lsn, checkpoint_lsn),
            snapshot_relative_path,
            snapshot_bytes,
            snapshot_checksum,
            chunks,
        )
    }
}

impl PrimaryReplicaFilePlan {
    pub fn list_basebackups(&self) -> RdbFileResult<Vec<PrimaryReplicaBaseBackupManifest>> {
        let entries = match fs::read_dir(self.basebackup_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut manifests = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("redbase") {
                continue;
            }
            let manifest = PrimaryReplicaBaseBackupManifest::read_from_path(&path)?;
            manifests.push(manifest);
        }
        manifests.sort_by_key(|manifest| (manifest.timeline.0, manifest.checkpoint_lsn));
        Ok(manifests)
    }

    pub fn basebackup_parts_dir(&self, backup: &BaseBackupPlan) -> PathBuf {
        self.basebackup_dir()
            .join(basebackup_artifact_name(backup))
            .with_extension("redbase.parts")
    }

    pub fn basebackup_chunk_relative_path(&self, backup: &BaseBackupPlan, ordinal: u32) -> PathBuf {
        PathBuf::from(basebackup_artifact_name(backup))
            .with_extension("redbase.parts")
            .join(basebackup_chunk_file_name(ordinal))
    }

    pub fn write_basebackup_snapshot_parts(
        &self,
        backup: BaseBackupPlan,
        snapshot: &[u8],
        chunk_bytes: usize,
    ) -> RdbFileResult<PrimaryReplicaBaseBackupManifest> {
        if backup.timeline != self.timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "base backup timeline {} does not match file plan timeline {}",
                backup.timeline.0, self.timeline.0
            )));
        }
        if !backup.is_valid() {
            return Err(RdbFileError::InvalidOperation(
                "base backup checkpoint lsn is before start lsn".into(),
            ));
        }
        let chunk_bytes = chunk_bytes.max(1);
        let parts_dir = self.basebackup_parts_dir(&backup);
        let manifest = self.basebackup_manifest_for_snapshot(backup, snapshot, chunk_bytes)?;
        if parts_dir.exists() {
            match manifest.verify_snapshot_parts(self.basebackup_dir()) {
                Ok(()) => return Ok(manifest),
                Err(err) if !self.basebackup_path(&backup).exists() => {
                    let _ = fs::remove_dir_all(&parts_dir);
                    let _ = err;
                }
                Err(err) => return Err(err),
            }
        }
        let staging_dir = self.basebackup_dir().join(format!(
            "{}.redbase.parts.tmp-{}-{}",
            basebackup_artifact_name(&backup),
            std::process::id(),
            now_unix_nanos()
        ));
        fs::create_dir_all(&staging_dir)?;

        let mut chunks = Vec::new();
        for (index, part) in snapshot.chunks(chunk_bytes).enumerate() {
            let ordinal = u32::try_from(index).map_err(|_| {
                RdbFileError::InvalidOperation("base backup has too many chunks".into())
            })?;
            let relative_path = self.basebackup_chunk_relative_path(&backup, ordinal);
            write_bytes_atomically(&staging_dir.join(basebackup_chunk_file_name(ordinal)), part)?;
            chunks.push(BaseBackupChunkRef::new(
                ordinal,
                (index * chunk_bytes) as u64,
                part.len() as u64,
                crc32(part),
                relative_path,
            ));
        }
        if chunks.is_empty() {
            let relative_path = self.basebackup_chunk_relative_path(&backup, 0);
            write_bytes_atomically(&staging_dir.join(basebackup_chunk_file_name(0)), &[])?;
            chunks.push(BaseBackupChunkRef::new(0, 0, 0, crc32(&[]), relative_path));
        }
        fs::rename(&staging_dir, &parts_dir)?;
        crash_inject("basebackup_after_parts_dir_rename");
        if let Ok(dir) = File::open(self.basebackup_dir()) {
            let _ = dir.sync_all();
        }

        Ok(manifest)
    }

    fn basebackup_manifest_for_snapshot(
        &self,
        backup: BaseBackupPlan,
        snapshot: &[u8],
        chunk_bytes: usize,
    ) -> RdbFileResult<PrimaryReplicaBaseBackupManifest> {
        let mut chunks = Vec::new();
        for (index, part) in snapshot.chunks(chunk_bytes).enumerate() {
            let ordinal = u32::try_from(index).map_err(|_| {
                RdbFileError::InvalidOperation("base backup has too many chunks".into())
            })?;
            chunks.push(BaseBackupChunkRef::new(
                ordinal,
                (index * chunk_bytes) as u64,
                part.len() as u64,
                crc32(part),
                self.basebackup_chunk_relative_path(&backup, ordinal),
            ));
        }
        if chunks.is_empty() {
            chunks.push(BaseBackupChunkRef::new(
                0,
                0,
                0,
                crc32(&[]),
                self.basebackup_chunk_relative_path(&backup, 0),
            ));
        }
        let snapshot_relative_path =
            PathBuf::from(basebackup_artifact_name(&backup)).with_extension("snapshot");
        PrimaryReplicaBaseBackupManifest::incremental(
            backup,
            snapshot_relative_path,
            snapshot.len() as u64,
            crc32(snapshot),
            chunks,
        )
    }
}

fn basebackup_artifact_name(backup: &BaseBackupPlan) -> String {
    format!(
        "base-{:020}-{:020}",
        backup.start_lsn, backup.checkpoint_lsn
    )
}

fn basebackup_chunk_file_name(ordinal: u32) -> String {
    format!("{ordinal:020}.redchunk")
}

fn validate_relative_path(path: &Path, artifact: &str) -> RdbFileResult<()> {
    if path.is_absolute() {
        return Err(RdbFileError::InvalidOperation(format!(
            "{artifact} path must be relative"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RdbFileError::InvalidOperation(format!(
            "{artifact} path must not contain parent components"
        )));
    }
    Ok(())
}
