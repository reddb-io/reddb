use super::*;

pub(super) struct Catalog {
    pub header: f::NativePagedMetadataHeader,
    pub roots: Vec<f::NativePagedCollectionRoot>,
    references: Vec<f::NativePagedCrossRef>,
    epoch: Option<u64>,
    retired_roots: Vec<u32>,
    deferred_pages: Vec<u32>,
    pub resume: Option<f::native_resume::CatalogResume>,
}
impl Catalog {
    pub fn has_cross_references(&self) -> bool {
        !self.references.is_empty()
    }
    pub fn read(source: &Source) -> Result<Self> {
        let page = source.raw_page(1, true)?;
        ensure(
            page.page_type()? == PageType::Header,
            "invalid catalog page",
        )?;
        let mut data = page.content().to_vec();
        if let Some(header) = f::decode_native_metadata_overflow_header(&data)? {
            let total = header.total_payload_bytes as usize;
            ensure(
                total > 0 && total <= 64 * 1024 * 1024,
                "catalog exceeds compaction memory budget",
            )?;
            let first = total.min(data.len() - 16);
            let mut payload = data[16..16 + first].to_vec();
            let mut next = header.next_overflow_page_id;
            while next != 0 {
                source.claim(next)?;
                let page = source.raw_page(next, true)?;
                ensure(
                    page.page_type()? == PageType::Overflow,
                    "invalid catalog continuation",
                )?;
                let continuation =
                    f::decode_native_metadata_overflow_continuation_header(page.content())?;
                let length = continuation.chunk_bytes as usize;
                ensure(
                    length > 0
                        && length <= PAGE_SIZE - HEADER_SIZE - 8
                        && payload.len() + length <= total,
                    "invalid catalog continuation length",
                )?;
                payload.extend_from_slice(&page.content()[8..8 + length]);
                next = continuation.next_overflow_page_id;
            }
            ensure(payload.len() == total, "incomplete catalog")?;
            data = payload;
        }
        let header = f::decode_native_paged_metadata_header(&data)?
            .ok_or("source is not a native paged catalog")?;
        ensure(
            matches!(header.format_version, 11 | 12),
            "unsupported native record format",
        )?;
        let mut position = f::METADATA_HEADER_BYTES;
        ensure(
            header.collection_count as usize <= data.len() / 8,
            "invalid catalog count",
        )?;
        let mut roots = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for _ in 0..header.collection_count {
            let root = f::decode_native_paged_collection_root(&data, &mut position)?;
            ensure(
                names.insert(root.collection.clone()),
                "duplicate collection",
            )?;
            roots.push(root);
        }
        let count = number(&data, position)?;
        position += 4;
        ensure(
            count as usize <= data.len().saturating_sub(position) / 21,
            "invalid reference count",
        )?;
        let mut references = Vec::new();
        for _ in 0..count {
            references.push(f::decode_native_paged_cross_ref(&data, &mut position)?);
        }
        // Offline ownership has no reader pins. Retired generations and deferred
        // frees are physical garbage, not MVCC versions in the current trees.
        let retired_roots = f::decode_native_paged_retired_trees(&data, &mut position)?;
        let deferred_pages = f::decode_native_paged_deferred_pages(&data, &mut position)?;
        let epoch = f::native_store::decode_native_paged_ownership_epoch(&data, &mut position)?;
        let resume = f::native_resume::decode_catalog_resume(&data)?;
        if let Some(resume) = &resume {
            let mut canonical = Vec::new();
            f::native_resume::encode_catalog_resume(&mut canonical, resume)?;
            ensure(
                data.get(position..position + canonical.len()) == Some(canonical.as_slice()),
                "unsupported/noncanonical resume trailer",
            )?;
            position += canonical.len();
        }
        ensure(
            data[position..].iter().all(|byte| *byte == 0),
            "unknown catalog extension",
        )?;
        Ok(Self {
            header,
            roots,
            references,
            epoch,
            retired_roots,
            deferred_pages,
            resume,
        })
    }

    /// Offline readers have no pins, but retired pages still need an explicit
    /// ownership proof. Never treat arbitrary unclassified pages as garbage.
    pub fn claim_retired(&self, source: &Arc<Source>) -> Result<()> {
        for root in &self.retired_roots {
            let tree = BTree::with_root(Arc::clone(source), *root);
            for page in tree.retired_pages()? {
                source.claim(page)?;
            }
        }
        for page in &self.deferred_pages {
            source.claim(*page)?;
        }
        Ok(())
    }

    pub(super) fn verify_alias_has_no_references(&self, legacy: &str) -> Result<()> {
        ensure(
            self.references
                .iter()
                .all(|r| r.target_collection != legacy),
            "legacy runtime metadata has incoming references",
        )
    }

    pub fn retain_canonical_system_config(&mut self) -> Result<()> {
        let legacy_roots = self
            .roots
            .iter()
            .filter(|root| root.collection == "red_config")
            .count();
        let canonical_roots = self
            .roots
            .iter()
            .filter(|root| root.collection == "red.config")
            .count();
        ensure(
            legacy_roots == 1 && canonical_roots == 1,
            "canonical config repair requires exactly one red_config and one red.config root",
        )?;
        ensure(
            self.references
                .iter()
                .all(|reference| reference.target_collection != "red_config"),
            "legacy red_config has cross-references and cannot be retired",
        )?;
        let resume = self
            .resume
            .as_mut()
            .ok_or("canonical config repair requires a resume catalog")?;
        ensure(
            resume.collections.contains_key("red_config")
                && resume.collections.contains_key("red.config"),
            "canonical config repair requires both resume identities",
        )?;
        resume.collections.remove("red_config");
        self.roots.retain(|root| root.collection != "red_config");
        self.header.collection_count = u32::try_from(self.roots.len())?;
        Ok(())
    }

