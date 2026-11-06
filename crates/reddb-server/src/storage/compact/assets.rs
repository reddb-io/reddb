use super::*;
use crate::serde_json;

#[derive(Clone, Copy)]
pub(super) struct Blob {
    pub root: u32,
    pub pages: u32,
    pub bytes: u64,
    pub checksum: u64,
}
pub(super) struct Assets<'a> {
    pub source: &'a Source,
    pub target: Option<&'a Pager>,
    blobs: BTreeMap<u32, Blob>,
    verification: Vec<(u32, blake3::Hash, bool)>,
}
impl<'a> Assets<'a> {
    pub fn new(source: &'a Source, target: &'a Pager) -> Self {
        Self {
            source,
            target: Some(target),
            blobs: BTreeMap::new(),
            verification: Vec::new(),
        }
    }
    pub fn for_verification(source: &'a Source) -> Self {
        Self {
            source,
            target: None,
            blobs: BTreeMap::new(),
            verification: Vec::new(),
        }
    }
    fn remember(&mut self, page: &Page, checksum: bool) {
        self.verification
            .push((page.page_id(), blake3::hash(page.content()), checksum));
    }
    pub fn into_verification(self) -> Vec<(u32, blake3::Hash, bool)> {
        self.verification
    }
    pub fn blob(&mut self, root: u32) -> Result<Blob> {
        if let Some(blob) = self.blobs.get(&root) {
            return Ok(*blob);
        }
        ensure(root != 0, "native blob has zero root")?;
        let mut next = root;
        let mut pages = Vec::new();
        let mut bytes = 0u64;
        let mut checksum = crc32fast::Hasher::new();
        while next != 0 {
            self.source.claim(next)?;
            let page = self.source.raw_page(next, true)?;
            ensure(
                page.page_type()? == PageType::NativeMeta,
                "invalid native blob page type",
            )?;
            let (following, chunk) = f::decode_native_blob_page(page.content())?;
            checksum.update(&chunk);
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .ok_or("blob size overflow")?;
            let destination = match self.target {
                Some(target) => target.allocate_page(PageType::NativeMeta)?.page_id(),
                None => next,
            };
            pages.push((next, destination));
            next = following;
        }
        if let Some(target) = self.target {
            for (index, &(old, new)) in pages.iter().enumerate() {
                let original = self.source.raw_page(old, true)?;
                let (_, chunk) = f::decode_native_blob_page(original.content())?;
                let next = pages.get(index + 1).map_or(0, |(_, id)| *id);
                let data = f::encode_native_blob_page(next, &chunk);
                let mut page = Page::new(PageType::NativeMeta, new);
                page.content_mut()[..data.len()].copy_from_slice(&data);
                self.remember(&page, true);
                target.write_page(new, page)?;
            }
        }
        let blob = Blob {
            root: pages[0].1,
            pages: u32::try_from(pages.len())?,
            bytes,
            checksum: u64::from(checksum.finalize()),
        };
        self.blobs.insert(root, blob);
        Ok(blob)
    }
    pub fn vault(&mut self) -> Result<()> {
        let header = self.source.raw_page(2, false)?;
        if header.as_bytes().iter().all(|byte| *byte == 0) {
            return Ok(());
        }
        if header.page_type()? == PageType::Vault && header.content().iter().all(|byte| *byte == 0)
        {
            header.verify_checksum()?;
            return Ok(());
        }
        ensure(
            header.page_type()? == PageType::Vault
                && &header.content()[..4] == b"RDVT"
                && matches!(header.content()[4], 2 | 3),
            "unsupported vault header",
        )?;
        let count = number(header.content(), 37)?;
        let mut next = number(header.content(), 41)?;
        let payload = number(header.content(), 21)? as usize;
        ensure(payload >= 12, "invalid vault payload length")?;
        let cipher_bytes = payload - 12;
        let first_capacity = PAGE_SIZE - HEADER_SIZE - 45;
        let continuation_capacity = PAGE_SIZE - HEADER_SIZE - 8;
        ensure(
            cipher_bytes
                .saturating_sub(first_capacity)
                .div_ceil(continuation_capacity)
                == count as usize,
            "vault length and chain count disagree",
        )?;
        let mut pages = Vec::new();
        while next != 0 {
            ensure(
                pages.len() < count as usize,
                "vault chain exceeds declared length",
            )?;
            self.source.claim(next)?;
            let page = self.source.raw_page(next, false)?;
            ensure(
                page.page_type()? == PageType::Vault && &page.content()[..4] == b"RDVD",
                "invalid vault continuation",
            )?;
            let destination = match self.target {
                Some(target) => target.allocate_page(PageType::Vault)?.page_id(),
                None => next,
            };
            pages.push((next, destination));
            next = number(page.content(), 4)?;
        }
        ensure(pages.len() == count as usize, "incomplete vault chain")?;
        if self.target.is_none() {
            return Ok(());
        }
        for (index, &(old, new)) in pages.iter().enumerate() {
            let mut page = relocate(self.source.raw_page(old, false)?, new);
            page.content_mut()[4..8]
                .copy_from_slice(&pages.get(index + 1).map_or(0, |(_, id)| *id).to_le_bytes());
            self.remember(&page, false);
            self.target
                .expect("rewrite target")
                .write_page_no_checksum(new, page)?;
        }
        let mut header = header;
        header.content_mut()[41..45]
            .copy_from_slice(&pages.first().map_or(0, |(_, id)| *id).to_le_bytes());
        self.remember(&header, false);
        self.target
            .expect("rewrite target")
            .write_page_no_checksum(2, header)?;
        Ok(())
    }
    pub fn physical(&mut self) -> Result<f::PhysicalFileHeader> {
        let mut header = self.source.header.physical;
        for (id, magic) in [
            (&mut header.manifest_page, b"RDMF"),
            (&mut header.collection_roots_page, b"RDRT"),
            (&mut header.registry_page, b"RDRG"),
            (&mut header.recovery_page, b"RDRV"),
            (&mut header.catalog_page, b"RDCL"),
            (&mut header.metadata_state_page, b"RDMS"),
        ] {
            if *id == 0 {
                continue;
            }
            self.source.claim(*id)?;
            let page = self.source.raw_page(*id, true)?;
            ensure(
                page.page_type()? == PageType::NativeMeta,
                "invalid physical summary page type",
            )?;
            ensure(
                &page.content()[..4] == magic,
                "physical header names the wrong summary",
            )?;
            match &page.content()[..4] {
                b"RDMF" => {
                    f::decode_native_manifest_summary_page(page.content())?;
                }
                b"RDRT" => {
                    f::decode_native_collection_roots_page(page.content())?;
                }
                b"RDRG" => {
                    f::decode_native_registry_summary_page(page.content())?;
                }
                b"RDRV" => {
                    f::decode_native_recovery_summary_page(page.content())?;
                }
                b"RDCL" => {
                    f::decode_native_catalog_summary_page(page.content())?;
                }
                b"RDMS" => {
                    f::decode_native_metadata_state_summary_page(page.content())?;
                }
                _ => return Err("unsupported physical summary".into()),
            }
            if let Some(target) = self.target {
                let new = target.allocate_page(PageType::NativeMeta)?.page_id();
                let page = relocate(page, new);
                self.remember(&page, true);
                target.write_page(new, page)?;
                *id = new;
            }
        }
        if header.vector_artifact_page != 0 {
            self.source.claim(header.vector_artifact_page)?;
            let page = self.source.raw_page(header.vector_artifact_page, true)?;
            ensure(
                page.page_type()? == PageType::NativeMeta,
                "invalid artifact summary page type",
            )?;
            let mut summaries = f::decode_native_vector_artifact_store_page(page.content())?;
            for summary in &mut summaries {
                let blob = self.blob(summary.root_page)?;
                ensure(
                    blob.pages == summary.page_count
                        && blob.bytes == summary.byte_len
                        && blob.checksum == summary.checksum,
                    "native artifact differs from its descriptor",
                )?;
                summary.root_page = blob.root;
            }
            let data = f::encode_native_vector_artifact_store_page(&summaries);
            ensure(
                data.len() <= PAGE_SIZE - HEADER_SIZE,
                "artifact summary exceeds page",
            )?;
            if let Some(target) = self.target {
                let mut page = target.allocate_page(PageType::NativeMeta)?;
                page.content_mut()[..data.len()].copy_from_slice(&data);
                header.vector_artifact_page = page.page_id();
                header.vector_artifact_checksum = f::native_store_page_checksum(&data);
                self.remember(&page, true);
                target.write_page(page.page_id(), page)?;
            }
        }
        Ok(header)
    }
    pub fn metadata(&mut self, source: &Path, destination: Option<&Path>) -> Result<()> {
        let binary = f::physical_metadata_binary_path(source);
        let json = f::physical_metadata_json_path(source);
        let path = if binary.exists() {
            binary
        } else if json.exists() {
            json
        } else {
            ensure(
                self.source.header.physical.index_count == 0
                    && self.source.header.physical.catalog_collection_count == 0,
                "physical metadata sidecar is missing",
            )?;
            return Ok(());
        };
        ensure(
            std::fs::metadata(&path)?.len() <= 64 * 1024 * 1024,
            "physical metadata exceeds compaction budget",
        )?;
        let text = f::physical_metadata::read_physical_metadata_document(&path)?;
        let mut document: serde_json::Value = serde_json::from_str(&text)?;
        // Validate the complete native contract, then edit only physical addresses
        // in the JSON value so unrelated and forward-compatible fields survive.
        let envelope = f::decode_physical_metadata_document_root_json(&text)?;
        for hypertable in envelope.hypertables_json {
            let hypertable = f::decode_persisted_physical_hypertable_json(&hypertable)?;
            ensure(
                hypertable
                    .chunks
                    .iter()
                    .all(|chunk| chunk.columnar_page.is_none()),
                "columnar page owners require a qualified relocation codec",
            )?;
        }
        if let serde_json::Value::Array(indexes) = &mut document["indexes"] {
            ensure(
                indexes.len() == envelope.indexes_json.len(),
                "index descriptor count mismatch",
            )?;
            for (index, encoded) in indexes.iter_mut().zip(&envelope.indexes_json) {
                let descriptor = f::decode_persisted_physical_index_state_json(encoded)?;
                if let Some(root) = descriptor.artifact_root_page {
                    let blob = self.blob(root)?;
                    if let Some(checksum) = descriptor.artifact_checksum {
                        ensure(
                            checksum == blob.checksum,
                            "index artifact checksum mismatch",
                        )?;
                    }
                    index["artifact_root_page"] = serde_json::Value::Integer(i64::from(blob.root));
                }
            }
        }
        let Some(destination) = destination else {
            return Ok(());
        };
        let encoded =
            f::encode_physical_metadata_binary_document(&serde_json::to_string(&document)?)?;
        let output = f::physical_metadata_binary_path(destination);
        write_new(&output, &encoded)?;
        ensure(
            f::physical_metadata::read_physical_metadata_document(&output)?
                == serde_json::to_string(&document)?,
            "physical metadata failed readback parity",
        )?;
        Ok(())
    }
}
fn relocate(mut page: Page, id: u32) -> Page {
    page.as_bytes_mut()[8..12].copy_from_slice(&id.to_le_bytes());
    page.set_lsn(0);
    page
}
