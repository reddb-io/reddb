use super::*;
use reddb_file::native_artifact_source::{
    append_native_artifact_source_reference, decode_native_artifact_source_reference,
    decode_native_artifact_sources, encode_native_artifact_sources, NativeArtifactSourceProof,
    NativeArtifactSourceReference,
};
use reddb_file::native_blob_compression::{
    compress_native_blob, decompress_native_blob, encode_native_compressed_blob_page,
    native_blob_compression_header, NativeBlobCompressionHeader,
    NATIVE_COMPRESSED_BLOB_HEADER_BYTES,
};

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
        let mut visited = std::collections::HashSet::new();
        let mut current = root_page;
        while current != 0 {
            if !visited.insert(current) {
                return Err(StoreError::Serialization(
                    "cycle in native blob chain".into(),
                ));
            }
            pages.push(current);
            let page = pager
                .read_page(current)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            let bytes = page.as_bytes();
            let content = &bytes[crate::storage::engine::HEADER_SIZE..];
            if pages.len() != 1
                && native_blob_compression_header(content)
                    .map_err(|err| StoreError::Serialization(err.to_string()))?
                    .is_some()
            {
                return Err(StoreError::Serialization(
                    "compressed header inside native blob tail".into(),
                ));
            }
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
        self.write_native_blob_chain_with_compression(payload, existing_root, false)
    }

    fn write_native_blob_chain_with_compression(
        &self,
        payload: &[u8],
        existing_root: Option<u32>,
        compress: bool,
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
        let checksum = crate::storage::engine::crc32(payload);
        let encoded = if compress {
            compress_native_blob(payload, self.config.native_blob_decode_limit_bytes)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
        } else {
            None
        };
        let stored = encoded.as_deref().unwrap_or(payload);
        let first_capacity = if encoded.is_some() {
            crate::storage::engine::PAGE_SIZE
                - crate::storage::engine::HEADER_SIZE
                - NATIVE_COMPRESSED_BLOB_HEADER_BYTES
        } else {
            chunk_capacity
        };
        let page_count = u32::try_from(
            1 + stored
                .len()
                .saturating_sub(first_capacity)
                .div_ceil(chunk_capacity),
        )
        .map_err(|_| StoreError::Serialization("native blob exceeds page count limit".into()))?;
        let mut page_ids = existing_root
            .map(|root| self.read_native_blob_chain_page_ids(root))
            .transpose()?
            .unwrap_or_default();
        let existing_pages = page_ids.len();

        // Complete allocation of the new tail before changing any published
        // prefix. An allocation/writeback failure must leave that prefix
        // intact, as it did when allocation preceded every payload write.
        let prefix_pages = existing_pages.min(page_count as usize);
        for index in (prefix_pages..page_count as usize).chain(0..prefix_pages) {
            let start = if index == 0 {
                0
            } else {
                first_capacity + (index - 1) * chunk_capacity
            };
            let capacity = if index == 0 {
                first_capacity
            } else {
                chunk_capacity
            };
            let chunk = &stored[start..stored.len().min(start + capacity)];
            // Allocation admits a dirty blank page. Reserving the entire blob
            // first evicts a blank generation before writing its real payload.
            // One-page lookahead supplies the next link without that extra pass.
            let required = (index + 2).min(page_count as usize);
            while page_ids.len() < required {
                page_ids.push(
                    pager
                        .allocate_page(crate::storage::engine::PageType::NativeMeta)
                        .map_err(|err| StoreError::Serialization(err.to_string()))?
                        .page_id(),
                );
            }
            let page_id = page_ids[index];
            let next_page = if index + 1 < page_count as usize {
                page_ids[index + 1]
            } else {
                0
            };
            let data = if index == 0 && encoded.is_some() {
                encode_native_compressed_blob_page(
                    next_page,
                    NativeBlobCompressionHeader {
                        decoded_bytes: payload.len() as u64,
                        checksum,
                    },
                    chunk,
                )
            } else {
                reddb_file::encode_native_blob_page(next_page, chunk)
            };

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

        Ok((root_page, page_count, checksum as u64))
    }

    pub fn read_native_blob_chain(&self, root_page: u32) -> Result<Vec<u8>, StoreError> {
        self.read_native_blob_chain_with_page_count(root_page)
            .map(|(bytes, _)| bytes)
    }

    fn read_native_blob_chain_with_page_count(
        &self,
        root_page: u32,
    ) -> Result<(Vec<u8>, usize), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native blob chain requires paged mode".to_string(),
            ));
        };
        if root_page == 0 {
            return Ok((Vec::new(), 0));
        }
        let mut current = root_page;
        let mut payload = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut compression = None;
        while current != 0 {
            if !visited.insert(current) {
                return Err(StoreError::Serialization(
                    "cycle in native blob chain".into(),
                ));
            }
            let page = pager
                .read_page(current)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            let bytes = page.as_bytes();
            let content = &bytes[crate::storage::engine::HEADER_SIZE..];
            if let Some(header) = native_blob_compression_header(content)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
            {
                if visited.len() != 1 {
                    return Err(StoreError::Serialization(
                        "compressed header inside native blob tail".into(),
                    ));
                }
                if header.decoded_bytes == 0
                    || header.decoded_bytes > self.config.native_blob_decode_limit_bytes as u64
                {
                    return Err(StoreError::Serialization(format!(
                        "native blob decoded length {} exceeds workspace budget {} or is empty",
                        header.decoded_bytes, self.config.native_blob_decode_limit_bytes,
                    )));
                }
                compression = Some(header);
            }
            let (next_page, chunk) = reddb_file::decode_native_blob_page(content)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            if compression.is_some_and(|header| {
                payload.len().saturating_add(chunk.len()) as u64 > header.decoded_bytes
            }) {
                return Err(StoreError::Serialization(
                    "compressed native blob input exceeds decoded length".into(),
                ));
            }
            payload.extend_from_slice(&chunk);
            current = next_page;
        }
        if let Some(header) = compression {
            payload = decompress_native_blob(
                &payload,
                header,
                self.config.native_blob_decode_limit_bytes,
            )
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        }
        Ok((payload, visited.len()))
    }

    pub(crate) fn read_native_artifact_payload(
        &self,
        summary: &NativeVectorArtifactPageSummary,
    ) -> Result<Vec<u8>, StoreError> {
        let (bytes, pages) = self.read_native_blob_chain_with_page_count(summary.root_page)?;
        if bytes.len() as u64 != summary.byte_len
            || pages != summary.page_count as usize
            || reddb_file::native_store_page_checksum(&bytes) != summary.checksum
        {
            return Err(StoreError::Serialization(
                "native artifact length, page count or checksum mismatch".into(),
            ));
        }
        Ok(bytes)
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
        self.write_native_vector_artifact_store_with_sources(
            artifacts,
            retained,
            &[],
            existing_page,
        )
    }

    pub(crate) fn write_native_vector_artifact_store_with_sources(
        &self,
        artifacts: &[(String, String, Vec<u8>)],
        retained: &[NativeVectorArtifactPageSummary],
        sources: &[NativeArtifactSourceProof],
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
            let compress = artifact_kind == "text.fulltext"
                && bytes.starts_with(&reddb_file::native_index_artifact::NATIVE_FULLTEXT_MAGIC);
            let (root_page, page_count, checksum) =
                self.write_native_blob_chain_with_compression(bytes, existing_root, compress)?;
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

        let mut data = reddb_file::encode_native_vector_artifact_store_page(&summaries);
        let previous_source = existing_page
            .map(|page| self.read_native_artifact_source_reference(page))
            .transpose()?
            .flatten();
        if sources.is_empty() {
            if let Some(previous) = previous_source {
                self.detached_native_blob_pages
                    .lock()
                    .extend(self.read_native_blob_chain_page_ids(previous.root_page)?);
            }
        } else {
            let mut sources = sources.to_vec();
            for source in &mut sources {
                source.pages = summaries
                    .iter()
                    .filter(|page| page.collection == source.collection)
                    .cloned()
                    .collect();
            }
            sources.sort_by(|a, b| a.collection.cmp(&b.collection));
            let bytes = encode_native_artifact_sources(&sources)
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            let (root_page, _, checksum) = self.write_native_blob_chain(
                &bytes,
                previous_source.map(|reference| reference.root_page),
            )?;
            append_native_artifact_source_reference(
                &mut data,
                NativeArtifactSourceReference {
                    root_page,
                    checksum,
                },
            );
        }
        let checksum = reddb_file::native_store_page_checksum(&data);
        let mut page = crate::storage::engine::Page::new(
            crate::storage::engine::PageType::NativeMeta,
            page_id,
        );
        let bytes = page.as_bytes_mut();
        let content_start = crate::storage::engine::HEADER_SIZE;
        if data.len() > bytes.len() - content_start {
            return Err(StoreError::Serialization(
                "native artifact descriptors exceed one page".into(),
            ));
        }
        bytes[content_start..content_start + data.len()].copy_from_slice(&data);
        pager
            .write_page(page_id, page)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        Ok((page_id, checksum, summaries))
    }

    fn read_native_artifact_source_reference(
        &self,
        page_id: u32,
    ) -> Result<Option<NativeArtifactSourceReference>, StoreError> {
        if page_id == 0 {
            return Ok(None);
        }
        let content = self.read_native_meta_page(page_id, "native artifact sources")?;
        let summaries = reddb_file::decode_native_vector_artifact_store_page(&content)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let offset = reddb_file::encode_native_vector_artifact_store_page(&summaries).len();
        decode_native_artifact_source_reference(content.get(offset..).unwrap_or_default())
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    pub(crate) fn read_native_artifact_sources(
        &self,
        page_id: u32,
    ) -> Result<Vec<NativeArtifactSourceProof>, StoreError> {
        let _publication = self.vault_publication_lock.lock();
        let Some(reference) = self.read_native_artifact_source_reference(page_id)? else {
            return Ok(Vec::new());
        };
        let bytes = self.read_native_blob_chain(reference.root_page)?;
        if reddb_file::native_store_page_checksum(&bytes) != reference.checksum {
            return Err(StoreError::Serialization(
                "native artifact source checksum mismatch".into(),
            ));
        }
        decode_native_artifact_sources(&bytes)
            .map_err(|err| StoreError::Serialization(err.to_string()))
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
        let bytes = self.read_native_artifact_payload(&summary)?;
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

    fn text_frame(documents: u32, terms: u32, varied: bool) -> reddb_file::NativeFulltextFrame {
        fn mix(mut value: u64) -> u64 {
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            value ^ (value >> 31)
        }
        reddb_file::NativeFulltextFrame {
            collection: "packed_rows".into(),
            doc_count: documents,
            terms: (0..terms)
                .map(|term| {
                    let mut postings: Vec<_> = (0..u64::from(documents))
                        .filter(|id| {
                            !varied
                                || term == 0
                                || mix(*id ^ u64::from(term).wrapping_mul(0x9e3779b97f4a7c15)) >> 63
                                    == 0
                        })
                        .map(|id| reddb_file::NativeFulltextPosting {
                            entity_id: if varied { mix(id.wrapping_add(1)) } else { id },
                            term_count: if varied {
                                1 + (mix(id ^ u64::from(term).wrapping_mul(0x9e3779b97f4a7c15))
                                    as u32
                                    & 0xffff)
                            } else {
                                1
                            },
                        })
                        .collect();
                    postings.sort_unstable_by_key(|posting| posting.entity_id);
                    reddb_file::NativeFulltextTerm {
                        term: format!("shared_term_{term:04}"),
                        postings,
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn native_fulltext_blobs_pack_physical_pages_without_changing_payloads() {
        let directory = tempfile::tempdir().expect("packed native artifact fixture");
        let path = directory.path().join("packed.rdb");
        let store = UnifiedStore::open_with_config(
            &path,
            UnifiedStoreConfig::default().with_page_cache_slots(32),
        )
        .expect("paged store");
        let frame = text_frame(2048, 64, false);
        let bytes = reddb_file::encode_native_fulltext_frame(&frame);
        let page = publish_artifacts(
            &store,
            &[("packed_rows".into(), "text.fulltext".into(), bytes.clone())],
        );
        let (summary, actual) = store
            .read_native_vector_artifact_blob(page, "packed_rows", Some("text.fulltext"))
            .expect("artifact read")
            .expect("published artifact");
        assert_eq!(actual, bytes);
        assert_eq!(summary.byte_len as usize, bytes.len());
        assert_eq!(summary.checksum, crc32fast::hash(&bytes) as u64);
        let raw_pages = bytes.len().div_ceil(reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        ));
        assert!(summary.page_count as usize <= raw_pages / 4,
            "text postings should pack physical pages: logical_bytes={}, raw_pages={raw_pages}, physical_pages={}", bytes.len(), summary.page_count);
        eprintln!(
            "packed native logical_bytes={} raw_pages={raw_pages} physical_pages={}",
            bytes.len(),
            summary.page_count
        );
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("cold reopen packed artifact");
        let (_, actual) = reopened
            .read_native_vector_artifact_blob(page, "packed_rows", Some("text.fulltext"))
            .expect("cold artifact read")
            .expect("cold artifact");
        assert_eq!(actual, bytes);
        assert_eq!(
            reddb_file::decode_native_fulltext_frame(&actual).expect("logical frame"),
            frame
        );
    }

    #[test]
    fn native_fulltext_blobs_transition_legacy_raw_packed_and_budget_fallback() {
        let directory = tempfile::tempdir().expect("legacy native artifact fixture");
        let path = directory.path().join("transitions.rdb");
        let mut store = UnifiedStore::open(&path).expect("store");
        let bytes = reddb_file::encode_native_fulltext_frame(&text_frame(2048, 64, false));
        let artifact = ("packed_rows".into(), "text.fulltext".into(), bytes.clone());
        store.config.native_blob_decode_limit_bytes = 0;
        let page = publish_artifacts(&store, std::slice::from_ref(&artifact));
        let raw = store
            .read_native_vector_artifact_store(page)
            .expect("legacy descriptors");
        store.config.native_blob_decode_limit_bytes = 64 * 1024 * 1024;
        publish_artifacts(&store, std::slice::from_ref(&artifact));
        let packed = store
            .read_native_vector_artifact_store(page)
            .expect("packed descriptors");
        assert_eq!(packed[0].root_page, raw[0].root_page);
        assert!(packed[0].page_count < raw[0].page_count);
        assert_eq!(
            store
                .read_native_artifact_payload(&packed[0])
                .expect("packed read"),
            bytes
        );
        let mut stale = packed[0].clone();
        stale.page_count += 1;
        assert!(
            store.read_native_artifact_payload(&stale).is_err(),
            "physical counts must match the chain"
        );
        let original = store
            .pager()
            .expect("pager")
            .read_page(packed[0].root_page)
            .expect("packed page");
        let mut corrupt = original.clone();
        corrupt.as_bytes_mut()
            [crate::storage::engine::HEADER_SIZE + NATIVE_COMPRESSED_BLOB_HEADER_BYTES] ^= 1;
        store
            .pager()
            .expect("pager")
            .write_page(packed[0].root_page, corrupt)
            .expect("valid page with corrupt compressed payload");
        assert!(store.read_native_artifact_payload(&packed[0]).is_err());
        store
            .pager()
            .expect("pager")
            .write_page(packed[0].root_page, original)
            .expect("restore fixture");
        store.persist().expect("durable packed fixture");
        drop(store);
        let mut reopened = UnifiedStore::open(&path).expect("packed reopen");
        assert_eq!(
            reopened
                .read_native_artifact_payload(&packed[0])
                .expect("cold packed read"),
            bytes
        );
        reopened.config.native_blob_decode_limit_bytes = bytes.len() - 1;
        assert!(reopened
            .read_native_artifact_payload(&packed[0])
            .expect_err("decode budget")
            .to_string()
            .contains("budget"));
        publish_artifacts(&reopened, std::slice::from_ref(&artifact));
        let raw_again = reopened
            .read_native_vector_artifact_store(page)
            .expect("raw fallback");
        assert_eq!(raw_again[0].page_count, raw[0].page_count);
        assert_eq!(
            reopened
                .read_native_artifact_payload(&raw_again[0])
                .expect("legacy read remains valid below compression budget"),
            bytes
        );
    }

    #[test]
    fn compressed_native_growth_failure_preserves_the_old_artifact() {
        use crate::storage::engine::pager::FAIL_NATIVE_HOME_WRITE;
        let directory = tempfile::tempdir().expect("compressed failure fixture");
        let path = directory.path().join("packed-failure.rdb");
        let store = UnifiedStore::open_with_config(
            &path,
            UnifiedStoreConfig::default().with_page_cache_slots(32),
        )
        .expect("store");
        let old = reddb_file::encode_native_fulltext_frame(&text_frame(8, 2, false));
        let page = publish_artifacts(
            &store,
            &[("packed_rows".into(), "text.fulltext".into(), old.clone())],
        );
        let replacement = reddb_file::encode_native_fulltext_frame(&text_frame(8192, 64, true));
        let encoded =
            compress_native_blob(&replacement, store.config.native_blob_decode_limit_bytes)
                .expect("compress fixture")
                .expect("compressed fixture");
        assert!(
            encoded.len() > 64 * crate::storage::engine::PAGE_SIZE,
            "force compressed tail eviction"
        );
        FAIL_NATIVE_HOME_WRITE.set(true);
        let error = store
            .write_native_vector_artifact_store(
                &[("packed_rows".into(), "text.fulltext".into(), replacement)],
                Some(page),
            )
            .expect_err("interrupt compressed growth");
        assert!(error
            .to_string()
            .contains("injected native home write failure"));
        assert!(!FAIL_NATIVE_HOME_WRITE.get());
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("recover compressed growth");
        let (_, actual) = reopened
            .read_native_vector_artifact_blob(page, "packed_rows", Some("text.fulltext"))
            .expect("old descriptor")
            .expect("old artifact");
        assert_eq!(actual, old);
    }

    #[test]
    fn native_blob_cycle_is_rejected_before_read_or_retirement_can_loop() {
        let directory = tempfile::tempdir().expect("cyclic blob fixture");
        let store = UnifiedStore::open(&directory.path().join("cycle.rdb")).expect("store");
        let (root, _, _) = store
            .write_native_blob_chain(&vec![7; 64 * 1024], None)
            .expect("blob");
        store.persist().expect("blob publication");
        let pager = store.pager().expect("pager");
        let mut page = pager.read_page(root).expect("root");
        let (_, chunk) = reddb_file::decode_native_blob_page(
            &page.as_bytes()[crate::storage::engine::HEADER_SIZE..],
        )
        .expect("chunk");
        let cyclic = reddb_file::encode_native_blob_page(root, &chunk);
        page.as_bytes_mut()[crate::storage::engine::HEADER_SIZE
            ..crate::storage::engine::HEADER_SIZE + cyclic.len()]
            .copy_from_slice(&cyclic);
        pager
            .write_page(root, page)
            .expect("valid page with cyclic pointer");
        assert!(store
            .read_native_blob_chain(root)
            .expect_err("cyclic read")
            .to_string()
            .contains("cycle"));
        assert!(store
            .read_native_blob_chain_page_ids(root)
            .expect_err("cyclic retirement")
            .to_string()
            .contains("cycle"));
    }

    #[test]
    fn native_blob_growth_failure_preserves_the_published_payload() {
        use crate::storage::engine::pager::FAIL_NATIVE_HOME_WRITE;

        let directory = tempfile::tempdir().expect("blob failure fixture");
        let path = directory.path().join("blob-failure.rdb");
        let store = UnifiedStore::open_with_config(
            &path,
            UnifiedStoreConfig::default().with_page_cache_slots(32),
        )
        .expect("small-cache store");
        let capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let original = vec![7; capacity * 4];
        let (root, _, _) = store
            .write_native_blob_chain(&original, None)
            .expect("original blob");
        store.persist().expect("publish original");
        FAIL_NATIVE_HOME_WRITE.set(true);
        let error = store
            .write_native_blob_chain(&vec![9; capacity * 512], Some(root))
            .expect_err("interrupt growth");
        assert!(error
            .to_string()
            .contains("injected native home write failure"));
        assert!(
            !FAIL_NATIVE_HOME_WRITE.get(),
            "the failure must reach native home I/O"
        );
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("recover staged growth frame");
        assert_eq!(
            reopened
                .read_native_blob_chain(root)
                .expect("old published blob"),
            original,
            "allocation failure must not replace the published prefix"
        );
    }

    #[test]
    fn native_blob_growth_does_not_flush_an_entire_blank_generation() {
        use crate::storage::engine::pager::NATIVE_PAGE_IO_COUNTS;

        let directory = tempfile::tempdir().expect("blob I/O fixture");
        let path = directory.path().join("blob-growth.rdb");
        let store = UnifiedStore::open_with_config(
            &path,
            UnifiedStoreConfig::default().with_page_cache_slots(32),
        )
        .expect("small-cache store");
        let capacity = reddb_file::native_blob_chunk_capacity(
            crate::storage::engine::PAGE_SIZE,
            crate::storage::engine::HEADER_SIZE,
        );
        let payload = vec![7; capacity * 512];
        NATIVE_PAGE_IO_COUNTS.set(Default::default());
        let (root, pages, _) = store
            .write_native_blob_chain(&payload, None)
            .expect("new blob");
        store.persist().expect("durable blob");
        let counts = NATIVE_PAGE_IO_COUNTS.get();
        assert_eq!(pages, 512);
        assert_eq!(
            store.read_native_blob_chain(root).expect("read blob"),
            payload
        );
        eprintln!("native blob pages={pages} I/O={counts:?}");
        // A bounded eviction frame may include the one-page lookahead. It
        // must not write a complete blank copy before the payload generation.
        let frame_pages = reddb_engine::storage::pager_io::DWB_MAX_PAGES_PER_FRAME;
        let blank_limit = (pages as usize).div_ceil(frame_pages) * 2 + 2;
        assert!(
            counts.blank_native_home_writes <= blank_limit,
            "preallocated blank pages amplified protected I/O: {counts:?}, limit={blank_limit}"
        );
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("cold reopen");
        assert_eq!(
            reopened.read_native_blob_chain(root).expect("cold blob"),
            payload
        );
    }

    #[test]
    fn native_source_proof_blob_reuses_space_and_retires_removed_pages() {
        let directory = tempfile::tempdir().expect("source proof fixture");
        let path = directory.path().join("source-proofs.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let mut sources: Vec<_> = (0..256)
            .map(|index| NativeArtifactSourceProof {
                collection: format!("collection_{index:04}"),
                source_digest: [17; 32],
                summaries: Vec::new(),
                pages: Vec::new(),
                fulltext_document_ids_complete: false,
            })
            .collect();
        let publish = |sources: &[NativeArtifactSourceProof]| {
            let mut header = store.physical_file_header().expect("header");
            let previous =
                (header.vector_artifact_page != 0).then_some(header.vector_artifact_page);
            let (page, checksum, _) = store
                .write_native_vector_artifact_store_with_sources(&[], &[], sources, previous)
                .expect("proof publication");
            header.vector_artifact_page = page;
            header.vector_artifact_checksum = checksum;
            store
                .update_physical_file_header(header)
                .expect("header publication");
            store.persist().expect("durable proof");
            store
                .publish_detached_native_blob_pages()
                .expect("proof quarantine");
            page
        };
        let page = publish(&sources);
        let reference = store
            .read_native_artifact_source_reference(page)
            .expect("reference")
            .expect("proof root");
        let pages = store
            .read_native_blob_chain_page_ids(reference.root_page)
            .expect("proof pages");
        assert!(
            pages.len() > 1,
            "proofs must cross a native metadata page boundary"
        );
        assert_eq!(
            store
                .read_native_artifact_sources(page)
                .expect("large proof"),
            sources
        );
        let length = std::fs::metadata(&path).expect("file length").len();
        for cycle in 0..12 {
            sources[0]
                .source_digest
                .fill(if cycle % 2 == 0 { 18 } else { 17 });
            let page = publish(&sources);
            assert_eq!(
                store
                    .read_native_artifact_source_reference(page)
                    .expect("reference")
                    .expect("root")
                    .root_page,
                reference.root_page
            );
            assert_eq!(
                store
                    .read_native_artifact_sources(page)
                    .expect("updated proof"),
                sources
            );
            assert_eq!(
                std::fs::metadata(&path).expect("file length").len(),
                length,
                "proof growth at cycle {cycle}"
            );
        }
        publish(&sources[..1]);
        let (pending, published) = store
            .pager()
            .expect("pager")
            .deferred_reclamation_pages()
            .expect("retirement");
        assert_eq!(pending.len(), pages.len() - 1);
        assert_eq!(published.len(), pending.len());
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("reopen shortened proof");
        assert_eq!(
            reopened
                .read_native_artifact_sources(page)
                .expect("short proof survives"),
            sources[..1]
        );
        reopened
            .flush_paged_registry()
            .expect("release retired pages");
        let mut header = reopened.physical_file_header().expect("header");
        let (page, checksum, _) = reopened
            .write_native_vector_artifact_store(&[], Some(page))
            .expect("legacy publication removes reference");
        header.vector_artifact_page = page;
        header.vector_artifact_checksum = checksum;
        reopened
            .update_physical_file_header(header)
            .expect("legacy header");
        reopened.persist().expect("legacy publication");
        reopened
            .publish_detached_native_blob_pages()
            .expect("retire final proof page");
        assert!(reopened
            .read_native_artifact_sources(page)
            .expect("legacy fallback")
            .is_empty());
        let (pending, _) = reopened
            .pager()
            .expect("pager")
            .deferred_reclamation_pages()
            .expect("final retirement");
        assert_eq!(pending.len(), 1);
    }

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
