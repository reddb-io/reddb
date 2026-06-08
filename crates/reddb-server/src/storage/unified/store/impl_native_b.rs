use super::*;

impl UnifiedStore {
    pub fn read_native_registry_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeRegistrySummary, StoreError> {
        let content = self.read_native_meta_page(page_id, "native registry summary")?;
        reddb_file::decode_native_registry_summary_page(&content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    /// Persist a compact native snapshot/export summary into a dedicated page.
    pub fn write_native_recovery_summary(
        &self,
        summary: &NativeRecoverySummary,
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let data = reddb_file::encode_native_recovery_summary_page(summary);
        self.write_native_meta_page(existing_page, &data)
    }

    /// Read a compact native snapshot/export summary from a dedicated page.
    pub fn read_native_recovery_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeRecoverySummary, StoreError> {
        let content = self.read_native_meta_page(page_id, "native recovery summary")?;
        reddb_file::decode_native_recovery_summary_page(&content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    /// Persist a compact native catalog summary into a dedicated page.
    pub fn write_native_catalog_summary(
        &self,
        summary: &NativeCatalogSummary,
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let data = reddb_file::encode_native_catalog_summary_page(summary);
        self.write_native_meta_page(existing_page, &data)
    }

    /// Read a compact native catalog summary from a dedicated page.
    pub fn read_native_catalog_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeCatalogSummary, StoreError> {
        let content = self.read_native_meta_page(page_id, "native catalog summary")?;
        reddb_file::decode_native_catalog_summary_page(&content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }

    /// Persist a compact native metadata state summary into a dedicated page.
    pub fn write_native_metadata_state_summary(
        &self,
        summary: &NativeMetadataStateSummary,
        existing_page: Option<u32>,
    ) -> Result<(u32, u64), StoreError> {
        let data = reddb_file::encode_native_metadata_state_summary_page(summary);
        self.write_native_meta_page(existing_page, &data)
    }

    /// Read a compact native metadata state summary from a dedicated page.
    pub fn read_native_metadata_state_summary(
        &self,
        page_id: u32,
    ) -> Result<NativeMetadataStateSummary, StoreError> {
        let content = self.read_native_meta_page(page_id, "native metadata state summary")?;
        reddb_file::decode_native_metadata_state_summary_page(&content)
            .map_err(|err| StoreError::Serialization(err.to_string()))
    }
}
