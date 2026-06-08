use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessBootIndexEntry {
    pub kind: ServerlessPackKind,
    pub relative_path: PathBuf,
    pub required_first: bool,
}

impl ServerlessBootIndexEntry {
    pub fn new(
        kind: ServerlessPackKind,
        relative_path: impl Into<PathBuf>,
        required_first: bool,
    ) -> Self {
        Self {
            kind,
            relative_path: relative_path.into(),
            required_first,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessBootIndex {
    pub generation: u64,
    pub entries: Vec<ServerlessBootIndexEntry>,
}

impl ServerlessBootIndex {
    pub fn from_plan(plan: &ServerlessFilePlan) -> Self {
        let mut entries = Vec::new();
        for path in plan.cold_start_order() {
            entries.push(ServerlessBootIndexEntry::new(
                kind_for_artifact_path(&path),
                relative_to_generation_dir(&path),
                true,
            ));
        }
        for path in ServerlessBootPlan::cold(plan).lazy_after_open {
            entries.push(ServerlessBootIndexEntry::new(
                kind_for_artifact_path(&path),
                relative_to_generation_dir(&path),
                false,
            ));
        }
        Self {
            generation: plan.generation,
            entries,
        }
    }

    pub fn required_first(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|entry| entry.required_first)
            .map(|entry| entry.relative_path.clone())
            .collect()
    }

    pub fn lazy_after_open(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|entry| !entry.required_first)
            .map(|entry| entry.relative_path.clone())
            .collect()
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes(path, &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SERVERLESS_BOOT_INDEX_MAGIC);
        put_u16(&mut out, SERVERLESS_ARTIFACT_VERSION);
        put_u64(&mut out, self.generation);
        put_u32(&mut out, self.entries.len() as u32);
        for entry in &self.entries {
            out.push(u8::from(entry.kind));
            out.push(u8::from(entry.required_first));
            put_string(&mut out, &entry.relative_path.to_string_lossy());
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes)?;
        let mut cursor = 0usize;
        expect_magic(bytes, &mut cursor, SERVERLESS_BOOT_INDEX_MAGIC)?;
        let version = take_u16(bytes, &mut cursor)?;
        if version != SERVERLESS_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported serverless boot-index version {version}"
            )));
        }
        let generation = take_u64(bytes, &mut cursor)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = ServerlessPackKind::try_from(take_u8(bytes, &mut cursor)?)?;
            let required_first = take_u8(bytes, &mut cursor)? != 0;
            let relative_path = PathBuf::from(take_string(bytes, &mut cursor)?);
            entries.push(ServerlessBootIndexEntry {
                kind,
                relative_path,
                required_first,
            });
        }
        reject_trailing_bytes(bytes, cursor)?;
        Ok(Self {
            generation,
            entries,
        })
    }
}
