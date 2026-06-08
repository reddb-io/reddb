use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessGenerationPointer {
    pub namespace: String,
    pub generation: u64,
    pub manifest_relative_path: PathBuf,
    pub manifest_bytes: u64,
    pub manifest_checksum: u32,
    pub manifest_content_hash: ServerlessContentHash,
}

impl ServerlessGenerationPointer {
    pub fn from_manifest(plan: &ServerlessFilePlan, manifest: &ServerlessManifest) -> Self {
        let manifest_bytes = manifest.encode();
        Self {
            namespace: plan.namespace.clone(),
            generation: manifest.generation,
            manifest_relative_path: PathBuf::from(format!(
                "g{:020}/manifest.redpack",
                manifest.generation
            )),
            manifest_bytes: manifest_bytes.len() as u64,
            manifest_checksum: crc32(&manifest_bytes),
            manifest_content_hash: ServerlessContentHash::from_bytes(&manifest_bytes),
        }
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_current_pointer_bytes(path, &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SERVERLESS_GENERATION_POINTER_MAGIC);
        put_u16(&mut out, SERVERLESS_ARTIFACT_VERSION);
        put_string(&mut out, &self.namespace);
        put_u64(&mut out, self.generation);
        put_string(&mut out, &self.manifest_relative_path.to_string_lossy());
        put_u64(&mut out, self.manifest_bytes);
        put_u32(&mut out, self.manifest_checksum);
        put_content_hash(&mut out, self.manifest_content_hash);
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes)?;
        let mut cursor = 0usize;
        expect_magic(bytes, &mut cursor, SERVERLESS_GENERATION_POINTER_MAGIC)?;
        let version = take_u16(bytes, &mut cursor)?;
        if version != SERVERLESS_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported serverless generation pointer version {version}"
            )));
        }
        let namespace = take_string(bytes, &mut cursor)?;
        let generation = take_u64(bytes, &mut cursor)?;
        let manifest_relative_path = PathBuf::from(take_string(bytes, &mut cursor)?);
        let manifest_bytes = take_u64(bytes, &mut cursor)?;
        let manifest_checksum = take_u32(bytes, &mut cursor)?;
        let manifest_content_hash = take_content_hash(bytes, &mut cursor)?;
        reject_trailing_bytes(bytes, cursor)?;
        Ok(Self {
            namespace,
            generation,
            manifest_relative_path,
            manifest_bytes,
            manifest_checksum,
            manifest_content_hash,
        })
    }
}
