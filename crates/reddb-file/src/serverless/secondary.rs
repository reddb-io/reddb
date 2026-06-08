use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessSecondaryIndexEntry {
    pub collection: String,
    pub range_start: Vec<u8>,
    pub range_end: Vec<u8>,
    pub relative_path: PathBuf,
    pub offset: u64,
    pub bytes: u64,
    pub checksum: u32,
    pub content_hash: ServerlessContentHash,
    pub hot: bool,
}

impl From<&ServerlessExtentRef> for ServerlessSecondaryIndexEntry {
    fn from(extent: &ServerlessExtentRef) -> Self {
        Self {
            collection: extent.collection.clone(),
            range_start: extent.range_start.clone(),
            range_end: extent.range_end.clone(),
            relative_path: extent.relative_path.clone(),
            offset: extent.offset,
            bytes: extent.bytes,
            checksum: extent.checksum,
            content_hash: extent.content_hash,
            hot: extent.hot,
        }
    }
}

impl ServerlessSecondaryIndexEntry {
    pub fn hydration_request(&self) -> ServerlessHydrationRequest {
        ServerlessHydrationRequest {
            relative_path: self.relative_path.clone(),
            offset: self.offset,
            bytes: self.bytes,
            checksum: self.checksum,
            content_hash: self.content_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessSecondaryIndex {
    pub generation: u64,
    pub entries: Vec<ServerlessSecondaryIndexEntry>,
}

impl ServerlessSecondaryIndex {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            entries: Vec::new(),
        }
    }

    pub fn from_extent_index(index: &ServerlessExtentIndex) -> Self {
        let mut secondary = Self::new(index.generation);
        for extent in &index.extents {
            secondary.push(ServerlessSecondaryIndexEntry::from(extent));
        }
        secondary
    }

    pub fn push(&mut self, entry: ServerlessSecondaryIndexEntry) {
        self.entries.push(entry);
        self.entries.sort_by(|left, right| {
            (
                left.collection.as_str(),
                left.range_start.as_slice(),
                left.range_end.as_slice(),
                left.relative_path.to_string_lossy(),
                left.offset,
            )
                .cmp(&(
                    right.collection.as_str(),
                    right.range_start.as_slice(),
                    right.range_end.as_slice(),
                    right.relative_path.to_string_lossy(),
                    right.offset,
                ))
        });
    }

    pub fn entries_for_collection(&self, collection: &str) -> Vec<&ServerlessSecondaryIndexEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.collection == collection)
            .collect()
    }

    pub fn hydration_plan_for_collection(&self, collection: &str) -> ServerlessHydrationPlan {
        ServerlessHydrationPlan {
            generation: self.generation,
            requests: self
                .entries_for_collection(collection)
                .into_iter()
                .map(ServerlessSecondaryIndexEntry::hydration_request)
                .collect(),
        }
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes(path, &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SERVERLESS_SECONDARY_INDEX_MAGIC);
        put_u16(&mut out, SERVERLESS_ARTIFACT_VERSION);
        put_u64(&mut out, self.generation);
        put_u32(&mut out, self.entries.len() as u32);
        for entry in &self.entries {
            put_string(&mut out, &entry.collection);
            put_bytes(&mut out, &entry.range_start);
            put_bytes(&mut out, &entry.range_end);
            put_string(&mut out, &entry.relative_path.to_string_lossy());
            put_u64(&mut out, entry.offset);
            put_u64(&mut out, entry.bytes);
            put_u32(&mut out, entry.checksum);
            put_content_hash(&mut out, entry.content_hash);
            out.push(u8::from(entry.hot));
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes)?;
        let mut cursor = 0usize;
        expect_magic(bytes, &mut cursor, SERVERLESS_SECONDARY_INDEX_MAGIC)?;
        let version = take_u16(bytes, &mut cursor)?;
        if version != SERVERLESS_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported serverless secondary index version {version}"
            )));
        }
        let generation = take_u64(bytes, &mut cursor)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        let mut index = Self::new(generation);
        for _ in 0..count {
            let collection = take_string(bytes, &mut cursor)?;
            let range_start = take_vec_bytes(bytes, &mut cursor)?;
            let range_end = take_vec_bytes(bytes, &mut cursor)?;
            if !range_end.is_empty() && range_start >= range_end {
                return Err(RdbFileError::InvalidOperation(
                    "serverless secondary index range_start must be before range_end".into(),
                ));
            }
            index.push(ServerlessSecondaryIndexEntry {
                collection,
                range_start,
                range_end,
                relative_path: PathBuf::from(take_string(bytes, &mut cursor)?),
                offset: take_u64(bytes, &mut cursor)?,
                bytes: take_u64(bytes, &mut cursor)?,
                checksum: take_u32(bytes, &mut cursor)?,
                content_hash: take_content_hash(bytes, &mut cursor)?,
                hot: take_u8(bytes, &mut cursor)? != 0,
            });
        }
        reject_trailing_bytes(bytes, cursor)?;
        Ok(index)
    }
}
