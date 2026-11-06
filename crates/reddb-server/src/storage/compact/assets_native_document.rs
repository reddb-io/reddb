use super::*;

impl Assets<'_> {
    pub(super) fn native_document(&mut self) -> Result<Option<String>> {
        use f::native_store::decode_native_metadata_document_ref;
        let page_id = self.source.header.physical.metadata_state_page;
        if page_id == 0 {
            return Ok(None);
        }
        let page = self.source.raw_page(page_id, true)?;
        let Some(reference) = decode_native_metadata_document_ref(page.content())? else {
            return Ok(None);
        };
        let mut next = reference.root_page;
        let mut bytes = Vec::new();
        while next != 0 {
            self.source.claim(next)?;
            let page = self.source.raw_page(next, true)?;
            ensure(
                page.page_type()? == PageType::NativeMeta,
                "metadata blob has invalid page type",
            )?;
            let (following, chunk) = f::decode_native_blob_page(page.content())?;
            ensure(
                !chunk.is_empty() && bytes.len() as u64 + chunk.len() as u64 <= reference.byte_len,
                "metadata blob exceeds declared length",
            )?;
            bytes.extend_from_slice(&chunk);
            next = following;
        }
        ensure(
            bytes.len() as u64 == reference.byte_len
                && f::native_store_page_checksum(&bytes) == reference.checksum,
            "metadata blob length or checksum mismatch",
        )?;
        Ok(Some(String::from_utf8(bytes)?))
    }

    pub(super) fn publish_native_document(
        &mut self,
        physical: &mut f::PhysicalFileHeader,
        text: &str,
    ) -> Result<()> {
        use f::native_store::{
            encode_native_metadata_document_page, NativeMetadataDocumentRef,
            NATIVE_METADATA_DOCUMENT_MAX_BYTES,
        };
        ensure(
            !text.is_empty() && text.len() as u64 <= NATIVE_METADATA_DOCUMENT_MAX_BYTES,
            "physical metadata exceeds compaction budget",
        )?;
        let target = self
            .target
            .ok_or("metadata publication requires a rewrite target")?;
        let envelope = f::decode_physical_metadata_document_root_json(text)?;
        let chunk_size = f::native_blob_chunk_capacity(PAGE_SIZE, HEADER_SIZE);
        let pages = (0..text.len().div_ceil(chunk_size))
            .map(|_| {
                target
                    .allocate_page(PageType::NativeMeta)
                    .map(|p| p.page_id())
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (index, chunk) in text.as_bytes().chunks(chunk_size).enumerate() {
            let mut page = Page::new(PageType::NativeMeta, pages[index]);
            let data =
                f::encode_native_blob_page(pages.get(index + 1).copied().unwrap_or(0), chunk);
            page.content_mut()[..data.len()].copy_from_slice(&data);
            self.remember(&page, true);
            target.write_page(page.page_id(), page)?;
        }
        let summary = f::NativeMetadataStateSummary {
            protocol_version: envelope.protocol_version,
            generated_at_unix_ms: envelope.generated_at_unix_ms,
            last_loaded_from: envelope.last_loaded_from,
            last_healed_at_unix_ms: envelope.last_healed_at_unix_ms,
        };
        let encoded = encode_native_metadata_document_page(
            &summary,
            NativeMetadataDocumentRef {
                root_page: pages[0],
                byte_len: text.len() as u64,
                checksum: f::native_store_page_checksum(text.as_bytes()),
            },
        )?;
        let id = if physical.metadata_state_page != 0 {
            physical.metadata_state_page
        } else {
            target.allocate_page(PageType::NativeMeta)?.page_id()
        };
        let mut page = Page::new(PageType::NativeMeta, id);
        ensure(
            encoded.len() <= page.content().len(),
            "metadata descriptor exceeds page",
        )?;
        page.content_mut()[..encoded.len()].copy_from_slice(&encoded);
        self.verification.retain(|(page_id, _, _)| *page_id != id);
        self.remember(&page, true);
        target.write_page(id, page)?;
        physical.metadata_state_page = id;
        physical.metadata_state_checksum = f::native_store_page_checksum(&encoded);
        Ok(())
    }
}

impl Assets<'_> {
    pub(in crate::storage::compact) fn publish_collection_roots(
        &mut self,
        physical: &mut f::PhysicalFileHeader,
        roots: &BTreeMap<String, u64>,
    ) -> Result<()> {
        if physical.collection_roots_page == 0 {
            return Ok(());
        }
        let target = self
            .target
            .ok_or("collection root publication requires a target")?;
        let encoded = f::encode_native_collection_roots_page(roots);
        ensure(
            encoded.len() <= PAGE_SIZE - HEADER_SIZE,
            "native collection root summary exceeds page",
        )?;
        let id = physical.collection_roots_page;
        let mut page = Page::new(PageType::NativeMeta, id);
        page.content_mut()[..encoded.len()].copy_from_slice(&encoded);
        self.verification.retain(|(page_id, _, _)| *page_id != id);
        self.remember(&page, true);
        target.write_page(id, page)?;
        physical.collection_roots_checksum = f::native_store_page_checksum(&encoded);
        physical.collection_root_count = u32::try_from(roots.len())?;
        Ok(())
    }
}
