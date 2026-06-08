use super::*;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessHydrationRequest {
    pub relative_path: PathBuf,
    pub offset: u64,
    pub bytes: u64,
    pub checksum: u32,
    pub content_hash: ServerlessContentHash,
}

impl ServerlessHydrationRequest {
    pub fn from_extent(extent: &ServerlessExtentRef) -> Self {
        Self {
            relative_path: extent.relative_path.clone(),
            offset: extent.offset,
            bytes: extent.bytes,
            checksum: extent.checksum,
            content_hash: extent.content_hash,
        }
    }

    pub fn validate_payload(&self, payload: &[u8]) -> RdbFileResult<()> {
        if payload.len() as u64 != self.bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "serverless hydration range {} has {} bytes, expected {}",
                self.relative_path.display(),
                payload.len(),
                self.bytes
            )));
        }
        let computed_crc = crc32(payload);
        if computed_crc != self.checksum {
            return Err(RdbFileError::InvalidOperation(format!(
                "serverless hydration range {} checksum mismatch: stored {:#010x}, computed {computed_crc:#010x}",
                self.relative_path.display(),
                self.checksum
            )));
        }
        let computed_hash = ServerlessContentHash::from_bytes(payload);
        if !self.content_hash.is_zero() && computed_hash != self.content_hash {
            return Err(RdbFileError::InvalidOperation(format!(
                "serverless hydration range {} content hash mismatch",
                self.relative_path.display()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessHydrationPlan {
    pub generation: u64,
    pub requests: Vec<ServerlessHydrationRequest>,
}

impl ServerlessHydrationPlan {
    pub fn total_bytes(&self) -> u64 {
        self.requests.iter().map(|request| request.bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessHydratedRange {
    pub request: ServerlessHydrationRequest,
    pub payload: Vec<u8>,
}

impl ServerlessFilePlan {
    pub fn hydrate_local_plan(
        &self,
        plan: &ServerlessHydrationPlan,
    ) -> RdbFileResult<Vec<ServerlessHydratedRange>> {
        if plan.generation != self.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "hydration plan generation {} does not match file plan generation {}",
                plan.generation, self.generation
            )));
        }
        let mut hydrated = Vec::with_capacity(plan.requests.len());
        for request in &plan.requests {
            hydrated.push(self.hydrate_local_request(request)?);
        }
        Ok(hydrated)
    }

    pub fn hydrate_local_plan_cached(
        &self,
        plan: &ServerlessHydrationPlan,
        cache: &ServerlessLocalCache,
    ) -> RdbFileResult<Vec<ServerlessHydratedRange>> {
        if plan.generation != self.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "hydration plan generation {} does not match file plan generation {}",
                plan.generation, self.generation
            )));
        }
        let mut hydrated = Vec::with_capacity(plan.requests.len());
        for request in &plan.requests {
            hydrated.push(self.hydrate_local_request_cached(request, cache)?);
        }
        Ok(hydrated)
    }

    pub fn hydrate_local_request(
        &self,
        request: &ServerlessHydrationRequest,
    ) -> RdbFileResult<ServerlessHydratedRange> {
        validate_hydration_relative_path(&request.relative_path)?;
        let end = request
            .offset
            .checked_add(request.bytes)
            .ok_or_else(|| RdbFileError::InvalidOperation("hydration range overflow".into()))?;
        let len = usize::try_from(request.bytes).map_err(|_| {
            RdbFileError::InvalidOperation("hydration range too large for local memory".into())
        })?;
        let path = self.generation_dir().join(&request.relative_path);
        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len();
        if end > file_len {
            return Err(RdbFileError::InvalidOperation(format!(
                "hydration range {}..{} exceeds pack {} length {}",
                request.offset,
                end,
                request.relative_path.display(),
                file_len
            )));
        }
        file.seek(SeekFrom::Start(request.offset))?;
        let mut payload = vec![0u8; len];
        file.read_exact(&mut payload)?;
        request.validate_payload(&payload)?;
        Ok(ServerlessHydratedRange {
            request: request.clone(),
            payload,
        })
    }

    pub fn hydrate_local_request_cached(
        &self,
        request: &ServerlessHydrationRequest,
        cache: &ServerlessLocalCache,
    ) -> RdbFileResult<ServerlessHydratedRange> {
        if cache.generation != self.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "serverless cache generation {} does not match file plan generation {}",
                cache.generation, self.generation
            )));
        }
        if let Ok(range) = cache.read_hydrated_range(request) {
            return Ok(range);
        }
        let range = self.hydrate_local_request(request)?;
        cache.write_hydrated_range(&range)?;
        cache.enforce_max_bytes(self.cache_policy.max_hot_bytes)?;
        Ok(range)
    }

    pub fn prefetch_hot_extents(
        &self,
        index: &ServerlessExtentIndex,
    ) -> RdbFileResult<Vec<ServerlessHydratedRange>> {
        self.hydrate_local_plan(&index.hot_hydration_plan())
    }

    pub fn prefetch_hot_extents_cached(
        &self,
        index: &ServerlessExtentIndex,
        cache: &ServerlessLocalCache,
    ) -> RdbFileResult<Vec<ServerlessHydratedRange>> {
        self.hydrate_local_plan_cached(&index.hot_hydration_plan(), cache)
    }
}

fn validate_hydration_relative_path(path: &Path) -> RdbFileResult<()> {
    if path.is_absolute() {
        return Err(RdbFileError::InvalidOperation(
            "serverless hydration path must be relative".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RdbFileError::InvalidOperation(
            "serverless hydration path must not contain parent components".into(),
        ));
    }
    Ok(())
}
