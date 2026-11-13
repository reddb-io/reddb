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
            || store.config().resident_memory_bytes_max.is_none()
            || indexes.iter().any(|index| {
                !matches!(index.method, IndexMethodKind::Hash | IndexMethodKind::BTree)
            })
        {
            return Err(RedDBError::InvalidOperation(
                "persisted index verification requires disk-resident paged HASH/BTREE indexes"
                    .into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_serverless_indexes_reject_physical_projection_verification() {
        let directory = tempfile::tempdir().expect("serverless index fixture");
        let options = crate::api::RedDBOptions::persistent(directory.path().join("index.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::Serverless.selection(),
            )
            .expect("serverless profile");
        let runtime = RedDBRuntime::with_options(options).expect("serverless runtime");
        runtime
            .execute_query("CREATE TABLE records (id INTEGER)")
            .expect("create table");
        runtime
            .execute_query("CREATE INDEX records_id ON records (id) USING HASH")
            .expect("create index");

        let error = runtime
            .verify_persisted_indexes("records")
            .expect_err("resident index has no physical projection to verify");
        assert!(error.to_string().contains(
            "persisted index verification requires disk-resident paged HASH/BTREE indexes"
        ));
    }
}
