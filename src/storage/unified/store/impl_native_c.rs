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
            if content.len() < 12 || &content[0..4] != NATIVE_BLOB_MAGIC {
                return Err(StoreError::Serialization(
                    "invalid native blob page".to_string(),
                ));
            }
            current = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
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

        let chunk_capacity =
            crate::storage::engine::PAGE_SIZE - crate::storage::engine::HEADER_SIZE - 12;
        let page_count = ((payload.len() + chunk_capacity - 1) / chunk_capacity) as u32;
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
            let mut data = Vec::with_capacity(chunk.len() + 12);
            data.extend_from_slice(NATIVE_BLOB_MAGIC);
            data.extend_from_slice(&next_page.to_le_bytes());
            data.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            data.extend_from_slice(chunk);

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
            if content.len() < 12 || &content[0..4] != NATIVE_BLOB_MAGIC {
                return Err(StoreError::Serialization(
                    "invalid native blob page".to_string(),
                ));
            }
            let next_page = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
            let chunk_len =
                u32::from_le_bytes([content[8], content[9], content[10], content[11]]) as usize;
            if 12 + chunk_len > content.len() {
                return Err(StoreError::Serialization(
                    "truncated native blob page".to_string(),
                ));
            }
            payload.extend_from_slice(&content[12..12 + chunk_len]);
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

        let mut data = Vec::with_capacity(1024 + summaries.len() * 64);
        data.extend_from_slice(NATIVE_VECTOR_ARTIFACT_MAGIC);
        data.extend_from_slice(&(summaries.len() as u32).to_le_bytes());
        for summary in &summaries {
            push_native_string(&mut data, &summary.collection);
            push_native_string(&mut data, &summary.artifact_kind);
            data.extend_from_slice(&summary.root_page.to_le_bytes());
            data.extend_from_slice(&summary.page_count.to_le_bytes());
            data.extend_from_slice(&summary.byte_len.to_le_bytes());
            data.extend_from_slice(&summary.checksum.to_le_bytes());
        }

        let checksum = crate::storage::engine::crc32(&data) as u64;
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
        if content.len() < 8 || &content[0..4] != NATIVE_VECTOR_ARTIFACT_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native vector artifact store page".to_string(),
            ));
        }
        let count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]) as usize;
        let mut pos = 8usize;
        let mut summaries = Vec::with_capacity(count);
        for _ in 0..count {
            let collection = read_native_string(content, &mut pos)?;
            let artifact_kind = read_native_string(content, &mut pos)?;
            if pos + 24 > content.len() {
                break;
            }
            let root_page = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let page_count = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let byte_len = u64::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
                content[pos + 4],
                content[pos + 5],
                content[pos + 6],
                content[pos + 7],
            ]);
            pos += 8;
            let checksum = u64::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
                content[pos + 4],
                content[pos + 5],
                content[pos + 6],
                content[pos + 7],
            ]);
            pos += 8;
            summaries.push(NativeVectorArtifactPageSummary {
                collection,
                artifact_kind,
                root_page,
                page_count,
                byte_len,
                checksum,
            });
        }
        Ok(summaries)
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
