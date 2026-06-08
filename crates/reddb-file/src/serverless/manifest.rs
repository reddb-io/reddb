use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessManifestEntry {
    pub kind: ServerlessPackKind,
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub checksum: u32,
    pub content_hash: ServerlessContentHash,
}

impl ServerlessManifestEntry {
    pub fn new(
        kind: ServerlessPackKind,
        relative_path: impl Into<PathBuf>,
        bytes: u64,
        checksum: u32,
    ) -> Self {
        Self {
            kind,
            relative_path: relative_path.into(),
            bytes,
            checksum,
            content_hash: ServerlessContentHash::ZERO,
        }
    }

    pub fn from_bytes(
        kind: ServerlessPackKind,
        relative_path: impl Into<PathBuf>,
        payload: &[u8],
    ) -> Self {
        Self {
            kind,
            relative_path: relative_path.into(),
            bytes: payload.len() as u64,
            checksum: crc32(payload),
            content_hash: ServerlessContentHash::from_bytes(payload),
        }
    }

    pub fn with_content_hash(mut self, content_hash: ServerlessContentHash) -> Self {
        self.content_hash = content_hash;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessManifest {
    pub namespace: String,
    pub generation: u64,
    pub entries: Vec<ServerlessManifestEntry>,
}

impl ServerlessManifest {
    pub fn new(namespace: impl Into<String>, generation: u64) -> Self {
        Self {
            namespace: namespace.into(),
            generation,
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: ServerlessManifestEntry) {
        self.entries.push(entry);
        self.entries.sort_by_key(|entry| {
            (
                u8::from(entry.kind),
                entry.relative_path.to_string_lossy().to_string(),
            )
        });
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes(path, &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SERVERLESS_MANIFEST_MAGIC);
        put_u16(&mut out, SERVERLESS_ARTIFACT_VERSION);
        put_u64(&mut out, self.generation);
        put_string(&mut out, &self.namespace);
        put_u32(&mut out, self.entries.len() as u32);
        for entry in &self.entries {
            out.push(u8::from(entry.kind));
            put_string(&mut out, &entry.relative_path.to_string_lossy());
            put_u64(&mut out, entry.bytes);
            put_u32(&mut out, entry.checksum);
            put_content_hash(&mut out, entry.content_hash);
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes)?;
        let mut cursor = 0usize;
        expect_magic(bytes, &mut cursor, SERVERLESS_MANIFEST_MAGIC)?;
        let version = take_u16(bytes, &mut cursor)?;
        if version != SERVERLESS_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported serverless manifest version {version}"
            )));
        }
        let generation = take_u64(bytes, &mut cursor)?;
        let namespace = take_string(bytes, &mut cursor)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = ServerlessPackKind::try_from(take_u8(bytes, &mut cursor)?)?;
            let relative_path = PathBuf::from(take_string(bytes, &mut cursor)?);
            let bytes_len = take_u64(bytes, &mut cursor)?;
            let checksum = take_u32(bytes, &mut cursor)?;
            let content_hash = take_content_hash(bytes, &mut cursor)?;
            entries.push(ServerlessManifestEntry {
                kind,
                relative_path,
                bytes: bytes_len,
                checksum,
                content_hash,
            });
        }
        reject_trailing_bytes(bytes, cursor)?;
        Ok(Self {
            namespace,
            generation,
            entries,
        })
    }
}
