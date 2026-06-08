use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessCachePolicy {
    pub keep_boot_index_local: bool,
    pub keep_hot_snapshot_local: bool,
    pub max_hot_bytes: u64,
}

impl Default for ServerlessCachePolicy {
    fn default() -> Self {
        Self {
            keep_boot_index_local: true,
            keep_hot_snapshot_local: true,
            max_hot_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessCacheEntry {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub hot: bool,
    pub last_access_unix_ms: u64,
}

impl ServerlessCacheEntry {
    pub fn new(
        relative_path: impl Into<PathBuf>,
        bytes: u64,
        hot: bool,
        last_access_unix_ms: u64,
    ) -> Self {
        Self {
            relative_path: relative_path.into(),
            bytes,
            hot,
            last_access_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessCacheEvictionPlan {
    pub evict: Vec<PathBuf>,
    pub bytes_after_eviction: u64,
}

impl ServerlessCacheEvictionPlan {
    pub fn plan(entries: &[ServerlessCacheEntry], max_bytes: u64) -> Self {
        let mut total: u64 = entries.iter().map(|entry| entry.bytes).sum();
        let mut candidates: Vec<&ServerlessCacheEntry> = entries.iter().collect();
        candidates.sort_by_key(|entry| (entry.hot, entry.last_access_unix_ms));
        let mut evict = Vec::new();
        for entry in candidates {
            if total <= max_bytes {
                break;
            }
            evict.push(entry.relative_path.clone());
            total = total.saturating_sub(entry.bytes);
        }
        Self {
            evict,
            bytes_after_eviction: total,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessLocalCache {
    pub root: PathBuf,
    pub generation: u64,
}

impl ServerlessLocalCache {
    pub fn new(root: impl Into<PathBuf>, generation: u64) -> Self {
        Self {
            root: root.into(),
            generation,
        }
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(format!("g{:020}", self.generation))
    }

    pub fn path_for_request(&self, request: &ServerlessHydrationRequest) -> PathBuf {
        self.cache_dir()
            .join(format!("{}.redcache", hydration_cache_key(request)))
    }

    pub fn write_hydrated_range(&self, range: &ServerlessHydratedRange) -> RdbFileResult<PathBuf> {
        range.request.validate_payload(&range.payload)?;
        let path = self.path_for_request(&range.request);
        write_bytes(&path, &range.payload)?;
        Ok(path)
    }

    pub fn read_hydrated_range(
        &self,
        request: &ServerlessHydrationRequest,
    ) -> RdbFileResult<ServerlessHydratedRange> {
        let path = self.path_for_request(request);
        let payload = fs::read(&path)?;
        request.validate_payload(&payload)?;
        write_bytes(&path, &payload)?;
        Ok(ServerlessHydratedRange {
            request: request.clone(),
            payload,
        })
    }

    pub fn remove_hydrated_range(&self, request: &ServerlessHydrationRequest) -> RdbFileResult<()> {
        let path = self.path_for_request(request);
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn cached_entries(&self) -> RdbFileResult<Vec<ServerlessCacheEntry>> {
        let cache_dir = self.cache_dir();
        let entries = match fs::read_dir(&cache_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut cached = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("redcache")
            {
                continue;
            }
            let Some(file_name) = path.file_name() else {
                continue;
            };
            let metadata = entry.metadata()?;
            cached.push(ServerlessCacheEntry::new(
                PathBuf::from(file_name),
                metadata.len(),
                true,
                metadata
                    .modified()
                    .ok()
                    .and_then(system_time_to_unix_ms)
                    .unwrap_or(0),
            ));
        }
        Ok(cached)
    }

    pub fn enforce_max_bytes(&self, max_bytes: u64) -> RdbFileResult<ServerlessCacheEvictionPlan> {
        let entries = self.cached_entries()?;
        let plan = ServerlessCacheEvictionPlan::plan(&entries, max_bytes);
        for relative_path in &plan.evict {
            validate_cache_relative_path(relative_path)?;
            let path = self.cache_dir().join(relative_path);
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(plan)
    }
}

fn system_time_to_unix_ms(time: SystemTime) -> Option<u64> {
    let millis = time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(millis).ok()
}

fn validate_cache_relative_path(path: &Path) -> RdbFileResult<()> {
    if path.is_absolute() {
        return Err(RdbFileError::InvalidOperation(
            "serverless cache path must be relative".into(),
        ));
    }
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(RdbFileError::InvalidOperation(
            "serverless cache path must be a file name".into(),
        ));
    }
    Ok(())
}

fn hydration_cache_key(request: &ServerlessHydrationRequest) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(request.relative_path.to_string_lossy().as_bytes());
    hasher.update(&[0]);
    hasher.update(&request.offset.to_le_bytes());
    hasher.update(&request.bytes.to_le_bytes());
    hasher.update(&request.checksum.to_le_bytes());
    hasher.update(&request.content_hash.0);
    hex_bytes(hasher.finalize().as_bytes())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
