use super::*;

impl UnifiedStore {
    pub fn read_native_physical_state(&self) -> Result<NativePhysicalState, StoreError> {
        let header = self.physical_file_header().ok_or_else(|| {
            StoreError::Serialization("native physical header is not available".to_string())
        })?;

        let collection_roots = self.read_native_collection_roots(header.collection_roots_page)?;
        let manifest = if header.manifest_page != 0 {
            self.read_native_manifest_summary(header.manifest_page).ok()
        } else {
            None
        };
        let registry = if header.registry_page != 0 {
            self.read_native_registry_summary(header.registry_page).ok()
        } else {
            None
        };
        let recovery = if header.recovery_page != 0 {
            self.read_native_recovery_summary(header.recovery_page).ok()
        } else {
            None
        };
        let catalog = if header.catalog_page != 0 {
            self.read_native_catalog_summary(header.catalog_page).ok()
        } else {
            None
        };
        let metadata_state = if header.metadata_state_page != 0 {
            self.read_native_metadata_state_summary(header.metadata_state_page)
                .ok()
        } else {
            None
        };
        let vector_artifact_pages = if header.vector_artifact_page != 0 {
            self.read_native_vector_artifact_store(header.vector_artifact_page)
                .ok()
        } else {
            None
        };

        Ok(NativePhysicalState {
            header,
            collection_roots,
            manifest,
            registry,
            recovery,
            catalog,
            metadata_state,
            vector_artifact_pages,
        })
    }

    fn read_native_blob_chain_page_ids(&self, root_page: u32) -> Result<Vec<u32>, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native blob chain requires paged mode".to_string(),
            ));
        };
        if root_page == 0 {
            return Ok(Vec::new());
        }
        let mut pages = Vec::new();
        let mut current = root_page;
        while current != 0 {
            pages.push(current);
            let page = pager
                .read_page(current)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            let bytes = page.as_bytes();
            let content = &bytes[crate::storage::engine::HEADER_SIZE..];
            let (next_page, _) = reddb_file::decode_native_blob_page(content)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            current = next_page;
        }
        Ok(pages)
    }

    fn write_native_blob_chain(
        &self,
        payload: &[u8],
        existing_root: Option<u32>,
    ) -> Result<(u32, u32, u64), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok((0, 0, 0));
        };
        if payload.is_empty() {
            return Ok((0, 0, 0));
        }

        let chunk_capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let page_count = payload.len().div_ceil(chunk_capacity) as u32;
        let mut page_ids = existing_root
            .map(|root| self.read_native_blob_chain_page_ids(root))
            .transpose()?
            .unwrap_or_default();
        while page_ids.len() < page_count as usize {
            page_ids.push(
                pager
                    .allocate_page(crate::storage::engine::PageType::NativeMeta)
                    .map_err(|err| StoreError::Serialization(err.to_string()))?
                    .page_id(),
            );
        }

        for (index, chunk) in payload.chunks(chunk_capacity).enumerate() {
            let page_id = page_ids[index];
            let next_page = page_ids.get(index + 1).copied().unwrap_or(0);
            let data = reddb_file::encode_native_blob_page(next_page, chunk);

            let mut page = crate::storage::engine::Page::new(
                crate::storage::engine::PageType::NativeMeta,
                page_id,
            );
            let bytes = page.as_bytes_mut();
            let content_start = crate::storage::engine::HEADER_SIZE;
            let copy_len = data.len().min(bytes.len() - content_start);
            bytes[content_start..content_start + copy_len].copy_from_slice(&data[..copy_len]);
            pager
                .write_page(page_id, page)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
        }

        Ok((
            page_ids[0],
            page_count,
            crate::storage::engine::crc32(payload) as u64,
        ))
    }

    pub fn read_native_blob_chain(&self, root_page: u32) -> Result<Vec<u8>, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native blob chain requires paged mode".to_string(),
            ));
        };
        if root_page == 0 {
            return Ok(Vec::new());
        }
        let mut current = root_page;
        let mut payload = Vec::new();
        while current != 0 {
            let page = pager
                .read_page(current)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            let bytes = page.as_bytes();
            let content = &bytes[crate::storage::engine::HEADER_SIZE..];
            let (next_page, chunk) = reddb_file::decode_native_blob_page(content)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            payload.extend_from_slice(&chunk);
            current = next_page;
        }
        Ok(payload)
    }

    pub fn write_native_vector_artifact_store(
        &self,
        artifacts: &[(String, String, Vec<u8>)],
        existing_page: Option<u32>,
    ) -> Result<(u32, u64, Vec<NativeVectorArtifactPageSummary>), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok((0, 0, Vec::new()));
        };

        let existing = existing_page
            .map(|page| self.read_native_vector_artifact_store(page))
            .transpose()?
            .unwrap_or_default();
        let page_id = match existing_page.filter(|page| *page != 0) {
            Some(page) => page,
            None => pager
                .allocate_page(crate::storage::engine::PageType::NativeMeta)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
                .page_id(),
        };

        let mut summaries = Vec::new();
        for (collection, artifact_kind, bytes) in artifacts {
            let existing_root = existing
                .iter()
                .find(|entry| {
                    entry.collection == *collection && entry.artifact_kind == *artifact_kind
                })
                .map(|entry| entry.root_page);
            let (root_page, page_count, checksum) =
                self.write_native_blob_chain(bytes, existing_root)?;
            summaries.push(NativeVectorArtifactPageSummary {
                collection: collection.clone(),
                artifact_kind: artifact_kind.clone(),
                root_page,
                page_count,
                byte_len: bytes.len() as u64,
                checksum,
            });
        }

        let data = reddb_file::encode_native_vector_artifact_store_page(&summaries);
        let checksum = reddb_file::native_store_page_checksum(&data);
        let mut page = crate::storage::engine::Page::new(
            crate::storage::engine::PageType::NativeMeta,
            page_id,
        );
        let bytes = page.as_bytes_mut();
        let content_start = crate::storage::engine::HEADER_SIZE;
        let copy_len = data.len().min(bytes.len() - content_start);
        bytes[content_start..content_start + copy_len].copy_from_slice(&data[..copy_len]);
        pager
            .write_page(page_id, page)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        Ok((page_id, checksum, summaries))
    }

    pub fn read_native_vector_artifact_store(
        &self,
        page_id: u32,
    ) -> Result<Vec<NativeVectorArtifactPageSummary>, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native vector artifact store requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Ok(Vec::new());
        }
        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        reddb_file::decode_native_vector_artifact_store_page(content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    pub fn read_native_vector_artifact_blob(
        &self,
        page_id: u32,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> Result<Option<(NativeVectorArtifactPageSummary, Vec<u8>)>, StoreError> {
        let artifact_kind = artifact_kind.unwrap_or("hnsw");
        let summaries = self.read_native_vector_artifact_store(page_id)?;
        let Some(summary) = summaries.into_iter().find(|summary| {
            summary.collection == collection && summary.artifact_kind == artifact_kind
        }) else {
            return Ok(None);
        };
        let bytes = self.read_native_blob_chain(summary.root_page)?;
        Ok(Some((summary, bytes)))
    }
}
