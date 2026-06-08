use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessExtentRef {
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

impl ServerlessExtentRef {
    pub fn new(
        collection: impl Into<String>,
        range_start: impl Into<Vec<u8>>,
        range_end: impl Into<Vec<u8>>,
        relative_path: impl Into<PathBuf>,
        offset: u64,
        payload: &[u8],
        hot: bool,
    ) -> RdbFileResult<Self> {
        let range_start = range_start.into();
        let range_end = range_end.into();
        if !range_end.is_empty() && range_start >= range_end {
            return Err(RdbFileError::InvalidOperation(
                "serverless extent range_start must be before range_end".into(),
            ));
        }
        Ok(Self {
            collection: collection.into(),
            range_start,
            range_end,
            relative_path: relative_path.into(),
            offset,
            bytes: payload.len() as u64,
            checksum: crc32(payload),
            content_hash: ServerlessContentHash::from_bytes(payload),
            hot,
        })
    }

    pub fn contains_key(&self, collection: &str, key: &[u8]) -> bool {
        self.collection == collection
            && key >= self.range_start.as_slice()
            && (self.range_end.is_empty() || key < self.range_end.as_slice())
    }

    pub fn overlaps_range(&self, collection: &str, range_start: &[u8], range_end: &[u8]) -> bool {
        if self.collection != collection {
            return false;
        }
        let extent_ends_after_start =
            self.range_end.is_empty() || self.range_end.as_slice() > range_start;
        let extent_starts_before_end =
            range_end.is_empty() || self.range_start.as_slice() < range_end;
        extent_ends_after_start && extent_starts_before_end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessExtentIndex {
    pub generation: u64,
    pub extents: Vec<ServerlessExtentRef>,
}

impl ServerlessExtentIndex {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            extents: Vec::new(),
        }
    }

    pub fn push(&mut self, extent: ServerlessExtentRef) {
        self.extents.push(extent);
        self.extents.sort_by(|left, right| {
            (
                left.collection.as_str(),
                left.range_start.as_slice(),
                left.relative_path.to_string_lossy(),
                left.offset,
            )
                .cmp(&(
                    right.collection.as_str(),
                    right.range_start.as_slice(),
                    right.relative_path.to_string_lossy(),
                    right.offset,
                ))
        });
    }

    pub fn extents_for_key(&self, collection: &str, key: &[u8]) -> Vec<&ServerlessExtentRef> {
        self.extents
            .iter()
            .filter(|extent| extent.contains_key(collection, key))
            .collect()
    }

    pub fn extents_for_range(
        &self,
        collection: &str,
        range_start: &[u8],
        range_end: &[u8],
    ) -> RdbFileResult<Vec<&ServerlessExtentRef>> {
        if !range_end.is_empty() && range_start >= range_end {
            return Err(RdbFileError::InvalidOperation(
                "serverless hydration range_start must be before range_end".into(),
            ));
        }
        Ok(self
            .extents
            .iter()
            .filter(|extent| extent.overlaps_range(collection, range_start, range_end))
            .collect())
    }

    pub fn hot_prefetch_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .extents
            .iter()
            .filter(|extent| extent.hot)
            .map(|extent| extent.relative_path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn hydration_plan_for_key(&self, collection: &str, key: &[u8]) -> ServerlessHydrationPlan {
        ServerlessHydrationPlan {
            generation: self.generation,
            requests: self
                .extents_for_key(collection, key)
                .into_iter()
                .map(ServerlessHydrationRequest::from_extent)
                .collect(),
        }
    }

    pub fn hydration_plan_for_range(
        &self,
        collection: &str,
        range_start: &[u8],
        range_end: &[u8],
    ) -> RdbFileResult<ServerlessHydrationPlan> {
        Ok(ServerlessHydrationPlan {
            generation: self.generation,
            requests: self
                .extents_for_range(collection, range_start, range_end)?
                .into_iter()
                .map(ServerlessHydrationRequest::from_extent)
                .collect(),
        })
    }

    pub fn hot_hydration_plan(&self) -> ServerlessHydrationPlan {
        ServerlessHydrationPlan {
            generation: self.generation,
            requests: self
                .extents
                .iter()
                .filter(|extent| extent.hot)
                .map(ServerlessHydrationRequest::from_extent)
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
        out.extend_from_slice(SERVERLESS_EXTENT_INDEX_MAGIC);
        put_u16(&mut out, SERVERLESS_ARTIFACT_VERSION);
        put_u64(&mut out, self.generation);
        put_u32(&mut out, self.extents.len() as u32);
        for extent in &self.extents {
            put_string(&mut out, &extent.collection);
            put_bytes(&mut out, &extent.range_start);
            put_bytes(&mut out, &extent.range_end);
            put_string(&mut out, &extent.relative_path.to_string_lossy());
            put_u64(&mut out, extent.offset);
            put_u64(&mut out, extent.bytes);
            put_u32(&mut out, extent.checksum);
            put_content_hash(&mut out, extent.content_hash);
            out.push(u8::from(extent.hot));
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes)?;
        let mut cursor = 0usize;
        expect_magic(bytes, &mut cursor, SERVERLESS_EXTENT_INDEX_MAGIC)?;
        let version = take_u16(bytes, &mut cursor)?;
        if version != SERVERLESS_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported serverless extent index version {version}"
            )));
        }
        let generation = take_u64(bytes, &mut cursor)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        let mut index = Self::new(generation);
        for _ in 0..count {
            let collection = take_string(bytes, &mut cursor)?;
            let range_start = take_vec_bytes(bytes, &mut cursor)?;
            let range_end = take_vec_bytes(bytes, &mut cursor)?;
            let relative_path = PathBuf::from(take_string(bytes, &mut cursor)?);
            let offset = take_u64(bytes, &mut cursor)?;
            let bytes_len = take_u64(bytes, &mut cursor)?;
            let checksum = take_u32(bytes, &mut cursor)?;
            let content_hash = take_content_hash(bytes, &mut cursor)?;
            let hot = take_u8(bytes, &mut cursor)? != 0;
            if !range_end.is_empty() && range_start >= range_end {
                return Err(RdbFileError::InvalidOperation(
                    "serverless extent range_start must be before range_end".into(),
                ));
            }
            index.push(ServerlessExtentRef {
                collection,
                range_start,
                range_end,
                relative_path,
                offset,
                bytes: bytes_len,
                checksum,
                content_hash,
                hot,
            });
        }
        reject_trailing_bytes(bytes, cursor)?;
        Ok(index)
    }
}
