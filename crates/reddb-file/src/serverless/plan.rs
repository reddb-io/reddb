use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessFilePlan {
    pub root: PathBuf,
    pub namespace: String,
    pub generation: u64,
    pub cache_policy: ServerlessCachePolicy,
}

impl ServerlessFilePlan {
    pub fn new(root: impl Into<PathBuf>, namespace: impl Into<String>, generation: u64) -> Self {
        Self {
            root: root.into(),
            namespace: namespace.into(),
            generation,
            cache_policy: ServerlessCachePolicy::default(),
        }
    }

    pub fn for_data_path(data_path: impl AsRef<Path>, generation: u64) -> Self {
        let data_path = data_path.as_ref();
        Self::new(
            crate::layout::serverless_root(data_path),
            crate::layout::serverless_namespace(data_path),
            generation,
        )
    }

    pub fn with_cache_policy(mut self, policy: ServerlessCachePolicy) -> Self {
        self.cache_policy = policy;
        self
    }

    pub fn for_generation(&self, generation: u64) -> Self {
        Self::new(self.root.clone(), self.namespace.clone(), generation)
            .with_cache_policy(self.cache_policy.clone())
    }

    pub fn local_cache(&self) -> ServerlessLocalCache {
        ServerlessLocalCache::new(
            crate::layout::serverless_cache_root(&self.root, &self.namespace),
            self.generation,
        )
    }

    pub fn artifact_path(&self, kind: ServerlessPackKind) -> PathBuf {
        self.root
            .join(&self.namespace)
            .join(format!("g{:020}", self.generation))
            .join(format!("{}.redpack", kind.as_str()))
    }

