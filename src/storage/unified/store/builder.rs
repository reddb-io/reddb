use super::*;

impl Default for UnifiedStore {
    fn default() -> Self {
        Self::new()
    }
}

// Builder for creating entities with a fluent API
pub struct EntityBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    entity: UnifiedEntity,
}

impl EntityBuilder {
    /// Start building an entity
    pub fn new(
        store: Arc<UnifiedStore>,
        collection: impl Into<String>,
        kind: EntityKind,
        data: EntityData,
    ) -> Self {
        let collection_name = collection.into();
        let _ = store.get_or_create_collection(&collection_name);
        let id = store.next_entity_id();

        Self {
            store,
            collection: collection_name,
            entity: UnifiedEntity::new(id, kind, data),
        }
    }

    /// Add metadata
    pub fn metadata(self, key: impl Into<String>, value: MetadataValue) -> Self {
        // Store metadata separately via manager after insert
        self
    }

    /// Add an embedding
    pub fn embedding(
        mut self,
        name: impl Into<String>,
        vector: Vec<f32>,
        model: impl Into<String>,
    ) -> Self {
        self.entity
            .add_embedding(EmbeddingSlot::new(name, vector, model));
        self
    }

    /// Add a cross-reference
    pub fn cross_ref(
        mut self,
        target: EntityId,
        target_collection: impl Into<String>,
        ref_type: RefType,
    ) -> Self {
        self.entity.add_cross_ref(CrossRef::new(
            self.entity.id,
            target,
            target_collection,
            ref_type,
        ));
        self
    }

    /// Build and insert the entity
    pub fn insert(self) -> Result<EntityId, StoreError> {
        self.store.insert(&self.collection, self.entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_store_basic() {
        let store = UnifiedStore::new();
        store.create_collection("hosts").unwrap();

        let entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("192.168.1.1".to_string())],
        );

        let id = store.insert("hosts", entity).unwrap();
        assert!(store.get("hosts", id).is_some());
    }

    #[test]
    fn test_store_auto_create() {
        let store = UnifiedStore::new();

        let entity =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);

