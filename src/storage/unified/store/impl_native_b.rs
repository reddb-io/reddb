use super::*;

impl UnifiedStore {
    pub fn read_native_registry_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeRegistrySummary, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native registry summary requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(
                "native registry summary page is not set".to_string(),
            ));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 77 || &content[0..4] != NATIVE_REGISTRY_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native registry summary page".to_string(),
            ));
        }

        let collection_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
        let index_count = u32::from_le_bytes([content[8], content[9], content[10], content[11]]);
        let graph_projection_count =
            u32::from_le_bytes([content[12], content[13], content[14], content[15]]);
        let analytics_job_count =
            u32::from_le_bytes([content[16], content[17], content[18], content[19]]);
        let vector_artifact_count =
            u32::from_le_bytes([content[20], content[21], content[22], content[23]]);
        let collections_complete = content[24] == 1;
        let indexes_complete = content[25] == 1;
        let graph_projections_complete = content[26] == 1;
        let analytics_jobs_complete = content[27] == 1;
        let vector_artifacts_complete = content[28] == 1;
        let omitted_collection_count =
            u32::from_le_bytes([content[29], content[30], content[31], content[32]]);
        let omitted_index_count =
            u32::from_le_bytes([content[33], content[34], content[35], content[36]]);
        let omitted_graph_projection_count =
            u32::from_le_bytes([content[37], content[38], content[39], content[40]]);
        let omitted_analytics_job_count =
            u32::from_le_bytes([content[41], content[42], content[43], content[44]]);
        let omitted_vector_artifact_count =
            u32::from_le_bytes([content[45], content[46], content[47], content[48]]);
        let collection_sample_count =
            u32::from_le_bytes([content[49], content[50], content[51], content[52]]) as usize;
        let index_sample_count =
            u32::from_le_bytes([content[53], content[54], content[55], content[56]]) as usize;
        let projection_sample_count =
            u32::from_le_bytes([content[57], content[58], content[59], content[60]]) as usize;
        let job_sample_count =
            u32::from_le_bytes([content[61], content[62], content[63], content[64]]) as usize;
        let vector_artifact_sample_count =
            u32::from_le_bytes([content[65], content[66], content[67], content[68]]) as usize;

        let mut pos = 69usize;
        let mut collection_names = Vec::with_capacity(collection_sample_count);
        for _ in 0..collection_sample_count {
            collection_names.push(read_native_string(content, &mut pos)?);
        }

        let mut indexes = Vec::with_capacity(index_sample_count);
        for _ in 0..index_sample_count {
            let name = read_native_string(content, &mut pos)?;
            let kind = read_native_string(content, &mut pos)?;
            let collection = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    Some(read_native_string(content, &mut pos)?)
                }
                Some(_) => {
                    pos += 1;
                    None
                }
                None => None,
            };
            let enabled = content.get(pos).copied().unwrap_or(0) == 1;
            pos = pos.saturating_add(1);
            if pos + 16 > content.len() {
                break;
            }
            let entries = u64::from_le_bytes([
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
            let estimated_memory_bytes = u64::from_le_bytes([
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
            let last_refresh_ms = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    if pos + 16 > content.len() {
                        return Err(StoreError::Serialization(
                            "truncated native registry refresh timestamp".to_string(),
                        ));
                    }
                    let mut bytes = [0u8; 16];
                    bytes.copy_from_slice(&content[pos..pos + 16]);
                    pos += 16;
                    Some(u128::from_le_bytes(bytes))
                }
                Some(_) => {
                    pos += 1;
                    None
                }
                None => None,
            };
            let backend = read_native_string(content, &mut pos)?;
            indexes.push(NativeRegistryIndexSummary {
                name,
                kind,
                collection,
                enabled,
                entries,
                estimated_memory_bytes,
                last_refresh_ms,
                backend,
            });
        }

        let mut graph_projections = Vec::with_capacity(projection_sample_count);
        for _ in 0..projection_sample_count {
            let name = read_native_string(content, &mut pos)?;
            let source = read_native_string(content, &mut pos)?;
            if pos + 32 > content.len() {
                break;
            }
            let mut created_bytes = [0u8; 16];
            created_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let mut updated_bytes = [0u8; 16];
            updated_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let node_labels = read_native_string_list(content, &mut pos)?;
            let node_types = read_native_string_list(content, &mut pos)?;
            let edge_labels = read_native_string_list(content, &mut pos)?;
            let last_materialized_sequence = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    if pos + 8 > content.len() {
                        return Err(StoreError::Serialization(
                            "truncated native projection materialization sequence".to_string(),
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
            graph_projections.push(NativeRegistryProjectionSummary {
                name,
                source,
                created_at_unix_ms: u128::from_le_bytes(created_bytes),
                updated_at_unix_ms: u128::from_le_bytes(updated_bytes),
                node_labels,
                node_types,
                edge_labels,
                last_materialized_sequence,
            });
        }

        let mut analytics_jobs = Vec::with_capacity(job_sample_count);
        for _ in 0..job_sample_count {
            let id = read_native_string(content, &mut pos)?;
            let kind = read_native_string(content, &mut pos)?;
            let projection = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    Some(read_native_string(content, &mut pos)?)
                }
                Some(_) => {
                    pos += 1;
                    None
                }
                None => None,
            };
            let state = read_native_string(content, &mut pos)?;
            if pos + 32 > content.len() {
                break;
            }
            let mut created_bytes = [0u8; 16];
            created_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let mut updated_bytes = [0u8; 16];
            updated_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let last_run_sequence = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    if pos + 8 > content.len() {
                        return Err(StoreError::Serialization(
                            "truncated native analytics job run sequence".to_string(),
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
            if pos + 2 > content.len() {
                return Err(StoreError::Serialization(
                    "truncated native analytics job metadata count".to_string(),
                ));
            }
            let metadata_count = u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
            pos += 2;
            let mut metadata = BTreeMap::new();
            for _ in 0..metadata_count {
                let key = read_native_string(content, &mut pos)?;
                let value = read_native_string(content, &mut pos)?;
                metadata.insert(key, value);
            }
            analytics_jobs.push(NativeRegistryJobSummary {
                id,
                kind,
                projection,
                state,
                created_at_unix_ms: u128::from_le_bytes(created_bytes),
                updated_at_unix_ms: u128::from_le_bytes(updated_bytes),
                last_run_sequence,
                metadata,
            });
        }
        let mut vector_artifacts = Vec::with_capacity(vector_artifact_sample_count);
        for _ in 0..vector_artifact_sample_count {
            let collection = read_native_string(content, &mut pos)?;
            let artifact_kind = read_native_string(content, &mut pos)?;
            if pos + 32 > content.len() {
                break;
            }
            let vector_count = u64::from_le_bytes([
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
            let dimension = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let max_layer = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let serialized_bytes = u64::from_le_bytes([
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
            vector_artifacts.push(NativeVectorArtifactSummary {
                collection,
                artifact_kind,
                vector_count,
                dimension,
                max_layer,
                serialized_bytes,
                checksum,
            });
        }

        Ok(NativeRegistrySummary {
            collection_count,
            index_count,
            graph_projection_count,
            analytics_job_count,
            vector_artifact_count,
            collections_complete,
            indexes_complete,
            graph_projections_complete,
            analytics_jobs_complete,
            vector_artifacts_complete,
            omitted_collection_count,
            omitted_index_count,
            omitted_graph_projection_count,
            omitted_analytics_job_count,
            omitted_vector_artifact_count,
            collection_names,
            indexes,
            graph_projections,
            analytics_jobs,
            vector_artifacts,
        })
    }

    /// Persist a compact native snapshot/export summary into a dedicated page.
    pub fn write_native_recovery_summary(
        &self,
        summary: &NativeRecoverySummary,
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
        data.extend_from_slice(NATIVE_RECOVERY_MAGIC);
        data.extend_from_slice(&summary.snapshot_count.to_le_bytes());
        data.extend_from_slice(&summary.export_count.to_le_bytes());
        data.push(u8::from(summary.snapshots_complete));
        data.push(u8::from(summary.exports_complete));
        data.extend_from_slice(&summary.omitted_snapshot_count.to_le_bytes());
        data.extend_from_slice(&summary.omitted_export_count.to_le_bytes());
        data.extend_from_slice(&(summary.snapshots.len() as u32).to_le_bytes());
        data.extend_from_slice(&(summary.exports.len() as u32).to_le_bytes());

        for snapshot in &summary.snapshots {
            data.extend_from_slice(&snapshot.snapshot_id.to_le_bytes());
            data.extend_from_slice(&snapshot.created_at_unix_ms.to_le_bytes());
            data.extend_from_slice(&snapshot.superblock_sequence.to_le_bytes());
            data.extend_from_slice(&snapshot.collection_count.to_le_bytes());
            data.extend_from_slice(&snapshot.total_entities.to_le_bytes());
        }

        for export in &summary.exports {
            push_native_string(&mut data, &export.name);
            data.extend_from_slice(&export.created_at_unix_ms.to_le_bytes());
            match export.snapshot_id {
                Some(snapshot_id) => {
                    data.push(1);
                    data.extend_from_slice(&snapshot_id.to_le_bytes());
                }
                None => data.push(0),
            }
            data.extend_from_slice(&export.superblock_sequence.to_le_bytes());
            data.extend_from_slice(&export.collection_count.to_le_bytes());
            data.extend_from_slice(&export.total_entities.to_le_bytes());
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

    /// Read a compact native snapshot/export summary from a dedicated page.
    pub fn read_native_recovery_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeRecoverySummary, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native recovery summary requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(
                "native recovery summary page is not set".to_string(),
            ));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 30 || &content[0..4] != NATIVE_RECOVERY_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native recovery summary page".to_string(),
            ));
        }

        let snapshot_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
        let export_count = u32::from_le_bytes([content[8], content[9], content[10], content[11]]);
        let snapshots_complete = content[12] == 1;
        let exports_complete = content[13] == 1;
        let omitted_snapshot_count =
            u32::from_le_bytes([content[14], content[15], content[16], content[17]]);
        let omitted_export_count =
            u32::from_le_bytes([content[18], content[19], content[20], content[21]]);
        let snapshot_sample_count =
            u32::from_le_bytes([content[22], content[23], content[24], content[25]]) as usize;
        let export_sample_count =
            u32::from_le_bytes([content[26], content[27], content[28], content[29]]) as usize;

        let mut pos = 30usize;
        let mut snapshots = Vec::with_capacity(snapshot_sample_count);
        for _ in 0..snapshot_sample_count {
            if pos + 44 > content.len() {
                break;
            }
            let snapshot_id = u64::from_le_bytes([
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
            let mut created_bytes = [0u8; 16];
            created_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let superblock_sequence = u64::from_le_bytes([
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
            let collection_count = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let total_entities = u64::from_le_bytes([
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
            snapshots.push(NativeSnapshotSummary {
                snapshot_id,
                created_at_unix_ms: u128::from_le_bytes(created_bytes),
                superblock_sequence,
                collection_count,
                total_entities,
            });
        }

        let mut exports = Vec::with_capacity(export_sample_count);
        for _ in 0..export_sample_count {
            let name = read_native_string(content, &mut pos)?;
            if pos + 16 > content.len() {
                break;
            }
            let mut created_bytes = [0u8; 16];
            created_bytes.copy_from_slice(&content[pos..pos + 16]);
            pos += 16;
            let snapshot_id = match content.get(pos).copied() {
                Some(1) => {
                    pos += 1;
                    if pos + 8 > content.len() {
                        return Err(StoreError::Serialization(
                            "truncated native export snapshot id".to_string(),
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
            if pos + 20 > content.len() {
                break;
            }
            let superblock_sequence = u64::from_le_bytes([
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
            let collection_count = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            let total_entities = u64::from_le_bytes([
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
            exports.push(NativeExportSummary {
                name,
                created_at_unix_ms: u128::from_le_bytes(created_bytes),
                snapshot_id,
                superblock_sequence,
                collection_count,
                total_entities,
            });
        }

        Ok(NativeRecoverySummary {
            snapshot_count,
            export_count,
            snapshots_complete,
            exports_complete,
            omitted_snapshot_count,
            omitted_export_count,
            snapshots,
            exports,
        })
    }

    /// Persist a compact native catalog summary into a dedicated page.
    pub fn write_native_catalog_summary(
        &self,
        summary: &NativeCatalogSummary,
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
        data.extend_from_slice(NATIVE_CATALOG_MAGIC);
        data.extend_from_slice(&summary.collection_count.to_le_bytes());
        data.extend_from_slice(&summary.total_entities.to_le_bytes());
        data.push(u8::from(summary.collections_complete));
        data.extend_from_slice(&summary.omitted_collection_count.to_le_bytes());
        data.extend_from_slice(&(summary.collections.len() as u32).to_le_bytes());
        for collection in &summary.collections {
            push_native_string(&mut data, &collection.name);
            data.extend_from_slice(&collection.entities.to_le_bytes());
            data.extend_from_slice(&collection.cross_refs.to_le_bytes());
            data.extend_from_slice(&collection.segments.to_le_bytes());
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

    /// Read a compact native catalog summary from a dedicated page.
    pub fn read_native_catalog_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeCatalogSummary, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native catalog summary requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(
                "native catalog summary page is not set".to_string(),
            ));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 25 || &content[0..4] != NATIVE_CATALOG_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native catalog summary page".to_string(),
            ));
        }

        let collection_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
        let total_entities = u64::from_le_bytes([
            content[8],
            content[9],
            content[10],
            content[11],
            content[12],
            content[13],
            content[14],
            content[15],
        ]);
        let collections_complete = content[16] == 1;
        let omitted_collection_count =
            u32::from_le_bytes([content[17], content[18], content[19], content[20]]);
        let sample_count =
            u32::from_le_bytes([content[21], content[22], content[23], content[24]]) as usize;

        let mut pos = 25usize;
        let mut collections = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            let name = read_native_string(content, &mut pos)?;
            if pos + 20 > content.len() {
                break;
            }
            let entities = u64::from_le_bytes([
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
            let cross_refs = u64::from_le_bytes([
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
            let segments = u32::from_le_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            collections.push(NativeCatalogCollectionSummary {
                name,
                entities,
                cross_refs,
                segments,
            });
        }

        Ok(NativeCatalogSummary {
            collection_count,
            total_entities,
            collections_complete,
            omitted_collection_count,
            collections,
        })
    }

    /// Persist a compact native metadata state summary into a dedicated page.
    pub fn write_native_metadata_state_summary(
        &self,
        summary: &NativeMetadataStateSummary,
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

        let mut data = Vec::with_capacity(512);
        data.extend_from_slice(NATIVE_METADATA_STATE_MAGIC);
        push_native_string(&mut data, &summary.protocol_version);
        data.extend_from_slice(&summary.generated_at_unix_ms.to_le_bytes());
        match &summary.last_loaded_from {
            Some(value) => {
                data.push(1);
                push_native_string(&mut data, value);
            }
            None => data.push(0),
        }
        match summary.last_healed_at_unix_ms {
            Some(value) => {
                data.push(1);
                data.extend_from_slice(&value.to_le_bytes());
            }
            None => data.push(0),
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

    /// Read a compact native metadata state summary from a dedicated page.
    pub fn read_native_metadata_state_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeMetadataStateSummary, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(
                "native metadata state summary requires paged mode".to_string(),
            ));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(
                "native metadata state page is not set".to_string(),
            ));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        let bytes = page.as_bytes();
        let content = &bytes[crate::storage::engine::HEADER_SIZE..];
        if content.len() < 22 || &content[0..4] != NATIVE_METADATA_STATE_MAGIC {
            return Err(StoreError::Serialization(
                "invalid native metadata state page".to_string(),
            ));
        }

        let mut pos = 4usize;
        let protocol_version = read_native_string(content, &mut pos)?;
        if pos + 16 > content.len() {
            return Err(StoreError::Serialization(
                "truncated native metadata state timestamp".to_string(),
            ));
        }
        let mut generated_bytes = [0u8; 16];
        generated_bytes.copy_from_slice(&content[pos..pos + 16]);
        pos += 16;
        let last_loaded_from = match content.get(pos).copied() {
            Some(1) => {
                pos += 1;
                Some(read_native_string(content, &mut pos)?)
            }
            Some(_) => {
                pos += 1;
                None
            }
            None => None,
        };
        let last_healed_at_unix_ms = match content.get(pos).copied() {
            Some(1) => {
                pos += 1;
                if pos + 16 > content.len() {
                    return Err(StoreError::Serialization(
                        "truncated native metadata heal timestamp".to_string(),
                    ));
                }
                let mut healed_bytes = [0u8; 16];
                healed_bytes.copy_from_slice(&content[pos..pos + 16]);
                Some(u128::from_le_bytes(healed_bytes))
            }
            Some(_) => None,
            None => None,
        };

        Ok(NativeMetadataStateSummary {
            protocol_version,
            generated_at_unix_ms: u128::from_le_bytes(generated_bytes),
            last_loaded_from,
            last_healed_at_unix_ms,
        })
    }
}