    pub fn generation_dir(&self) -> PathBuf {
        self.root
            .join(&self.namespace)
            .join(format!("g{:020}", self.generation))
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::Manifest)
    }

    pub fn boot_index_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::BootIndex)
    }

    pub fn extent_index_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::ExtentIndex)
    }

    pub fn collection_data_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::CollectionData)
    }

    pub fn collection_data_extent_ref(
        &self,
        collection: impl Into<String>,
        offset: u64,
        payload: &[u8],
        hot: bool,
    ) -> RdbFileResult<ServerlessExtentRef> {
        ServerlessExtentRef::new(
            collection,
            Vec::<u8>::new(),
            Vec::<u8>::new(),
            relative_to_generation_dir(&self.collection_data_path()),
            offset,
            payload,
            hot,
        )
    }

    pub fn secondary_index_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::SecondaryIndex)
    }

    pub fn current_pointer_path(&self) -> PathBuf {
        self.root.join(&self.namespace).join("CURRENT.redptr")
    }

    pub fn publish_generation_pointer(
        &self,
        manifest: &ServerlessManifest,
    ) -> RdbFileResult<ServerlessGenerationPointer> {
        if manifest.namespace != self.namespace {
            return Err(RdbFileError::InvalidOperation(format!(
                "manifest namespace {} does not match plan namespace {}",
                manifest.namespace, self.namespace
            )));
        }
        if manifest.generation != self.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "manifest generation {} does not match plan generation {}",
                manifest.generation, self.generation
            )));
        }
        self.validate_complete_generation(manifest)?;
        let pointer = ServerlessGenerationPointer::from_manifest(self, manifest);
        pointer.write_to_path(self.current_pointer_path())?;
        Ok(pointer)
    }

    pub fn read_current_pointer(&self) -> RdbFileResult<ServerlessGenerationPointer> {
        ServerlessGenerationPointer::read_from_path(self.current_pointer_path())
    }

    pub fn read_current_pointer_verified(&self) -> RdbFileResult<ServerlessGenerationPointer> {
        let pointer = self.read_current_pointer()?;
        if pointer.namespace != self.namespace {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer namespace {} does not match plan namespace {}",
                pointer.namespace, self.namespace
            )));
        }

        let expected_manifest_relative_path =
            PathBuf::from(format!("g{:020}/manifest.redpack", pointer.generation));
        validate_generation_relative_path(&pointer.manifest_relative_path)?;
        if pointer.manifest_relative_path != expected_manifest_relative_path {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer manifest path {} does not match expected {}",
                pointer.manifest_relative_path.display(),
                expected_manifest_relative_path.display()
            )));
        }

        let generation_plan = ServerlessFilePlan::new(
            self.root.clone(),
            self.namespace.clone(),
            pointer.generation,
        )
        .with_cache_policy(self.cache_policy.clone());
        let manifest_path = self
            .root
            .join(&self.namespace)
            .join(&pointer.manifest_relative_path);
        let manifest_bytes = fs::read(&manifest_path)?;
        if manifest_bytes.len() as u64 != pointer.manifest_bytes {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer manifest has {} bytes, expected {}",
                manifest_bytes.len(),
                pointer.manifest_bytes
            )));
        }
        let computed_crc = crc32(&manifest_bytes);
        if computed_crc != pointer.manifest_checksum {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer manifest checksum mismatch: stored {:#010x}, computed {computed_crc:#010x}",
                pointer.manifest_checksum
            )));
        }
        let computed_hash = ServerlessContentHash::from_bytes(&manifest_bytes);
        if computed_hash != pointer.manifest_content_hash {
            return Err(RdbFileError::InvalidOperation(
                "current pointer manifest content hash mismatch".into(),
            ));
        }

        let manifest = ServerlessManifest::decode(&manifest_bytes)?;
        if manifest.namespace != pointer.namespace {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer namespace {} does not match manifest namespace {}",
                pointer.namespace, manifest.namespace
            )));
        }
        if manifest.generation != pointer.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "current pointer generation {} does not match manifest generation {}",
                pointer.generation, manifest.generation
            )));
        }
        generation_plan.validate_complete_generation(&manifest)?;
        Ok(pointer)
    }

    pub fn wal_tail_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::WalTail)
    }

    pub fn hot_snapshot_path(&self) -> PathBuf {
        self.artifact_path(ServerlessPackKind::HotSnapshot)
    }

    pub fn publish_core_generation(
        &self,
        extent_index: &ServerlessExtentIndex,
        collection_data: &[u8],
        secondary_index: &[u8],
    ) -> RdbFileResult<ServerlessGenerationPointer> {
        if extent_index.generation != self.generation {
            return Err(RdbFileError::InvalidOperation(format!(
                "extent index generation {} does not match plan generation {}",
                extent_index.generation, self.generation
            )));
        }

        let mut manifest = ServerlessManifest::new(&self.namespace, self.generation);
        let boot_index = ServerlessBootIndex::from_plan(self);
        let boot_index_bytes = boot_index.encode();
        let extent_index_bytes = extent_index.encode();
        let empty_pack: &[u8] = &[];
        let packs = [
            (
                ServerlessPackKind::BootIndex,
                relative_to_generation_dir(&self.boot_index_path()),
                boot_index_bytes.as_slice(),
            ),
            (
                ServerlessPackKind::ExtentIndex,
                relative_to_generation_dir(&self.extent_index_path()),
                extent_index_bytes.as_slice(),
            ),
            (
                ServerlessPackKind::HotSnapshot,
                relative_to_generation_dir(&self.hot_snapshot_path()),
                empty_pack,
            ),
            (
                ServerlessPackKind::WalTail,
                relative_to_generation_dir(&self.wal_tail_path()),
                empty_pack,
            ),
            (
                ServerlessPackKind::CollectionData,
                relative_to_generation_dir(&self.collection_data_path()),
                collection_data,
            ),
            (
                ServerlessPackKind::SecondaryIndex,
                relative_to_generation_dir(&self.secondary_index_path()),
                secondary_index,
            ),
            (
                ServerlessPackKind::ColdArchive,
                relative_to_generation_dir(&self.artifact_path(ServerlessPackKind::ColdArchive)),
                empty_pack,
            ),
        ];

        for (kind, relative_path, payload) in packs {
            write_bytes(self.generation_dir().join(&relative_path), payload)?;
            manifest.push(ServerlessManifestEntry::from_bytes(
                kind,
                relative_path,
                payload,
            ));
        }
        manifest.write_to_path(self.manifest_path())?;
        self.publish_generation_pointer(&manifest)
    }

    pub fn validate_complete_generation(&self, manifest: &ServerlessManifest) -> RdbFileResult<()> {
        let required = [
            ServerlessPackKind::BootIndex,
            ServerlessPackKind::ExtentIndex,
            ServerlessPackKind::HotSnapshot,
            ServerlessPackKind::WalTail,
            ServerlessPackKind::CollectionData,
            ServerlessPackKind::SecondaryIndex,
        ];
        for required_kind in required {
            if !manifest
                .entries
                .iter()
                .any(|entry| entry.kind == required_kind)
            {
                return Err(RdbFileError::InvalidOperation(format!(
                    "serverless generation {} is missing required {} pack",
                    self.generation,
                    required_kind.as_str()
                )));
            }
        }

        for entry in &manifest.entries {
            validate_generation_relative_path(&entry.relative_path)?;
            let payload = fs::read(self.generation_dir().join(&entry.relative_path))?;
            if payload.len() as u64 != entry.bytes {
                return Err(RdbFileError::InvalidOperation(format!(
                    "serverless pack {} has {} bytes, expected {}",
                    entry.relative_path.display(),
                    payload.len(),
                    entry.bytes
                )));
            }
            let computed_crc = crc32(&payload);
            if computed_crc != entry.checksum {
                return Err(RdbFileError::InvalidOperation(format!(
                    "serverless pack {} checksum mismatch: stored {:#010x}, computed {computed_crc:#010x}",
                    entry.relative_path.display(),
                    entry.checksum
                )));
            }
            let computed_hash = ServerlessContentHash::from_bytes(&payload);
            if !entry.content_hash.is_zero() && computed_hash != entry.content_hash {
                return Err(RdbFileError::InvalidOperation(format!(
                    "serverless pack {} content hash mismatch",
                    entry.relative_path.display()
                )));
            }
        }

        let manifest_bytes = fs::read(self.manifest_path())?;
        let encoded = manifest.encode();
        if manifest_bytes != encoded {
            return Err(RdbFileError::InvalidOperation(
                "serverless manifest on disk does not match publish manifest".into(),
            ));
        }
        Ok(())
    }

    pub fn cold_start_order(&self) -> Vec<PathBuf> {
        let mut order = vec![self.manifest_path(), self.boot_index_path()];
        order.push(self.extent_index_path());
        if self.cache_policy.keep_hot_snapshot_local {
            order.push(self.hot_snapshot_path());
        }
        order.push(self.wal_tail_path());
        order
    }

    pub fn hot_start_order(&self) -> Vec<PathBuf> {
        let mut order = Vec::new();
        if self.cache_policy.keep_boot_index_local {
            order.push(self.boot_index_path());
        }
        if self.cache_policy.keep_hot_snapshot_local {
            order.push(self.hot_snapshot_path());
        }
        order.push(self.wal_tail_path());
        order
    }

    pub fn is_generation_dir(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                name.len() == 21
                    && name.starts_with('g')
                    && name[1..].chars().all(|c| c.is_ascii_digit())
            })
            .unwrap_or(false)
    }
}

fn validate_generation_relative_path(path: &Path) -> RdbFileResult<()> {
    if path.is_absolute() {
        return Err(RdbFileError::InvalidOperation(
            "serverless pack path must be relative".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RdbFileError::InvalidOperation(
            "serverless pack path must not contain parent components".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessBootPlan {
    pub required_first: Vec<PathBuf>,
    pub lazy_after_open: Vec<PathBuf>,
}

impl ServerlessBootPlan {
    pub fn cold(plan: &ServerlessFilePlan) -> Self {
        Self {
            required_first: plan.cold_start_order(),
            lazy_after_open: vec![
                plan.artifact_path(ServerlessPackKind::CollectionData),
                plan.artifact_path(ServerlessPackKind::SecondaryIndex),
                plan.artifact_path(ServerlessPackKind::ColdArchive),
            ],
        }
    }

    pub fn hot(plan: &ServerlessFilePlan) -> Self {
        Self {
            required_first: plan.hot_start_order(),
            lazy_after_open: vec![
                plan.artifact_path(ServerlessPackKind::Manifest),
                plan.artifact_path(ServerlessPackKind::CollectionData),
                plan.artifact_path(ServerlessPackKind::SecondaryIndex),
            ],
        }
    }
}
