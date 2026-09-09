use reddb::storage::{EntityId, ManagerConfig, SegmentManager, UnifiedEntity};

fn assert_kind_results(manager: &SegmentManager, expected_live: &[EntityId]) {
    let mut live: Vec<_> = manager
        .query_all(|_| true)
        .into_iter()
        .map(|item| item.id)
        .collect();
    live.sort_unstable();
    assert_eq!(
        live, expected_live,
        "the full scan must retain every surviving item"
    );
    for kind in ["table", "vector", "graph_node", "graph_edge", "unknown"] {
        let mut expected: Vec<_> = manager
            .query_all(|item| item.kind.storage_type() == kind)
            .into_iter()
            .map(|item| item.id)
            .collect();
        let result = manager.get_by_kind(kind);
        assert!(
            result.iter().all(|item| item.kind.storage_type() == kind),
            "kind lookup returned another type for {kind}"
        );
        let mut expected_live = Vec::new();
        let mut actual: Vec<_> = result.into_iter().map(|item| item.id).collect();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected, "missing or duplicated items for {kind}");
    }
}

fn mixed_bulk_lifecycle(hashmap_first: bool, gap: bool) {
    // Rotate the first kind to catch both false positives and false negatives.
    for rotation in 0..4 {
        let manager = SegmentManager::with_config(
            "mixed",
            ManagerConfig {
                max_sealed_segments: 1,
                consolidation_entities_per_tick: 2,
                ..Default::default()
            },
        );
        assert!(manager
            .bulk_insert(Vec::new())
            .expect("empty bulk")
            .is_empty());
        if hashmap_first {
            let id = manager.next_entity_id();
            manager
                .insert(UnifiedEntity::vector(id, "mixed", vec![1.0]))
                .expect("seed hashmap storage");
            expected_live.push(id);
        }
        for batch in 0..2 {
            let mut items = vec![
                UnifiedEntity::table_row(EntityId::new(0), "mixed", 0, vec![]),
                UnifiedEntity::vector(EntityId::new(0), "mixed", vec![1.0]),
                UnifiedEntity::graph_node(EntityId::new(0), "mixed", "person", Default::default()),
                UnifiedEntity::graph_edge(
                    EntityId::new(0),
                    "mixed",
                    "alice",
                    "bob",
                    1.0,
                    Default::default(),
                ),
            ];
            items.rotate_left(rotation);
            // Repeated runs plus a return to the first kind exercise both index
            // reservation and reuse without changing storage order.
            items.push(items[0].clone());
            let mut items: Vec<_> = items
                .into_iter()
                .flat_map(|item| [item.clone(), item])
                .collect();
            for (index, item) in items.iter_mut().enumerate() {
                if gap && index == 2 {
                    let _ = manager.next_entity_id();
                }
                item.id = manager.next_entity_id();
            }
            let ids = manager.bulk_insert(items).expect("mixed bulk");
            expected_live.extend(&ids);
            for id in &ids {
                assert_eq!(manager.get(*id).expect("point lookup").id, *id);
            }
            assert_kind_results(&manager, &expected_live);
            manager.force_seal().expect("seal mixed segment");
            assert_kind_results(&manager, &expected_live);
            assert!(manager.delete(ids[batch]).expect("delete sealed item"));
            expected_live.retain(|id| *id != ids[batch]);
            assert_kind_results(&manager, &expected_live);
        }
        for _ in 0..32 {
            manager.run_maintenance().expect("consolidation tick");
            assert_kind_results(&manager, &expected_live);
            if manager.stats().consolidation.runs_completed > 0 {
                break;
            }
        }
        assert_eq!(manager.stats().consolidation.runs_completed, 1);
    }
}

#[test]
fn mixed_bulk_kind_index_flat_storage() {
    mixed_bulk_lifecycle(false, false);
}

#[test]
fn mixed_bulk_kind_index_hashmap_storage() {
    mixed_bulk_lifecycle(true, false);
}

#[test]
fn mixed_bulk_kind_index_flat_with_id_gaps() {
    mixed_bulk_lifecycle(false, true);
}
