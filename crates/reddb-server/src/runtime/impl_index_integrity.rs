use super::*;

/// A derived checkpoint index which does not match its authoritative source.
#[derive(Debug)]
pub struct IndexIntegrityFinding {
    pub index: String,
    pub reason: String,
}

impl RedDBRuntime {
    /// Verify HASH/BTREE checkpoint postings against persisted source rows.
    ///
    /// This explicit, potentially expensive pass does not modify data or repair
    /// indexes. It checks the checkpoint generation, not uncheckpointed WAL or
    /// resident index overlays, and does not replace the offline physical scrub.
    /// Other index methods are rejected rather than reported as verified.
    pub fn verify_persisted_indexes(
        &self,
        collection: &str,
    ) -> RedDBResult<Vec<IndexIntegrityFinding>> {
        use super::index_store::IndexMethodKind;
        let topology = self.index_store_ref().collection_topology_lock(collection);
        let _topology = topology.read();
        let indexes = self.index_store_ref().list_indices(collection);
        let store = self.db().store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.into()))?;
        if !store.is_paged()
            || indexes.iter().any(|index| {
                !matches!(index.method, IndexMethodKind::Hash | IndexMethodKind::BTree)
            })
        {
            return Err(RedDBError::InvalidOperation(
                "persisted index verification requires paged HASH/BTREE indexes".into(),
            ));
        }
        let (_scratch, limit_bytes) = self.reserve_index_scan(&manager)?;
        let mut findings = Vec::new();
        for index in indexes {
            let mut names = vec![index.name.clone()];
            if index.method == IndexMethodKind::BTree && (index.columns.len() == 1 || index.unique)
            {
                names.push(super::paged_btree_index::equality_storage_name(&index.name));
            }
            for name in names {
                if let Some(reason) =
                    store.verify_persisted_secondary_index(collection, &name, limit_bytes)?
                {
                    findings.push(IndexIntegrityFinding {
                        index: index.name.clone(),
                        reason: format!("{name:?}: {reason}"),
                    });
                }
            }
        }
        Ok(findings)
    }
}
