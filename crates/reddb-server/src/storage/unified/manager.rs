//! Native compatibility path for the canonical shared collection lifecycle.
pub use reddb_engine::storage::manager::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::mvcc::SnapshotContext;
    use crate::storage::transaction::snapshot::{Snapshot, SnapshotManager};
    use crate::storage::unified::entity::{EntityId, EntityKind, UnifiedEntity};
    use reddb_types::Value;
    use std::collections::HashSet;
    use std::sync::Arc;

    /// Bulk-load `count` rows into a fresh segment and seal it. Bulk-inserted
    /// entities land in flat storage, where a tombstone keeps its payload
    /// resident — which is the memory consolidation reclaims.
    fn seal_bulk_segment(manager: &SegmentManager, count: usize) -> Vec<EntityId> {
        let entities: Vec<UnifiedEntity> = (0..count)
            .map(|i| {
                UnifiedEntity::table_row(
                    manager.next_entity_id(),
                    "users",
                    i as u64,
                    vec![Value::Integer(i as i64), Value::text(format!("row-{i}"))],
                )
            })
            .collect();
        let ids = manager.bulk_insert(entities).expect("bulk insert");
        manager.force_seal().expect("seal");
        ids
    }

    fn drain_maintenance(manager: &SegmentManager) {
        for _ in 0..1_000 {
            manager.run_maintenance().expect("maintenance");
            let stats = manager.stats().consolidation;
            if stats.runs_completed > 0 && stats.runs_started == stats.runs_completed {
                return;
            }
        }
        panic!("consolidation failed to converge");
    }

    #[test]
    fn query_scan_callers_use_snapshot_api_for_visibility() {
        let table = include_str!("../../runtime/query_exec/table.rs");
        let aggregate = include_str!("../../runtime/query_exec/aggregate.rs");

        assert_eq!(
            table.matches(".for_each_entity(|entity|").count(),
            1,
            "only the visibility-independent schema inspection may use the raw table iterator"
        );
        assert!(!table.contains(".query_all_zoned_with_stats("));
        assert!(!table.contains(".for_each_entity_zoned_with_stats("));
        assert!(!table.contains("TableRowMvccReadResolver::captured"));

        assert!(!aggregate.contains("TableRowMvccReadResolver"));
        assert!(!aggregate.contains(".for_each_entity("));
        assert!(!aggregate.contains(".fold_entities_parallel("));
    }

    #[test]
    fn remaining_visibility_scan_callers_use_snapshot_api() {
        let aggregate_pushdown =
            include_str!("../../runtime/query_exec/aggregate_pushdown_dispatch.rs");
        let ask = include_str!("../../runtime/ask_pipeline.rs");
        let dml = include_str!("../../runtime/impl_dml.rs");
        let dml_targets = include_str!("../../runtime/dml_target_scan.rs");
        let events = include_str!("../../runtime/impl_events.rs");
        let graph_commands = include_str!("../../runtime/impl_graph_commands.rs");
        let graph_dsl = include_str!("../../runtime/graph_dsl.rs");
        let graph_tvf = include_str!("../../runtime/graph_tvf.rs");
        let kv = include_str!("../../runtime/impl_kv.rs");
        let queue = include_str!("../../runtime/impl_queue.rs");
        let primary_queue = include_str!("../../runtime/primary_queue_store.rs");
        let record_search = include_str!("../../runtime/record_search.rs");
        let search = include_str!("../../runtime/impl_search.rs");
        let vector = include_str!("../../runtime/query_exec/vector.rs");

        assert!(!aggregate_pushdown.contains("TableRowMvccReadResolver"));
        assert!(!aggregate_pushdown.contains(".for_each_entity("));
        assert!(!ask.contains("for entity in manager.query_all(|_| true)"));
        assert!(!dml.contains("let entities = manager.query_all(|_| true);"));
        // The DML target scan keeps raw iteration on purpose: its
        // visibility is kind-conditional (live table rows at the tip
        // version, everything else under the snapshot), which the uniform
        // scan API cannot express. `candidate_visible` is the single gate.
        assert_eq!(
            dml_targets
                .matches("self.candidate_visible(snapshot.as_ref(), entity)")
                .count(),
            2,
            "both raw DML iterations must route through candidate_visible"
        );
        // The event-backfill dedup set is deliberately visibility-free:
        // it must include consumed messages or backfill re-enqueues them.
        assert_eq!(
            events.matches("manager.query_all(").count(),
            1,
            "only the visibility-free backfill dedup may use query_all"
        );
        assert!(!graph_commands.contains("TableRowMvccReadResolver"));
        assert!(!graph_dsl.contains("entity_visible_with_context(snap_ctx.as_ref(), e)"));
        assert!(!graph_tvf.contains("entity_visible_with_context(snap_ctx.as_ref(), &entity)"));
        assert!(!kv.contains("TableRowMvccReadResolver"));
        assert!(!queue.contains("entity_visible_with_context(snap_ctx.as_ref(), entity)"));
        assert!(!primary_queue.contains("entity_visible_with_context(snap_ctx.as_ref(), entity)"));
        assert!(!record_search.contains("TableRowMvccReadResolver"));
        assert!(!search.contains("for entity in manager.query_all(|_| true)"));
        assert!(!vector.contains("manager.query_all("));
    }

    /// Strip `#[cfg(test)] mod … { … }` blocks so the raw-iteration gate
    /// only sees production code. Blocks are matched at column zero and
    /// closed by brace balance, which is why a file may carry a test
    /// module in the middle and still keep production code after it.
    fn production_source(src: &str) -> String {
        let mut kept: Vec<&str> = Vec::new();
        let lines: Vec<&str> = src.lines().collect();
        let mut idx = 0;
        while idx < lines.len() {
            let starts_test_mod = lines[idx].starts_with("#[cfg(test)]")
                && lines
                    .get(idx + 1)
                    .is_some_and(|next| next.starts_with("mod "));
            if !starts_test_mod {
                kept.push(lines[idx]);
                idx += 1;
                continue;
            }
            idx += 1;
            let mut depth: isize = 0;
            while idx < lines.len() {
                depth += lines[idx].matches('{').count() as isize;
                depth -= lines[idx].matches('}').count() as isize;
                idx += 1;
                if depth <= 0 {
                    break;
                }
            }
        }
        kept.join("\n")
    }

    /// Count raw (visibility-bypassing) iteration entry points.
    fn raw_iteration_sites(src: &str) -> usize {
        src.matches("query_all(").count()
            + src.matches(".for_each_entity").count()
            + src.matches("fold_entities_parallel(").count()
            + src.matches(".try_for_each_entity(").count()
            + src.matches(".try_for_each_key_entity(").count()
            + src.matches(".try_paged_physical_cursor(").count()
            + src.matches(".try_paged_key_physical_cursor(").count()
    }

    #[test]
    fn raw_iteration_gate_counts_fallible_physical_entry_points() {
        for source in [
            "manager.try_for_each_entity(usize::MAX, visit)",
            "manager.try_for_each_key_entity(key, visit)",
            "manager.try_paged_physical_cursor()",
            "manager.try_paged_key_physical_cursor(key)",
        ] {
            assert_eq!(raw_iteration_sites(source), 1, "{source}");
        }
        assert_eq!(
            raw_iteration_sites("manager.try_paged_entity_cursor(snapshot)"),
            0
        );
        assert_eq!(
            raw_iteration_sites("manager.try_paged_key_entity_cursor(key, snapshot)"),
            0
        );
    }

    #[test]
    fn raw_iteration_confined_to_maintenance_allowlist() {
        // The query paths migrated by Spec #2107 batches 1 and 2. Inside
        // this perimeter raw iteration is maintenance-only: it bypasses
        // MVCC visibility entirely, so every remaining site is either an
        // internal catalog/bookkeeping read that must observe all physical
        // versions, or one of the three exceptions the batch reviews
        // documented. A new raw-iteration site changes the count and fails
        // this gate — route it through `SegmentManager::scan*` instead.
        let allowlist: &[(&str, &str, usize, &str)] = &[
            (
                "runtime/query_exec/table.rs",
                include_str!("../../runtime/query_exec/table.rs"),
                1,
                "documented exception: schema inspection (does the collection ever \
                 observe these fields?) is visibility-independent",
            ),
            (
                "runtime/query_exec/aggregate.rs",
                include_str!("../../runtime/query_exec/aggregate.rs"),
                0,
                "fully migrated by batch 1",
            ),
            (
                "runtime/query_exec/aggregate_pushdown_dispatch.rs",
                include_str!("../../runtime/query_exec/aggregate_pushdown_dispatch.rs"),
                0,
                "fully migrated by batch 1",
            ),
            (
                "runtime/query_exec/vector.rs",
                include_str!("../../runtime/query_exec/vector.rs"),
                0,
                "fully migrated by batch 2",
            ),
            (
                "runtime/ask_pipeline.rs",
                include_str!("../../runtime/ask_pipeline.rs"),
                0,
                "fully migrated by batch 2",
            ),
            (
                "runtime/impl_dml.rs",
                include_str!("../../runtime/impl_dml.rs"),
                1,
                "documented exception: ON CONFLICT resolves a unique key against \
                 physical versions on purpose. A snapshot scan cannot see a row an \
                 uncommitted concurrent writer already holds the key for, which is \
                 exactly the case that must raise a serialization conflict rather \
                 than insert a duplicate; the same pass reaps index entries left by \
                 aborted writers. \
                 TODO(#2263 follow-up): narrow the candidates through the unique \
                 index instead of walking the collection",
            ),
            (
                "runtime/dml_target_scan.rs",
                include_str!("../../runtime/dml_target_scan.rs"),
                2,
                "documented exception: kind-conditional visibility the uniform scan \
                 API cannot express; both iterations gate on candidate_visible",
            ),
            (
                "runtime/impl_events.rs",
                include_str!("../../runtime/impl_events.rs"),
                1,
                "documented exception: backfill dedup must include consumed messages, \
                 so it is deliberately visibility-free",
            ),
            (
                "runtime/impl_graph_commands.rs",
                include_str!("../../runtime/impl_graph_commands.rs"),
                0,
                "native node properties resolve one snapshot-visible, RLS-admitted version",
            ),
            (
                "runtime/graph_dsl.rs",
                include_str!("../../runtime/graph_dsl.rs"),
                1,
                "remaining legacy debt: unused edge-property helper; search expansion \
                 now uses snapshot visitors and RLS-gated hydration",
            ),
            (
                "runtime/graph_tvf.rs",
                include_str!("../../runtime/graph_tvf.rs"),
                0,
                "fully migrated by batch 2",
            ),
            (
                "runtime/impl_kv.rs",
                include_str!("../../runtime/impl_kv.rs"),
                1,
                "non-versioned KV listings intentionally read physical rows; \
                 versioned listings use the captured snapshot. Vault history/export \
                 and point visibility are centralized in keyed_spine",
            ),
            (
                "runtime/keyed_spine.rs",
                include_str!("../../runtime/keyed_spine.rs"),
                3,
                "physical history is explicit only when snapshot is None: \
                 keyed/all cold cursors and the resident fallback. Snapshot callers \
                 use paged entity cursors or scan with captured visibility",
            ),
            (
                "runtime/impl_queue.rs",
                include_str!("../../runtime/impl_queue.rs"),
                10,
                "maintenance: red_queue_meta bookkeeping (config, groups, acks, \
                 pending, dedup TTL reaper, gauge counts) — queue message bodies are \
                 read through scan. \
                 TODO(#2138 follow-up): QUEUE PENDING projects meta rows to clients",
            ),
            (
                "runtime/primary_queue_store.rs",
                include_str!("../../runtime/primary_queue_store.rs"),
                3,
                "maintenance: generic red_queue_meta read/delete helpers and the \
                 next-position allocator, which must not collide with rows invisible \
                 to the writer's snapshot",
            ),
            (
                "runtime/record_search.rs",
                include_str!("../../runtime/record_search.rs"),
                0,
                "fully migrated by batch 2",
            ),
            (
                "runtime/search_graph.rs",
                include_str!("../../runtime/search_graph.rs"),
                0,
                "search graph preparation uses snapshot visitors and gated hydration",
            ),
            (
                "runtime/impl_search.rs",
                include_str!("../../runtime/impl_search.rs"),
                1,
                "maintenance: ask-audit retention sweep must see every physical row",
            ),
        ];

        for (path, src, allowed, reason) in allowlist {
            let found = raw_iteration_sites(&production_source(src));
            assert_eq!(
                found, *allowed,
                "{path}: {found} raw-iteration sites, allowlist permits {allowed} \
                 ({reason}). Raw iteration bypasses MVCC visibility — use \
                 SegmentManager::scan / scan_for_each / scan_fold_parallel / \
                 scan_zoned_with_stats, or extend the allowlist with a maintenance \
                 justification."
            );
        }
    }

    #[test]
    fn explicit_snapshot_scan_ignores_ambient_thread_local() {
        let manager = Arc::new(SegmentManager::new("accounts"));
        // (xmin, xmax) — mix of committed, in-progress and future writers.
        let versions = [(0, 0), (3, 0), (11, 0), (4, 0), (3, 7), (3, 11), (3, 4)];
        for (index, (xmin, xmax)) in versions.iter().enumerate() {
            let row_id = index as u64 + 1;
            let mut entity = UnifiedEntity::table_row(
                EntityId::new(row_id),
                "accounts",
                row_id,
                vec![Value::Integer(row_id as i64)],
            );
            entity.set_xmin(*xmin);
            entity.set_xmax(*xmax);
            manager.insert(entity).unwrap();
        }

        let explicit = SnapshotContext {
            snapshot: Snapshot {
                xid: 10,
                in_progress: HashSet::from([4]),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };
        // A decoy statement frame that would hide almost everything if the
        // scan ever consulted ambient state instead of its argument.
        let decoy = SnapshotContext {
            snapshot: Snapshot {
                xid: 1,
                in_progress: HashSet::new(),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };

        let row_ids = |entities: Vec<UnifiedEntity>| -> HashSet<u64> {
            entities
                .into_iter()
                .filter_map(|entity| match entity.kind {
                    EntityKind::TableRow { row_id, .. } => Some(row_id),
                    _ => None,
                })
                .collect()
        };

        let guard = crate::runtime::execution_context::CurrentSnapshotGuard::install(decoy);
        let on_installing_thread = row_ids(manager.scan(Some(&explicit), |_| true));
        drop(guard);

        let spawned_manager = Arc::clone(&manager);
        let spawned_snapshot = explicit.clone();
        let on_fresh_thread = std::thread::spawn(move || {
            row_ids(spawned_manager.scan(Some(&spawned_snapshot), |_| true))
        })
        .join()
        .unwrap();

        assert_eq!(
            on_installing_thread, on_fresh_thread,
            "an explicit snapshot must produce the same visible set on any thread"
        );
        assert_eq!(on_installing_thread, HashSet::from([1, 2, 6, 7]));

        // The raw iterators are the maintenance contract: no visibility
        // filtering at all, on any thread, regardless of installed frames.
        let raw_manager = Arc::clone(&manager);
        let raw_count = std::thread::spawn(move || raw_manager.query_all(|_| true).len())
            .join()
            .unwrap();
        assert_eq!(
            raw_count,
            versions.len(),
            "raw iteration must expose every physical version"
        );
    }

    #[test]
    fn retained_memory_gates_runtime_growth_until_the_cursor_is_cancelled() {
        use crate::storage::memory_pools::MemoryPool;
        use crate::{RedDBOptions, RedDBRuntime};

        let budget = 128 * 1024;
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(budget))
                .expect("runtime");
        runtime
            .db()
            .store()
            .create_collection("test")
            .expect("collection");
        let manager = runtime
            .db()
            .store()
            .get_collection("test")
            .expect("manager");
        let ids = seal_bulk_segment(&manager, 100);
        for id in ids.iter().take(40) {
            manager.delete(*id).expect("delete before scan");
        }
        let source_bytes = manager.resident_bytes();
        let mut growth = 0;
        let mut held_used = 0;
        assert!(!manager.scan_batches(
            None,
            || true,
            |_| {
                drain_maintenance(&manager);
                runtime.refresh_memory_accounting();
                // Other collections (including the runtime catalog) share this
                // arena, so derive headroom from the complete runtime sample.
                held_used = runtime.memory_accounting().total_used_bytes();
                growth = budget
                    .checked_sub(held_used)
                    .expect("fixture fits while the cursor is open")
                    + source_bytes / 2;
                assert!(growth > 0);
                assert!(
                    runtime
                        .admit_non_evictable_growth(
                            MemoryPool::SegmentArena,
                            "growth with pinned source",
                            growth,
                        )
                        .is_err(),
                    "retained source must participate in runtime admission"
                );
                false
            }
        ));
        runtime.refresh_memory_accounting();
        assert_eq!(
            held_used - runtime.memory_accounting().total_used_bytes(),
            source_bytes
        );
        let _reservation = runtime
            .admit_non_evictable_growth(
                MemoryPool::SegmentArena,
                "growth after cursor cancellation",
                growth,
            )
            .expect("the same growth fits once the cursor releases its source");
    }
}
