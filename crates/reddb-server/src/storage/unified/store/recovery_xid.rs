//! The XID upper bound belongs to the same checksummed catalog as its roots.
//! Unknown or replaced bases require a physical scan; resident WAL overlays
//! remain part of bootstrap and of every subsequent summary publication.
use super::*;

pub(super) struct RecoveryXidSummary {
    pub high_water_mark: u64,
    pub roots: std::collections::BTreeMap<String, u32>,
}

impl UnifiedStore {
    pub(super) fn base_xid_high_water_mark(
        &self,
        collections: &HashMap<String, Arc<SegmentManager>>,
    ) -> Option<u64> {
        let summary = self.recovery_xid_summary.read();
        for (name, manager) in collections {
            if let Some(root) = manager.backing_root_page() {
                if summary.as_ref()?.roots.get(name) != Some(&root) {
                    return None;
                }
            }
        }
        Some(
            summary
                .as_ref()
                .map_or(0, |summary| summary.high_water_mark),
        )
    }

    pub(crate) fn startup_xid_high_water_mark(&self) -> Option<u64> {
        self.base_xid_high_water_mark(&self.resident.collections.read())
    }

    /// Exclusive constructor recovery has visited every physical header before
    /// handing this store to a live transaction manager. This changes no disk
    /// bytes: only a later fenced catalog publication can make it durable.
    pub(crate) fn remember_startup_xid_high_water_mark(&self, high_water_mark: u64) {
        assert!(self.snapshot_manager.get().is_none());
        let roots = self
            .resident
            .collections
            .read()
            .iter()
            .filter_map(|(name, manager)| {
                manager.backing_root_page().map(|root| (name.clone(), root))
            })
            .collect();
        *self.recovery_xid_summary.write() = Some(RecoveryXidSummary {
            high_water_mark,
            roots,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xid_summary_preserves_imported_and_deleted_headers_across_reopen() {
        let directory = tempfile::tempdir().expect("XID summary fixture");
        let path = directory.path().join("headers.rdb");
        let config = UnifiedStoreConfig {
            page_cache_slots: Some(32),
            resident_memory_bytes_max: Some(2 * 1024 * 1024),
            ..UnifiedStoreConfig::default()
        };
        let store = UnifiedStore::open_with_config(&path, config.clone()).expect("store");
        store.create_collection("rows").expect("collection");
        let mut old = UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![]);
        old.xmin = 8000;
        old.xmax = 12000;
        store.insert("rows", old).expect("persisted tombstone");
        store.persist().expect("publish headers");
        assert_eq!(store.startup_xid_high_water_mark(), Some(12000));
        drop(store);

        let store = UnifiedStore::open_with_config(&path, config.clone()).expect("reopen");
        assert_eq!(store.startup_xid_high_water_mark(), Some(12000));
        let mut imported = UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![]);
        imported.xmin = 90000;
        store
            .insert("rows", imported)
            .expect("imported XID beyond allocator");
        store.persist().expect("publish dirty header");
        assert_eq!(store.startup_xid_high_water_mark(), Some(90000));
        assert!(store
            .delete("rows", EntityId::new(1))
            .expect("remove old version"));
        assert!(store
            .delete("rows", EntityId::new(2))
            .expect("remove import"));
        store.persist().expect("publish empty base");
        drop(store);
        let reopened = UnifiedStore::open_with_config(&path, config).expect("empty reopen");
        assert_eq!(reopened.startup_xid_high_water_mark(), Some(90000));
    }
}