        let id = store.insert_auto("new_collection", entity).unwrap();
        assert!(store.get("new_collection", id).is_some());
    }

    #[test]
    fn test_cross_references() {
        let store = UnifiedStore::new();

        // Create hosts collection
        let host_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("192.168.1.1".to_string())],
        );
        let host_id = store.insert_auto("hosts", host_entity).unwrap();

        // Create vulns collection
        let vuln_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "vulns",
            1,
            vec![Value::Text("CVE-2024-1234".to_string())],
        );
        let vuln_id = store.insert_auto("vulns", vuln_entity).unwrap();

        // Add cross-reference
        store
            .add_cross_ref("hosts", host_id, "vulns", vuln_id, RefType::RelatedTo, 1.0)
            .unwrap();

        // Verify forward reference
        let refs = store.get_refs_from(host_id);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, vuln_id);

        // Verify reverse reference
        let back_refs = store.get_refs_to(vuln_id);
        assert_eq!(back_refs.len(), 1);
        assert_eq!(back_refs[0].0, host_id);
    }

    #[test]
    fn test_expand_refs() {
        let store = UnifiedStore::new();

        // Create a chain: A → B → C
        let _ = store.get_or_create_collection("test");

        let a = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.1]);
        let a_id = store.insert_auto("test", a).unwrap();

        let b = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.2]);
        let b_id = store.insert_auto("test", b).unwrap();

        let c = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.3]);
        let c_id = store.insert_auto("test", c).unwrap();

        store
            .add_cross_ref("test", a_id, "test", b_id, RefType::SimilarTo, 0.9)
            .unwrap();
        store
            .add_cross_ref("test", b_id, "test", c_id, RefType::SimilarTo, 0.8)
            .unwrap();

        // Expand from A with depth 2
        let expanded = store.expand_refs(a_id, 2, None);
        assert_eq!(expanded.len(), 2); // Should find B and C
    }

    #[test]
    fn test_query_all_collections() {
        let store = UnifiedStore::new();

        // Insert into multiple collections
        store
            .insert_auto(
                "hosts",
                UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]),
            )
            .unwrap();

        store
            .insert_auto(
                "vulns",
                UnifiedEntity::table_row(store.next_entity_id(), "vulns", 1, vec![]),
            )
            .unwrap();

        // Query all
        let results = store.query_all(|_| true);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_stats() {
        let store = UnifiedStore::new();

        let _ = store.get_or_create_collection("test");
        for i in 0..5 {
            store
                .insert_auto(
                    "test",
                    UnifiedEntity::vector(store.next_entity_id(), "v", vec![i as f32]),
                )
                .unwrap();
        }

        let stats = store.stats();
        assert_eq!(stats.collection_count, 1);
        assert_eq!(stats.total_entities, 5);
    }

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_path(name: &str) -> (FileGuard, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("rb_store_{}_{}.rdb", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    #[test]
    fn test_cross_refs_persist_file_mode() {
        let (_guard, path) = temp_path("file");
        let store = UnifiedStore::new();

        let row = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("10.0.0.1".to_string())],
        );
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        let vector =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);
        let vector_id = store.insert_auto("embeddings", vector).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();
        store
            .add_cross_ref(
                "graph",
                node_id,
                "embeddings",
                vector_id,
                RefType::NodeToVector,
                1.0,
            )
            .unwrap();

        store.save_to_file(&path).unwrap();

        let loaded = UnifiedStore::load_from_file(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));

        let graph_refs = loaded.get_refs_from(node_id);
        assert!(graph_refs.iter().any(|(id, kind, coll)| {
            *id == vector_id && *kind == RefType::NodeToVector && coll == "embeddings"
        }));

        let expanded = loaded.expand_refs(row_id, 2, None);
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == node_id && *depth == 1 }));
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == vector_id && *depth == 2 }));
    }

    #[test]
    fn test_cross_refs_persist_paged_mode() {
        let (_guard, path) = temp_path("paged");
        let store = UnifiedStore::open(&path).unwrap();

        let row = UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]);
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();

        store.persist().unwrap();

        drop(store);

        let loaded = UnifiedStore::open(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));
    }

    #[test]
    fn test_paged_mode_survives_multiple_reopens() {
        let (_guard, path) = temp_path("paged_multi_reopen");

        let store = UnifiedStore::open(&path).unwrap();
        store.set_config_tree(
            "red.system",
            &crate::json!({
                "hostname": "test-host",
                "arch": "x86_64",
                "started_at": 123_u64
            }),
        );
        let initial = store
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert!(initial >= 3);
        drop(store);

        let reopened = UnifiedStore::open(&path).unwrap();
        let first_reopen = reopened
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert_eq!(first_reopen, initial);
        drop(reopened);

        let reopened_again = UnifiedStore::open(&path).unwrap();
        let second_reopen = reopened_again
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert_eq!(second_reopen, initial);
    }

    #[test]
    fn test_global_ids_unique_across_collections() {
        let store = UnifiedStore::new();

        let entity_a = UnifiedEntity::table_row(EntityId::new(0), "alpha", 1, vec![]);
        let entity_b = UnifiedEntity::table_row(EntityId::new(0), "beta", 1, vec![]);

        let id_a = store.insert_auto("alpha", entity_a).unwrap();
        let id_b = store.insert_auto("beta", entity_b).unwrap();

        assert_ne!(id_a, id_b);

        store
            .add_cross_ref("alpha", id_a, "beta", id_b, RefType::RelatedTo, 1.0)
            .unwrap();

        let expanded = store.expand_refs(id_a, 1, None);
        assert!(expanded.iter().any(|(entity, _, _)| entity.id == id_b));
    }
}
