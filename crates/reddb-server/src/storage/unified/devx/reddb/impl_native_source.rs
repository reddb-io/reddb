use super::*;
use reddb_file::native_artifact_source::NATIVE_ARTIFACT_SOURCE_VERSION;

impl RedDB {
    /// Hash the raw source, including versions omitted or normalized by a
    /// checkpoint. Such a projection must fail cold admission rather than
    /// attaching artifacts derived from a different source. One row is encoded
    /// at a time; unchanged warm publications retain their existing digest.
    pub(super) fn native_artifact_source_digest(
        collection: &str,
        manager: &super::super::super::manager::SegmentManager,
    ) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        digest.update(&NATIVE_ARTIFACT_SOURCE_VERSION.to_le_bytes());
        digest.update(&(collection.len() as u64).to_le_bytes());
        digest.update(collection.as_bytes());
        manager.for_each_entity_in_id_order(|entity| {
            let bytes = UnifiedStore::serialize_entity_record(
                entity,
                None,
                reddb_file::STORE_VERSION_CURRENT,
            );
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(&bytes);
            // The entity codec stores named rows instead of their positional
            // columns, but native derivation observes both representations.
            if let EntityData::Row(row) = &entity.data {
                if row.named.is_some() {
                    let mut columns = Vec::new();
                    for value in &row.columns {
                        reddb_file::native_value::write_native_value(&mut columns, value);
                    }
                    digest.update(&(columns.len() as u64).to_le_bytes());
                    digest.update(&columns);
                }
            }
            true
        });
        *digest.finalize().as_bytes()
    }

    pub(super) fn recovered_native_artifact_versions(
        &self,
        existing: &[NativeVectorArtifactPageSummary],
    ) -> HashMap<String, NativeArtifactVersion> {
        let proofs = {
            let _publication = self.store.vault_publication_lock.lock();
            let Some(header) = self.store.physical_file_header() else {
                return HashMap::new();
            };
            let Ok(proofs) = self
                .store
                .read_native_artifact_sources(header.vector_artifact_page)
            else {
                return HashMap::new();
            };
            proofs
        };
        let mut versions = HashMap::new();
        for proof in proofs {
            let Some(manager) = self.store.get_collection(&proof.collection) else {
                continue;
            };
            let segments = manager.checkpoint_revisions();
            if proof.pages.len() != proof.summaries.len()
                || proof
                    .pages
                    .iter()
                    .map(|page| &page.artifact_kind)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != proof.pages.len()
                || proof
                    .summaries
                    .iter()
                    .map(|summary| &summary.artifact_kind)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != proof.summaries.len()
                || proof.pages.len()
                    != existing
                        .iter()
                        .filter(|page| page.collection == proof.collection)
                        .count()
                || proof
                    .pages
                    .iter()
                    .any(|page| page.collection != proof.collection || !existing.contains(page))
                || proof.summaries.iter().any(|summary| {
                    summary.collection != proof.collection
                        || proof
                            .pages
                            .iter()
                            .filter(|page| {
                                page.artifact_kind == summary.artifact_kind
                                    && page.byte_len == summary.serialized_bytes
                                    && page.checksum == summary.checksum
                            })
                            .count()
                            != 1
                })
                || Self::native_artifact_source_digest(&proof.collection, &manager)
                    != proof.source_digest
            {
                continue;
            }
            // Page checksums detect physical corruption; the persisted payload
            // checksum additionally binds these exact bytes to the source proof.
            let payloads_valid = {
                let _publication = self.store.vault_publication_lock.lock();
                self.current_native_artifact_pages().is_ok_and(|current| {
                    proof.pages.iter().all(|page| current.contains(page))
                        && proof
                            .pages
                            .iter()
                            .all(|page| self.store.read_native_artifact_payload(page).is_ok())
                })
            };
            if !payloads_valid || manager.checkpoint_revisions() != segments {
                continue;
            }
            versions.insert(
                proof.collection,
                NativeArtifactVersion {
                    manager: Arc::downgrade(&manager),
                    segments,
                    summaries: proof.summaries,
                    pages: proof.pages,
                    fulltext_document_ids_complete: proof.fulltext_document_ids_complete,
                    source_digest: Some(proof.source_digest),
                },
            );
        }
        versions
    }
}
