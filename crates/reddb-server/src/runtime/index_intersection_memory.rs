//! Regression coverage for shared candidate admission during index intersections.

#[cfg(test)]
mod tests {
    use crate::wire::query_direct::try_handle_query_binary_direct;
    use crate::{RedDBOptions, RedDBRuntime};

    fn fixture() -> RedDBRuntime {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(16 * 1024 * 1024),
        )
        .unwrap();
        for sql in [
            "CREATE TABLE intersection_budget (id INT, a INT, b INT)",
            "CREATE INDEX intersection_a ON intersection_budget (a) USING BTREE",
            "CREATE INDEX intersection_b ON intersection_budget (b) USING BTREE",
        ] {
            runtime.execute_query(sql).unwrap();
        }
        for start in (0..4096).step_by(512) {
            let rows = (start..start + 512)
                .map(|id| format!("({id},{},{})", id % 8, 4095 - id))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO intersection_budget (id,a,b) VALUES {rows}"
                ))
                .unwrap();
        }
        runtime
    }

    #[test]
    fn sorted_intersection_rejects_candidate_allocations_before_a_small_reply() {
        let runtime = fixture();
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer(available as usize - 4096)
            .unwrap();
        let sql = "SELECT id FROM intersection_budget WHERE a BETWEEN 0 AND 4095 AND b BETWEEN 0 AND 4095 LIMIT 1";
        let reply = try_handle_query_binary_direct(&runtime, sql).expect("direct indexed query");
        assert_eq!(
            reply[4],
            reddb_wire::legacy::MSG_ERROR,
            "LIMIT 1 must not permit unadmitted 4096-ID candidate lists and intersection scratch"
        );
        assert!(String::from_utf8_lossy(&reply[5..]).contains("memory budget"));
        drop(reply);
        drop(held);
        let reply = try_handle_query_binary_direct(&runtime, sql).unwrap();
        assert_eq!(reply[4], reddb_wire::legacy::MSG_RESULT);
    }
    fn used(runtime: &RedDBRuntime) -> u64 {
        runtime
            .memory_accounting()
            .used_bytes(crate::storage::memory_pools::MemoryPool::QueryBuffers)
    }

    fn hold_except(
        runtime: &RedDBRuntime,
        bytes: usize,
    ) -> super::super::memory_admission::QueryBufferReservation {
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        runtime
            .try_reserve_query_buffer(available as usize - bytes)
            .unwrap()
    }

    fn range(column: &str) -> reddb_rql::ast::Filter {
        reddb_rql::ast::Filter::Between {
            field: reddb_rql::ast::FieldRef::TableColumn {
                table: "intersection_budget".into(),
                column: column.into(),
            },
            low: reddb_types::Value::Integer(0),
            high: reddb_types::Value::Integer(4095),
        }
    }

    #[test]
    fn sorted_intersection_admits_leaves_set_and_output_and_keeps_the_result_owner() {
        use crate::runtime::query_exec::try_sorted_index_lookup;
        let runtime = fixture();
        let store = runtime.index_store_ref();
        let left = try_sorted_index_lookup(&range("a"), "intersection_budget", store, None)
            .unwrap()
            .unwrap();
        let one = used(&runtime) as usize;
        let right = try_sorted_index_lookup(&range("b"), "intersection_budget", store, None)
            .unwrap()
            .unwrap();
        assert_eq!(used(&runtime) as usize, one * 2);
        let set = store.candidate_set(left.iter().map(|id| id.raw())).unwrap();
        let scratch = used(&runtime) as usize - one * 2;
        drop((left, right, set));
        assert_eq!(used(&runtime), 0);
        let filter = reddb_rql::ast::Filter::And(Box::new(range("a")), Box::new(range("b")));
        for headroom in [4096, one * 2 + 4096, one * 2 + scratch + 8] {
            let held = hold_except(&runtime, headroom);
            let before = used(&runtime);
            let error = try_sorted_index_lookup(&filter, "intersection_budget", store, Some(1))
                .unwrap_err();
            assert!(error.to_string().contains("memory budget"), "{error}");
            assert_eq!(
                used(&runtime),
                before,
                "failed lookup releases every temporary owner"
            );
            drop(held);
        }
        let result = try_sorted_index_lookup(&filter, "intersection_budget", store, None)
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 4096);
        assert_eq!(
            used(&runtime) as usize,
            one,
            "only the final candidate allocation remains"
        );
        runtime.refresh_memory_accounting();
        let mut iter = result.into_iter();
        assert!(iter.next().is_some());
        assert_eq!(used(&runtime) as usize, one);
        drop(iter);
        assert_eq!(used(&runtime), 0);
    }

    #[test]
    fn index_intersection_read_and_dml_failures_preserve_rows_and_release_memory() {
        let runtime = fixture();
        runtime
            .execute_query("CREATE INDEX intersection_ab ON intersection_budget (a,b) USING BTREE")
            .unwrap();
        let queries=[
            "SELECT id FROM intersection_budget WHERE a BETWEEN 0 AND 4095 LIMIT 1",
            "SELECT id FROM intersection_budget WHERE a > 0 LIMIT 1",
            "SELECT id FROM intersection_budget WHERE a IN (0,1,2,3,4,5,6,7) LIMIT 1",
            "SELECT id FROM intersection_budget WHERE a = 0 AND b BETWEEN 0 AND 4095 LIMIT 1",
            "SELECT id FROM intersection_budget WHERE a BETWEEN 0 AND 4095 AND b BETWEEN 0 AND 4095 LIMIT 1",
        ];
        // LIMIT can legitimately bound a single range/IN posting list. Force
        // full candidate materialization with ORDER BY for the SQL pressure test.
        for query in queries {
            let sql = query.replace(" LIMIT 1", " ORDER BY id LIMIT 1");
            let expected = runtime.execute_query(&sql).unwrap();
            runtime.invalidate_result_cache();
            let held = hold_except(&runtime, 4096);
            let error = runtime.execute_query(&sql).unwrap_err();
            assert!(
                error.to_string().contains("memory budget"),
                "{sql}: {error}"
            );
            drop(held);
            let actual = runtime.execute_query(&sql).unwrap();
            assert_eq!(actual.result.records.len(), expected.result.records.len());
            assert_eq!(
                actual.result.records[0].get("id"),
                expected.result.records[0].get("id")
            );
            assert_eq!(used(&runtime), 0);
        }
        let before = runtime
            .execute_query("SELECT id,a,b FROM intersection_budget ORDER BY id")
            .unwrap();
        for sql in [
            "DELETE FROM intersection_budget WHERE a BETWEEN 0 AND 4095 AND b BETWEEN 0 AND 4095",
            "UPDATE intersection_budget SET b = 0 WHERE a BETWEEN 0 AND 4095 AND b BETWEEN 0 AND 4095",
        ] {
            let held=hold_except(&runtime,4096);
            let error=runtime.execute_query(sql).unwrap_err();
            assert!(error.to_string().contains("budget"),"{error}");
            drop(held);
        }
        let after = runtime
            .execute_query("SELECT id,a,b FROM intersection_budget ORDER BY id")
            .unwrap();
        assert_eq!(before.result.records.len(), after.result.records.len());
        for (before, after) in before.result.records.iter().zip(&after.result.records) {
            for col in ["id", "a", "b"] {
                assert_eq!(before.get(col), after.get(col));
            }
        }
        assert_eq!(used(&runtime), 0);
    }

    #[test]
    fn candidate_bitmap_budget_covers_measured_heap_peak_and_full_width_ids() {
        use crate::storage::EntityId;
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(16 * 1024 * 1024),
        )
        .unwrap();
        let store = runtime.index_store_ref();
        for raw in [
            (0..4096).collect::<Vec<u64>>(),
            (0..4097).collect(),
            (0..16384).collect(),
            (0..4096).map(|id| id << 16).collect(),
            (0..4096).map(|id| (id << 32) + 1).collect(),
            vec![1, u32::MAX as u64, (1u64 << 32) + 1, u64::MAX],
        ] {
            let ids = store
                .collect_candidates(raw.iter().copied().map(EntityId::new))
                .unwrap();
            let (bitmap, allocations) =
                crate::storage::no_malloc_hot_paths::measure_allocations(|| {
                    store.candidate_bitmap(ids).unwrap()
                });
            assert!(
                used(&runtime) >= allocations.peak_live_bytes as u64,
                "{} ids: charged {}, peak {}",
                raw.len(),
                used(&runtime),
                allocations.peak_live_bytes
            );
            assert_eq!(bitmap.iter().collect::<Vec<_>>(), raw);
            drop(bitmap);
            assert_eq!(used(&runtime), 0);
        }
        let low = store
            .collect_candidates(std::iter::once(EntityId::new(1)))
            .unwrap();
        let high = store
            .collect_candidates(std::iter::once(EntityId::new((1u64 << 32) + 1)))
            .unwrap();
        let mut low = store.candidate_bitmap(low).unwrap();
        let high = store.candidate_bitmap(high).unwrap();
        low.intersect_with(&high);
        assert!(
            low.is_empty(),
            "distinct 64-bit IDs must never alias through a u32 cast"
        );
        drop((low, high));
        assert_eq!(used(&runtime), 0);
    }

    #[test]
    fn candidate_bitmap_intersections_cover_conversion_and_removal_scratch() {
        use crate::storage::{no_malloc_hot_paths::measure_allocations, EntityId};
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(16 * 1024 * 1024),
        )
        .unwrap();
        let store = runtime.index_store_ref();
        for shift in [0, 16, 32] {
            let left: Vec<_> = (0..8192u64).map(|id| EntityId::new(id << shift)).collect();
            let right: Vec<_> = (0..8192u64)
                .filter(|id| id % 3 == 0)
                .map(|id| EntityId::new(id << shift))
                .collect();
            // Keep pre-scope candidate owners alive while measuring AND scratch.
            let mut left = store.candidate_bitmap(left.into()).unwrap();
            let right = store.candidate_bitmap(right.into()).unwrap();
            let (_, allocations) = measure_allocations(|| left.intersect_with(&right));
            assert!(allocations.peak_live_bytes as u64 <= used(&runtime));
            assert_eq!(
                left.iter().collect::<Vec<_>>(),
                right.iter().collect::<Vec<_>>()
            );
            drop((left, right));
            assert_eq!(used(&runtime), 0);
        }
    }

    #[test]
    fn candidate_bitmap_denial_releases_input_and_retry_succeeds() {
        use crate::storage::EntityId;
        let runtime = fixture();
        let store = runtime.index_store_ref();
        let input = store
            .collect_candidates((0..8192).map(EntityId::new))
            .unwrap();
        let input_charge = used(&runtime);
        let held = hold_except(&runtime, 4096);
        let baseline = used(&runtime) - input_charge;
        let error = store
            .candidate_bitmap(input)
            .err()
            .expect("bitmap must be refused");
        assert!(error.to_string().contains("memory budget"));
        assert_eq!(used(&runtime), baseline, "denial releases the input owner");
        drop(held);
        let input = store
            .collect_candidates((0..8192).map(EntityId::new))
            .unwrap();
        let bitmap = store.candidate_bitmap(input).unwrap();
        assert_eq!(bitmap.len(), 8192);
        drop(bitmap);
        assert_eq!(used(&runtime), 0);
    }

    #[test]
    fn candidate_sets_share_the_budget_across_one_four_and_sixteen_owners() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        };
        let runtime = fixture();
        let store = runtime.index_store_ref();
        let set = store.candidate_set(0..4096).unwrap();
        let charge = used(&runtime) as usize;
        drop(set);
        for clients in [1usize, 4, 16] {
            let permits = clients.div_ceil(2);
            let held = hold_except(&runtime, permits * charge);
            let baseline = used(&runtime);
            let barrier = Barrier::new(clients + 1);
            let count = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for _ in 0..clients {
                    handles.push(scope.spawn(|| {
                        barrier.wait();
                        let result = store.candidate_set(0..4096);
                        if result.is_ok() {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        barrier.wait();
                        barrier.wait();
                        match result {
                            Ok(set) => assert_eq!(set.len(), 4096),
                            Err(error) => assert!(error.to_string().contains("memory budget")),
                        }
                    }));
                }
                barrier.wait();
                barrier.wait();
                let admitted = count.load(Ordering::SeqCst);
                let charged = used(&runtime);
                barrier.wait();
                for handle in handles {
                    handle.join().unwrap();
                }
                assert_eq!(admitted, permits);
                assert_eq!(charged, baseline + (permits * charge) as u64);
            });
            assert_eq!(used(&runtime), baseline);
            drop(held);
        }
        assert_eq!(used(&runtime), 0);
    }
}
