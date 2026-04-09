use super::*;

impl UnifiedStore {
    pub fn update_physical_file_header(
        &self,
        physical: PhysicalFileHeader,
    ) -> Result<(), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(());
        };
        pager
            .update_physical_header(physical)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    /// Read the minimal physical header mirrored into page 0 for paged databases.
    pub fn physical_file_header(&self) -> Option<PhysicalFileHeader> {
        self.pager.as_ref().and_then(|pager| pager.physical_header().ok())
    }

    /// Persist native collection roots into a dedicated page in the paged file.
    pub fn write_native_collection_roots(
        &self,
        roots: &BTreeMap<String, u64>,
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok((0, 0));
        };

        let page_id = match existing_page.filter(|page| *page != 0) {
            Some(page) => page,
            None => pager
                .allocate_page(crate::storage::engine::PageType::NativeMeta)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
                .page_id(),
        };

        let mut data = Vec::with_capacity(1024);
        data.extend_from_slice(NATIVE_COLLECTION_ROOTS_MAGIC);
        data.extend_from_slice(&(roots.len() as u32).to_le_bytes());
        for (collection, root) in roots {
            data.extend_from_slice(&(collection.len() as u32).to_le_bytes());
            data.extend_from_slice(collection.as_bytes());
            data.extend_from_slice(&root.to_le_bytes());
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
        Ok((page_id, checksum))
    }

    /// Read native collection roots from a dedicated page in the paged file.
    pub fn read_native_collection_roots(
        &self,
        page_id: u32,
    ) -> Result<BTreeMap<String, u64>, StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(BTreeMap::new());
        };
        if page_id == 0 {
            return Ok(BTreeMap::new());
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 8 || &content[0..4] != NATIVE_COLLECTION_ROOTS_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native collection roots page".to_string(),
            ));
        }

        let count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]) as usize;
        let mut pos = 8usize;
        let mut roots = BTreeMap::new();

        for _ in 0..count {
            if pos + 4 > content.len() {
                break;
            }
            let name_len = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]) as usize;
            pos += 4;
            if pos + name_len + 8 > content.len() {
                break;
            }
            let name = String::from_utf8(content[pos..pos + name_len].to_vec())
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            pos += name_len;
            let root = u64::from_le_bytes([
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
            roots.insert(name, root);
        }

        Ok(roots)
    }

    /// Persist a compact native manifest summary into a dedicated page in the paged file.
    pub fn write_native_manifest_summary(
        &self,
        sequence: u64,
        events: &[ManifestEvent],
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok((0, 0));
        };

        let page_id = match existing_page.filter(|page| *page != 0) {
            Some(page) => page,
            None => pager
                .allocate_page(crate::storage::engine::PageType::NativeMeta)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
                .page_id(),
        };

        let sample_start = events.len().saturating_sub(NATIVE_MANIFEST_SAMPLE_LIMIT);
        let sample = &events[sample_start..];

        let mut data = Vec::with_capacity(1024);
        data.extend_from_slice(NATIVE_MANIFEST_MAGIC);
        data.extend_from_slice(&sequence.to_le_bytes());
        data.extend_from_slice(&(events.len() as u32).to_le_bytes());
        data.push(u8::from(events.len() <= NATIVE_MANIFEST_SAMPLE_LIMIT));
        data.extend_from_slice(
            &(events.len().saturating_sub(sample.len()) as u32).to_le_bytes(),
        );
        data.extend_from_slice(&(sample.len() as u32).to_le_bytes());
        for event in sample {
            data.push(native_manifest_kind_to_byte(event.kind));
            data.extend_from_slice(&(event.collection.len() as u16).to_le_bytes());
            data.extend_from_slice(event.collection.as_bytes());
            data.extend_from_slice(&(event.object_key.len() as u16).to_le_bytes());
            data.extend_from_slice(event.object_key.as_bytes());
            data.extend_from_slice(&event.block.index.to_le_bytes());
            data.extend_from_slice(&event.block.checksum.to_le_bytes());
            data.extend_from_slice(&event.snapshot_min.to_le_bytes());
            match event.snapshot_max {
                Some(value) => {
                    data.push(1);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                None => data.push(0),
            }
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
        Ok((page_id, checksum))
    }

    /// Read a compact native manifest summary from a dedicated page in the paged file.
    pub fn read_native_manifest_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeManifestSummary, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native manifest summary requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(
                "native manifest summary page is not set".to_string(),
            ));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 25 || &content[0..4] != NATIVE_MANIFEST_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native manifest summary page".to_string(),
            ));
        }

        let sequence = u64::from_le_bytes([
            content[4], content[5], content[6], content[7], content[8], content[9], content[10],
            content[11],
        ]);
        let event_count =
            u32::from_le_bytes([content[12], content[13], content[14], content[15]]);
        let events_complete = content[16] == 1;
        let omitted_event_count =
            u32::from_le_bytes([content[17], content[18], content[19], content[20]]);
        let sample_count =
            u32::from_le_bytes([content[21], content[22], content[23], content[24]]) as usize;

        let mut pos = 25usize;
        let mut recent_events = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            if pos + 1 + 2 > content.len() {
                break;
            }
            let kind = native_manifest_kind_from_byte(content[pos]).to_string();
            pos += 1;
            let collection_len =
                u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
            pos += 2;
            if pos + collection_len + 2 > content.len() {
                break;
            }
            let collection = String::from_utf8(content[pos..pos + collection_len].to_vec())
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            pos += collection_len;
            let object_key_len =
                u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
            pos += 2;
            if pos + object_key_len + 8 + 16 + 8 + 1 > content.len() {
                break;
            }
            let object_key = String::from_utf8(content[pos..pos + object_key_len].to_vec())
                .map_err(|err| StoreError::Serialization(err.to_string()))?;
            pos += object_key_len;
            let block_index = u64::from_le_bytes([
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
            let mut checksum_bytes = [0u8; 16];
            checksum_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let snapshot_min = u64::from_le_bytes([
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
            let snapshot_max = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    if pos + 8 > content.len() {
                        return Err(StoreError::Serialization(
                            "truncated native manifest snapshot_max".to_string(),
                        ));
                    }
                    let value = u64::from_le_bytes([
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
                    Some(value)
                }
                Some(_) => {
                    pos += 1;
                    None
                }
                None => None,
            };

            recent_events.push(NativeManifestEntrySummary {
                collection,
                object_key,
                kind,
                block_index,
                block_checksum: u128::from_le_bytes(checksum_bytes),
                snapshot_min,
                snapshot_max,
            });
        }

        Ok(NativeManifestSummary {
            sequence,
            event_count,
            events_complete,
            omitted_event_count,
            recent_events,
        })
    }

    /// Persist a compact native operational registry summary into a dedicated page.
    pub fn write_native_registry_summary(
        &self,
        summary: &NativeRegistrySummary,
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok((0, 0));
        };

        let page_id = match existing_page.filter(|page| *page != 0) {
            Some(page) => page,
            None => pager
                .allocate_page(crate::storage::engine::PageType::NativeMeta)
                .map_err(|err| StoreError::Serialization(err.to_string()))?
                .page_id(),
        };

        let mut data = Vec::with_capacity(2048);
        data.extend_from_slice(NATIVE_REGISTRY_MAGIC);
        data.extend_from_slice(&summary.collection_count.to_le_bytes());
        data.extend_from_slice(&summary.index_count.to_le_bytes());
        data.extend_from_slice(&summary.graph_projection_count.to_le_bytes());
        data.extend_from_slice(&summary.analytics_job_count.to_le_bytes());
        data.extend_from_slice(&summary.vector_artifact_count.to_le_bytes());
        data.push(u8::from(summary.collections_complete));
        data.push(u8::from(summary.indexes_complete));
        data.push(u8::from(summary.graph_projections_complete));
        data.push(u8::from(summary.analytics_jobs_complete));
        data.push(u8::from(summary.vector_artifacts_complete));
        data.extend_from_slice(&summary.omitted_collection_count.to_le_bytes());
        data.extend_from_slice(&summary.omitted_index_count.to_le_bytes());
        data.extend_from_slice(&summary.omitted_graph_projection_count.to_le_bytes());
        data.extend_from_slice(&summary.omitted_analytics_job_count.to_le_bytes());
        data.extend_from_slice(&summary.omitted_vector_artifact_count.to_le_bytes());
        data.extend_from_slice(&(summary.collection_names.len() as u32).to_le_bytes());
        data.extend_from_slice(&(summary.indexes.len() as u32).to_le_bytes());
        data.extend_from_slice(&(summary.graph_projections.len() as u32).to_le_bytes());
        data.extend_from_slice(&(summary.analytics_jobs.len() as u32).to_le_bytes());
        data.extend_from_slice(&(summary.vector_artifacts.len() as u32).to_le_bytes());

        for name in &summary.collection_names {
            push_native_string(&mut data, name);
        }
        for index in &summary.indexes {
            push_native_string(&mut data, &index.name);
            push_native_string(&mut data, &index.kind);
            match &index.collection {
                Some(collection) => {
                    data.push(1);
                    push_native_string(&mut data, collection);
                }
                None => data.push(0),
            }
            data.push(u8::from(index.enabled));
            data.extend_from_slice(&index.entries.to_le_bytes());
            data.extend_from_slice(&index.estimated_memory_bytes.to_le_bytes());
            match index.last_refresh_ms {
                Some(value) => {
                    data.push(1);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                None => data.push(0),
            }
            push_native_string(&mut data, &index.backend);
        }
        for projection in &summary.graph_projections {
            push_native_string(&mut data, &projection.name);
            push_native_string(&mut data, &projection.source);
            data.extend_from_slice(&projection.created_at_unix_ms.to_le_bytes());
            data.extend_from_slice(&projection.updated_at_unix_ms.to_le_bytes());
            push_native_string_list(&mut data, &projection.node_labels);
            push_native_string_list(&mut data, &projection.node_types);
            push_native_string_list(&mut data, &projection.edge_labels);
            match projection.last_materialized_sequence {
                Some(value) => {
                    data.push(1);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                None => data.push(0),
            }
        }
        for job in &summary.analytics_jobs {
            push_native_string(&mut data, &job.id);
            push_native_string(&mut data, &job.kind);
            match &job.projection {
                Some(projection) => {
                    data.push(1);
                    push_native_string(&mut data, projection);
                }
                None => data.push(0),
            }
            push_native_string(&mut data, &job.state);
            data.extend_from_slice(&job.created_at_unix_ms.to_le_bytes());
            data.extend_from_slice(&job.updated_at_unix_ms.to_le_bytes());
            match job.last_run_sequence {
                Some(value) => {
                    data.push(1);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                None => data.push(0),
            }
            let metadata_count = job.metadata.len().min(u16::MAX as usize) as u16;
            data.extend_from_slice(&metadata_count.to_le_bytes());
            for (key, value) in job.metadata.iter().take(metadata_count as usize) {
                push_native_string(&mut data, key);
                push_native_string(&mut data, value);
            }
        }
        for artifact in &summary.vector_artifacts {
            push_native_string(&mut data, &artifact.collection);
            push_native_string(&mut data, &artifact.artifact_kind);
            data.extend_from_slice(&artifact.vector_count.to_le_bytes());
            data.extend_from_slice(&artifact.dimension.to_le_bytes());
            data.extend_from_slice(&artifact.max_layer.to_le_bytes());
            data.extend_from_slice(&artifact.serialized_bytes.to_le_bytes());
            data.extend_from_slice(&artifact.checksum.to_le_bytes());
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
        Ok((page_id, checksum))
    }

}
