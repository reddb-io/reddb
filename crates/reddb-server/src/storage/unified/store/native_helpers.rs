use super::*;

impl UnifiedStore {
    pub(super) fn write_native_meta_page(
        &self,
        existing_page: Option<u32>,
        data: &[u8],
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

        let checksum = reddb_file::native_store_page_checksum(data);
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

    pub(super) fn read_native_meta_page(
        &self,
        page_id: u32,
        label: &str,
    ) -> Result<Vec<u8>, StoreError> {
        let Some(pager) = &self.pager else {
            return Err(StoreError::Serialization(format!(
                "{label} requires paged mode"
            )));
        };
        if page_id == 0 {
            return Err(StoreError::Serialization(format!(
                "{label} page is not set"
            )));
        }

        let page = pager
            .read_page(page_id)
            .map_err(|err| StoreError::Serialization(err.to_string()))?;
        Ok(page.as_bytes()[crate::storage::engine::HEADER_SIZE..].to_vec())
    }
}
