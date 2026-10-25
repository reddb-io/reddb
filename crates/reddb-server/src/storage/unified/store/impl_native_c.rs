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
            if let Some(root) = existing_root {
                self.detached_native_blob_pages
                    .lock()
                    .extend(self.read_native_blob_chain_page_ids(root)?);
            }
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
        let existing_pages = page_ids.len();
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
            let next_page = if index + 1 < page_count as usize {
                page_ids[index + 1]
            } else {
                0
            };
            let data = reddb_file::encode_native_blob_page(next_page, chunk);

            let mut page = crate::storage::engine::Page::new(
                crate::storage::engine::PageType::NativeMeta,
                page_id,
            );
            let bytes = page.as_bytes_mut();
            let content_start = crate::storage::engine::HEADER_SIZE;
            let copy_len = data.len().min(bytes.len() - content_start);
            bytes[content_start..content_start + copy_len].copy_from_slice(&data[..copy_len]);
            if index < existing_pages {
                let previous = pager
                    .read_page(page_id)
                    .map_err(|err| StoreError::Serialization(err.to_string()))?;
                // Compare actual encoded bytes, including next-page links and
                // cleared tails. A checksum alone is not an equality proof.
                if previous.as_bytes()[content_start..] == page.as_bytes()[content_start..] {
                    continue;
                }
            }
            pager
                .write_page(page_id, page)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
        }

        let root_page = page_ids[0];
        let mut detached = self.detached_native_blob_pages.lock();
        // A retry after an unsuccessful summary publication may retain a root
        // previously staged for removal. Its current pages are live again.
        for page in &page_ids[..page_count as usize] {
            detached.remove(page);
        }
        detached.extend(page_ids.into_iter().skip(page_count as usize));

        Ok((
            root_page,
            page_count,
            crate::storage::engine::crc32(payload) as u64,
        ))
    }

    pub fn read_native_blob_chain(&self, root_page: u32) -> Result<Vec<u8>, StoreError> {
        let _publication = self.vault_publication_lock.lock();
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
        self.write_native_vector_artifact_store_retaining(artifacts, &[], existing_page)
    }

    pub(crate) fn write_native_vector_artifact_store_retaining(
        &self,
        artifacts: &[(String, String, Vec<u8>)],
        retained: &[NativeVectorArtifactPageSummary],
        existing_page: Option<u32>,
    ) -> Result<(u32, u64, Vec<NativeVectorArtifactPageSummary>), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        let Some(pager) = &self.pager else {
            return Ok((0, 0, Vec::new()));
        };

        let existing = existing_page
            .map(|page| self.read_native_vector_artifact_store(page))
            .transpose()?
            .unwrap_or_default();
        if retained.iter().any(|entry| {
            !existing.contains(entry)
                || artifacts.iter().any(|(collection, kind, _)| {
                    entry.collection == *collection && entry.artifact_kind == *kind
                })
        }) {
            return Err(StoreError::Serialization(
                "retained native artifact does not match the published generation".to_string(),
            ));
        }
        let page_id = match existing_page.filter(|page| *page != 0) {
            Some(page) => page,
            None => pager
                .allocate_page(crate::storage::engine::PageType::NativeMeta)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
                .page_id(),
        };

        let mut summaries = retained.to_vec();
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

        for previous in &existing {
            if !retained.contains(previous)
                && !artifacts.iter().any(|(collection, kind, _)| {
                    previous.collection == *collection && previous.artifact_kind == *kind
                })
            {
                self.detached_native_blob_pages
                    .lock()
                    .extend(self.read_native_blob_chain_page_ids(previous.root_page)?);
            }
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
        let _publication = self.vault_publication_lock.lock();
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

    /// Call only after the native summary and physical header have been synced.
    /// A failed publication retains these pages; a crash before ledger admission
    /// can leave conservative garbage, but never returns a live page for reuse.
    pub(crate) fn publish_detached_native_blob_pages(&self) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        let Some(pager) = &self.pager else {
            return Ok(());
        };
        let mut detached = self.detached_native_blob_pages.lock();
        if detached.is_empty() {
            return Ok(());
        }
        while let Some(&page) = detached.first() {
            pager
                .free_page(page)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            detached.remove(&page);
        }
        drop(detached);
        // Use the existing RDF1 publication fence. Pages cannot enter the
        // allocator until a later durable catalog forgets this quarantine.
        self.flush_paged_registry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_native_blob_pages_remain_clean() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("unchanged-blob.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let mut payload = vec![7; capacity * 3];
        let (root, _, _) = store
            .write_native_blob_chain(&payload, None)
            .expect("initial blob");
        store.persist().expect("durable initial blob");
        let pager = store.pager().expect("pager");
        assert_eq!(pager.dirty_page_count(), 0);

        store
            .write_native_blob_chain(&payload, Some(root))
            .expect("unchanged publication");
        assert_eq!(
            pager.dirty_page_count(),
            0,
            "an unchanged native index must not re-enter the WAL and dirty-page queue"
        );

        payload[capacity + 13] = 9;
        store
            .write_native_blob_chain(&payload, Some(root))
            .expect("single-page change");
        assert_eq!(pager.dirty_page_count(), 1);
        store.persist().expect("durable changed blob");
        drop(store);

        let reopened = UnifiedStore::open(&path).expect("cold reopen");
        assert_eq!(
            reopened.read_native_blob_chain(root).expect("durable blob"),
            payload
        );
    }

    #[test]
    fn shrinking_native_blob_does_not_read_the_previous_tail() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("blob.rdb")).expect("paged store");
        let capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let original = vec![7; capacity * 3];
        let (root, count, _) = store
            .write_native_blob_chain(&original, None)
            .expect("original blob");
        assert_eq!(count, 3);
        store.persist().expect("publish original blob");

        let replacement = vec![9; capacity / 2];
        let (root, count, checksum) = store
            .write_native_blob_chain(&replacement, Some(root))
            .expect("smaller replacement");
        assert_eq!(count, 1);
        let actual = store
            .read_native_blob_chain(root)
            .expect("read replacement");
        assert_eq!(
            actual.len(),
            replacement.len(),
            "old artifact tail survived shrink"
        );
        assert_eq!(actual, replacement);
        assert_eq!(checksum, crate::storage::engine::crc32(&actual) as u64);
    }

    fn publish_artifacts(store: &UnifiedStore, artifacts: &[(String, String, Vec<u8>)]) -> u32 {
        let mut header = store.physical_file_header().expect("physical header");
        let previous = (header.vector_artifact_page != 0).then_some(header.vector_artifact_page);
        let (page, checksum, _) = store
            .write_native_vector_artifact_store(artifacts, previous)
            .expect("write artifact summary");
        header.vector_artifact_page = page;
        header.vector_artifact_checksum = checksum;
        store
            .update_physical_file_header(header)
            .expect("new header");
        store.persist().expect("durable native publication");
        store
            .publish_detached_native_blob_pages()
            .expect("quarantine detached pages");
        page
    }

    #[test]
    fn retained_native_artifact_requires_the_exact_published_reference() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("retain.rdb")).expect("store");
        let artifact = ("rows".into(), "text.fulltext".into(), vec![7; 1024]);
        let page = publish_artifacts(&store, &[artifact]);
        let mut retained = store
            .read_native_vector_artifact_store(page)
            .expect("references");
        retained[0].checksum ^= 1;
        let pager = store.pager().expect("pager");
        let before = pager.page_count().expect("page count");
        assert!(store
            .write_native_vector_artifact_store_retaining(&[], &retained, Some(page))
            .is_err());
        assert_eq!(pager.page_count().expect("unchanged page count"), before);
        assert_eq!(pager.dirty_page_count(), 0);
    }

    #[test]
    fn native_blob_retirement_survives_reopen_and_reuses_pages() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("retired-blob.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let large = ("rows".into(), "text.fulltext".into(), vec![7; capacity * 4]);
        let small = ("rows".into(), "text.fulltext".into(), vec![9; capacity / 2]);
        publish_artifacts(&store, std::slice::from_ref(&large));
        let size = std::fs::metadata(&path).expect("allocated file").len();
        let page = publish_artifacts(&store, std::slice::from_ref(&small));
        let (pending, published) = store
            .pager()
            .expect("pager")
            .deferred_reclamation_pages()
            .expect("quarantine");
        assert_eq!(pending.len(), 3, "shrunk tail must remain quarantined");
        assert_eq!(published.len(), 3);
        drop(store);

        let store = UnifiedStore::open(&path).expect("reopen published quarantine");
        let (summary, bytes) = store
            .read_native_vector_artifact_blob(page, "rows", Some("text.fulltext"))
            .expect("read artifact")
            .expect("artifact survives");
        assert_eq!(summary.page_count, 1);
        assert_eq!(summary.byte_len as usize, bytes.len());
        assert_eq!(bytes, small.2);
        store
            .flush_paged_registry()
            .expect("release published tail");
        assert!(
            store
                .pager()
                .expect("pager")
                .free_page_count()
                .expect("free pages")
                >= 2,
            "one reclaimed page hosts the allocator trunk"
        );
        // One three-page tail may still be quarantined when the next large
        // artifact is allocated. After that warmup, equal churn must plateau.
        let maximum_size = size + 3 * crate::storage::engine::PAGE_SIZE as u64;
        let mut plateau = None;
        for cycle in 0..8 {
            publish_artifacts(&store, std::slice::from_ref(&large));
            publish_artifacts(&store, std::slice::from_ref(&small));
            let current_size = std::fs::metadata(&path).expect("reused allocation").len();
            assert!(current_size <= maximum_size);
            let free = store
                .pager()
                .expect("pager")
                .free_page_count()
                .expect("free pages");
            if cycle >= 2 {
                assert_eq!(
                    Some((current_size, free)),
                    plateau,
                    "equal churn must preserve allocation and free-page accounting"
                );
            }
            plateau = Some((current_size, free));
        }
        publish_artifacts(&store, &[]);
        store
            .flush_paged_registry()
            .expect("release dropped artifact");
        assert!(store
            .read_native_vector_artifact_store(page)
            .expect("empty summary")
            .is_empty());
        assert!(store
            .pager()
            .expect("pager")
            .deferred_reclamation_pages()
            .expect("released quarantine")
            .0
            .is_empty());
        // Trunk pages are also reclaimed on allocation; check actual reuse
        // rather than counting only the free IDs stored inside those trunks.
        publish_artifacts(&store, std::slice::from_ref(&large));
        assert!(std::fs::metadata(&path).expect("reuse after drop").len() <= maximum_size);
        assert_eq!(
            store
                .read_native_vector_artifact_blob(page, "rows", Some("text.fulltext"))
                .expect("recreated artifact")
                .expect("artifact exists")
                .1,
            large.2
        );
    }

    #[test]
    fn retained_blob_retry_cancels_its_unpublished_retirement() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store =
            UnifiedStore::open(&directory.path().join("retry-blob.rdb")).expect("paged store");
        let bytes = vec![42; 1024];
        let (root, _, _) = store
            .write_native_blob_chain(&bytes, None)
            .expect("original blob");
        store
            .write_native_blob_chain(&[], Some(root))
            .expect("stage removal");
        store
            .write_native_blob_chain(&bytes, Some(root))
            .expect("retain on retry");
        store.persist().expect("retry publication");
        store
            .publish_detached_native_blob_pages()
            .expect("no live retirement");
        store.flush_paged_registry().expect("later publication");
        assert_eq!(
            store.read_native_blob_chain(root).expect("retained page"),
            bytes
        );
        assert_eq!(
            store
                .pager()
                .expect("pager")
                .free_page_count()
                .expect("no live free"),
            0
        );
    }
}
