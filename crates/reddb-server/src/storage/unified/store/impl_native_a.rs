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
        self.pager
            .as_ref()
            .and_then(|pager| pager.physical_header().ok())
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

        let data = reddb_file::encode_native_collection_roots_page(roots);
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
        reddb_file::decode_native_collection_roots_page(content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
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

        let data = reddb_file::encode_native_manifest_summary_page(sequence, events);
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
        reddb_file::decode_native_manifest_summary_page(content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
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

        let data = reddb_file::encode_native_registry_summary_page(summary);
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
        Ok((page_id, checksum))
    }
}