    pub fn encode(&self, roots: &BTreeMap<u32, u32>) -> Result<Vec<u8>> {
        let remap = |root: u32| -> Result<u32> {
            if root == 0 {
                Ok(0)
            } else {
                roots
                    .get(&root)
                    .copied()
                    .ok_or_else(|| "missing rewritten tree".into())
            }
        };
        let mut data = Vec::new();
        f::encode_native_paged_metadata_header(&mut data, self.header);
        for root in &self.roots {
            f::encode_native_paged_collection_root(
                &mut data,
                &root.collection,
                remap(root.root_page)?,
            );
        }
        data.extend_from_slice(&u32::try_from(self.references.len())?.to_le_bytes());
        for reference in &self.references {
            f::encode_native_paged_cross_ref(
                &mut data,
                reference.source_id,
                reference.target_id,
                reference.ref_type,
                &reference.target_collection,
            );
        }
        f::encode_native_paged_retired_trees(&mut data, &[])?;
        f::encode_native_paged_deferred_pages(&mut data, &[])?;
        if let Some(epoch) = self.epoch {
            f::native_store::encode_native_paged_ownership_epoch(&mut data, epoch)?;
        }
        if let Some(mut resume) = self.resume.clone() {
            for state in resume.collections.values_mut() {
                state.root_page = remap(state.root_page)?;
                state.key_index_root = state.key_index_root.map(remap).transpose()?;
                for index in state.secondary_indices.values_mut() {
                    index.root_page = remap(index.root_page)?;
                }
            }
            f::native_resume::encode_catalog_resume(&mut data, &resume)?;
        }
        Ok(data)
    }
}

pub(super) fn write_catalog(target: &Pager, data: &[u8]) -> Result<()> {
    let mut root = Page::new(PageType::Header, 1);
    if data.len() <= PAGE_SIZE - HEADER_SIZE {
        root.content_mut()[..data.len()].copy_from_slice(data);
    } else {
        let first = PAGE_SIZE - HEADER_SIZE - 16;
        let chunks: Vec<_> = data[first..].chunks(PAGE_SIZE - HEADER_SIZE - 8).collect();
        let mut next = 0;
        for chunk in chunks.into_iter().rev() {
            let mut page = target.allocate_page(PageType::Overflow)?;
            f::encode_native_metadata_overflow_continuation_header(
                page.content_mut(),
                f::NativeMetadataOverflowContinuationHeader {
                    next_overflow_page_id: next,
                    chunk_bytes: u32::try_from(chunk.len())?,
                },
            )?;
            page.content_mut()[8..8 + chunk.len()].copy_from_slice(chunk);
            next = page.page_id();
            target.write_page(next, page)?;
        }
        f::encode_native_metadata_overflow_header(
            root.content_mut(),
            f::NativeMetadataOverflowHeader {
                format_version: number(data, 4)?,
                total_payload_bytes: u32::try_from(data.len())?,
                next_overflow_page_id: next,
            },
        )?;
        root.content_mut()[16..16 + first].copy_from_slice(&data[..first]);
    }
    target.sync()?;
    target.write_page(1, root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resume(root_page: u32) -> f::native_resume::CollectionResume {
        f::native_resume::CollectionResume {
            root_page,
            entities: 1,
            entity_id_next: 2,
            row_id_next: 2,
            sequence_id_next: 2,
            key_index_root: None,
            secondary_indices: BTreeMap::new(),
        }
    }

    fn duplicate_config_catalog() -> Catalog {
        Catalog {
            header: f::NativePagedMetadataHeader {
                format_version: 11,
                collection_count: 3,
            },
            roots: vec![
                f::NativePagedCollectionRoot {
                    collection: "users".into(),
                    root_page: 10,
                },
                f::NativePagedCollectionRoot {
                    collection: "red_config".into(),
                    root_page: 20,
                },
                f::NativePagedCollectionRoot {
                    collection: "red.config".into(),
                    root_page: 30,
                },
            ],
            references: Vec::new(),
            epoch: Some(7),
            retired_roots: Vec::new(),
            deferred_pages: Vec::new(),
            resume: Some(f::native_resume::CatalogResume {
                entity_id_next: 100,
                xid_high_water_mark: Some(8),
                collections: [
                    ("users".into(), resume(10)),
                    ("red_config".into(), resume(20)),
                    ("red.config".into(), resume(30)),
                ]
                .into_iter()
                .collect(),
            }),
        }
    }

    #[test]
    fn explicit_repair_retires_only_legacy_config_identity() {
        let mut catalog = duplicate_config_catalog();
        catalog.retain_canonical_system_config().unwrap();
        assert_eq!(catalog.header.collection_count, 2);
        assert_eq!(
            catalog
                .roots
                .iter()
                .map(|root| root.collection.as_str())
                .collect::<Vec<_>>(),
            ["users", "red.config"]
        );
        let resume = catalog.resume.unwrap();
        assert!(!resume.collections.contains_key("red_config"));
        assert_eq!(resume.collections["red.config"].root_page, 30);
        assert_eq!(resume.collections["users"].root_page, 10);
    }

    #[test]
    fn explicit_repair_rejects_legacy_cross_references() {
        let mut catalog = duplicate_config_catalog();
        catalog.references.push(f::NativePagedCrossRef {
            source_id: 1,
            target_id: 2,
            ref_type: 0,
            target_collection: "red_config".into(),
        });
        assert!(catalog
            .retain_canonical_system_config()
            .unwrap_err()
            .to_string()
            .contains("cross-references"));
    }
}
