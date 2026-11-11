//! Shared owned admission for resident index candidates and intersection workspaces.

use super::memory_admission::QueryBufferReservation;
use super::{RedDBRuntime, RuntimeInner};
use crate::storage::unified::hash_index::{HashCandidateAdmission, HashIndexError};
use std::sync::{Arc, Weak};

struct RuntimeHashCandidateAdmission {
    runtime: Weak<RuntimeInner>,
}

impl RuntimeHashCandidateAdmission {
    fn runtime(&self) -> Result<RedDBRuntime, HashIndexError> {
        self.runtime
            .upgrade()
            .map(|inner| RedDBRuntime {
                inner,
                background_tasks: None,
            })
            .ok_or_else(|| HashIndexError::Read("runtime closed".into()))
    }
}

impl HashCandidateAdmission for RuntimeHashCandidateAdmission {
    fn sample(&self) -> Result<(), HashIndexError> {
        self.runtime()?.refresh_memory_accounting();
        Ok(())
    }

    fn reserve(&self, bytes: usize) -> Result<Box<dyn Send + Sync>, HashIndexError> {
        let runtime = self.runtime()?;
        let bytes = bytes
            .checked_add(std::mem::size_of::<QueryBufferReservation>())
            .ok_or_else(|| {
                HashIndexError::Read("index candidate allocation size overflow".into())
            })?;
        let guard = runtime.try_reserve_query_buffer_from_sample(bytes).ok_or_else(|| {
            runtime.memory_accounting().record_admission_denied();
            HashIndexError::Read(format!(
                "index candidate memory budget exceeded: requested {bytes} bytes, used {} bytes, budget {} bytes",
                runtime.memory_accounting().total_used_bytes(),
                runtime.memory_budget().resolved_bytes,
            ))
        })?;
        Ok(Box::new(guard))
    }
}

impl RedDBRuntime {
    pub(super) fn install_hash_candidate_admission(&self) {
        self.index_store_ref()
            .hash
            .install_candidate_admission(Arc::new(RuntimeHashCandidateAdmission {
                runtime: Arc::downgrade(&self.inner),
            }));
    }
}

#[cfg(test)]
mod tests {
    use super::super::memory_admission::MemoryReservation;
    use crate::storage::memory_pools::MemoryPool;
    use crate::{RedDBOptions, RedDBRuntime};

    fn fixture() -> RedDBRuntime {
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(16 * 1024 * 1024),
        )
        .unwrap();
        runtime
            .execute_query("CREATE TABLE candidate_budget (id INT, bucket INT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX candidate_bucket ON candidate_budget (bucket) USING HASH")
            .unwrap();
        for start in (0..4096).step_by(512) {
            let rows = (start..start + 512)
                .map(|id| format!("({id},7)"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO candidate_budget (id,bucket) VALUES {rows}"
                ))
                .unwrap();
        }
        runtime.flush().expect("settle fixture WAL");
        // These tests control the exact headroom between a storage sample and
        // candidate admission. Keep background maintenance from changing the
        // sampled resident footprint inside that deliberately narrow window.
        drop(runtime.background_tasks.take());
        runtime.refresh_memory_accounting();
        runtime
    }

    #[test]
    fn resident_hash_candidates_reserve_before_copy_and_hold_until_iterator_drops() {
        let runtime = fixture();
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer(available as usize - 4096)
            .unwrap();
        let candidates = runtime.index_store_ref().hash_lookup(
            "candidate_budget",
            "candidate_bucket",
            &7i64.to_le_bytes(),
        );
        assert!(
            candidates.is_err(),
            "4096 HASH IDs must not be copied with only 4096 bytes of shared headroom"
        );
        assert!(candidates.unwrap_err().contains("memory budget"));
        drop(held);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        let candidates = runtime
            .index_store_ref()
            .hash_lookup("candidate_budget", "candidate_bucket", &7i64.to_le_bytes())
            .unwrap();
        assert_eq!(candidates.len(), 4096);
        let charged = runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers);
        assert!(charged >= 4096 * std::mem::size_of::<crate::storage::EntityId>() as u64);
        runtime.refresh_memory_accounting();
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            charged
        );
        let mut iter = candidates.into_iter();
        assert!(iter.next().is_some());
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            charged
        );
        drop(iter);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
    fn query_bytes(runtime: &RedDBRuntime) -> u64 {
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers)
    }

    fn leave_headroom<'runtime>(
        runtime: &'runtime RedDBRuntime,
        bytes: usize,
    ) -> MemoryReservation<'runtime> {
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        runtime
            .try_reserve_memory_growth(available - bytes as u64)
            .unwrap()
    }

    fn in_filter(values: Vec<reddb_types::Value>) -> reddb_rql::ast::Filter {
        reddb_rql::ast::Filter::In {
            field: reddb_rql::ast::FieldRef::TableColumn {
                table: "candidate_budget".into(),
                column: "bucket".into(),
            },
            values,
        }
    }

    #[test]
    fn resident_hash_in_union_admits_the_sum_and_preserves_candidate_order() {
        use super::super::query_exec::try_hash_eq_lookup;
        use reddb_types::Value;
        let runtime = fixture();
        let rows = (4096..4608)
            .map(|id| format!("({id},8)"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO candidate_budget (id,bucket) VALUES {rows}"
            ))
            .unwrap();
        let expected = [7i64, 8]
            .into_iter()
            .flat_map(|key| {
                runtime
                    .index_store_ref()
                    .hash_lookup("candidate_budget", "candidate_bucket", &key.to_le_bytes())
                    .unwrap()
                    .into_iter()
            })
            .collect::<Vec<_>>();
        // Each posting list fits independently; the combined allocation does not.
        let filter = in_filter(vec![Value::Integer(7), Value::UnsignedInteger(8)]);
        let held = leave_headroom(&runtime, 34 * 1024);
        let baseline = query_bytes(&runtime);
        let one = runtime
            .index_store_ref()
            .hash_lookup("candidate_budget", "candidate_bucket", &7i64.to_le_bytes())
            .unwrap();
        assert_eq!(one.len(), 4096);
        drop(one);
        let error =
            try_hash_eq_lookup(&filter, "candidate_budget", runtime.index_store_ref()).unwrap_err();
        assert!(error.to_string().contains("memory budget"), "{error}");
        assert_eq!(query_bytes(&runtime), baseline);
        drop(held);
        let result = try_hash_eq_lookup(&filter, "candidate_budget", runtime.index_store_ref())
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 4608);
        assert_eq!(result, expected);
        assert!(
            query_bytes(&runtime) >= 4608 * std::mem::size_of::<crate::storage::EntityId>() as u64
        );
        let iterator = result.into_iter();
        runtime.refresh_memory_accounting();
        assert!(query_bytes(&runtime) > 0);
        drop(iterator);
        assert_eq!(query_bytes(&runtime), 0);
    }

    #[test]
    fn resident_hash_misses_do_not_allocate_or_sample_and_unsupported_in_falls_back() {
        use super::super::query_exec::try_hash_eq_lookup;
        use reddb_types::Value;
        let runtime = fixture();
        let samples = runtime.inner.memory_reservations.lock().samples;
        let (_, allocations) = crate::storage::no_malloc_hot_paths::measure_allocations(|| {
            for _ in 0..100 {
                let empty = runtime
                    .index_store_ref()
                    .hash_lookup("candidate_budget", "candidate_bucket", &99i64.to_le_bytes())
                    .unwrap();
                assert!(empty.is_empty());
            }
        });
        assert_eq!(allocations.allocs, 0);
        for values in [
            vec![],
            vec![Value::Integer(99)],
            vec![Value::text("missing")],
        ] {
            assert!(try_hash_eq_lookup(
                &in_filter(values),
                "candidate_budget",
                runtime.index_store_ref()
            )
            .unwrap()
            .unwrap()
            .is_empty());
        }
        assert!(try_hash_eq_lookup(
            &in_filter(vec![Value::Integer(7), Value::Float(7.0)]),
            "candidate_budget",
            runtime.index_store_ref()
        )
        .unwrap()
        .is_none());
        assert!(runtime
            .index_store_ref()
            .hash_lookup("candidate_budget", "missing", b"x")
            .is_err());
        assert_eq!(runtime.inner.memory_reservations.lock().samples, samples);
        assert_eq!(query_bytes(&runtime), 0);
    }

    #[test]
    fn resident_hash_concurrent_owners_share_one_budget_at_one_four_and_sixteen_clients() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        };
        let runtime = fixture();
        let candidates = runtime
            .index_store_ref()
            .hash_lookup("candidate_budget", "candidate_bucket", &7i64.to_le_bytes())
            .unwrap();
        let charge = query_bytes(&runtime) as usize;
        drop(candidates);
        for clients in [1usize, 4, 16] {
            let permits = clients.div_ceil(2);
            let held = leave_headroom(&runtime, permits * charge);
            let baseline = query_bytes(&runtime);
            let barrier = Barrier::new(clients + 1);
            let successes = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for _ in 0..clients {
                    handles.push(scope.spawn(|| {
                        barrier.wait();
                        let result = runtime.index_store_ref().hash_lookup(
                            "candidate_budget",
                            "candidate_bucket",
                            &7i64.to_le_bytes(),
                        );
                        if result.is_ok() {
                            successes.fetch_add(1, Ordering::SeqCst);
                        }
                        barrier.wait();
                        barrier.wait();
                        match result {
                            Ok(candidates) => assert_eq!(candidates.into_iter().count(), 4096),
                            Err(error) => assert!(error.contains("memory budget"), "{error}"),
                        }
                    }));
                }
                barrier.wait();
                barrier.wait();
                let admitted = successes.load(Ordering::SeqCst);
                let charged = query_bytes(&runtime);
                barrier.wait();
                for handle in handles {
                    handle.join().unwrap();
                }
                assert_eq!(admitted, permits);
                assert_eq!(charged, baseline + (permits * charge) as u64);
            });
            assert_eq!(query_bytes(&runtime), baseline);
            drop(held);
            assert_eq!(query_bytes(&runtime), 0);
        }
    }

    #[test]
    fn resident_hash_denial_reaches_select_wire_and_dml_without_changing_rows() {
        use crate::wire::query_direct::try_handle_query_binary_direct;
        let runtime = fixture();
        let before = runtime
            .execute_query("SELECT id, bucket FROM candidate_budget ORDER BY id")
            .unwrap();
        let held = leave_headroom(&runtime, 4096);
        let baseline = query_bytes(&runtime);
        for sql in [
            "SELECT id FROM candidate_budget WHERE bucket = 7",
            "UPDATE candidate_budget SET bucket = 8 WHERE bucket = 7",
            "DELETE FROM candidate_budget WHERE bucket = 7",
            "UPDATE candidate_budget SET bucket = 8 WHERE bucket IN (7, 8)",
            "DELETE FROM candidate_budget WHERE bucket IN (7, 8)",
        ] {
            let error = runtime.execute_query(sql).unwrap_err();
            // Autocommit UPDATE may use the snapshot-preserving source scan;
            // its mutation admission must still reject before publishing rows.
            let expected = if sql.starts_with("UPDATE") {
                "budget"
            } else {
                "index candidate memory budget"
            };
            assert!(error.to_string().contains(expected), "{sql}: {error}");
            assert_eq!(query_bytes(&runtime), baseline);
        }
        for sql in [
            "SELECT id FROM candidate_budget WHERE bucket = 7 LIMIT 1",
            "SELECT id FROM candidate_budget WHERE bucket IN (7, 8) LIMIT 1",
        ] {
            let response = try_handle_query_binary_direct(&runtime, sql)
                .expect("direct path must not fall back");
            assert_eq!(response[4], reddb_wire::legacy::MSG_ERROR);
            assert!(
                String::from_utf8_lossy(&response[5..]).contains("index candidate memory budget")
            );
            drop(response);
            assert_eq!(query_bytes(&runtime), baseline);
        }
        drop(held);
        let after = runtime
            .execute_query("SELECT id, bucket FROM candidate_budget ORDER BY id")
            .unwrap();
        assert_eq!(before.result.records.len(), 4096);
        assert_eq!(before.result.records.len(), after.result.records.len());
        for (before, after) in before.result.records.iter().zip(&after.result.records) {
            assert_eq!(before.get("id"), after.get("id"));
            assert_eq!(before.get("bucket"), after.get("bucket"));
        }
        for sql in [
            "SELECT id FROM candidate_budget WHERE bucket = 7 LIMIT 1",
            "SELECT id FROM candidate_budget WHERE bucket IN (7, 8) LIMIT 1",
        ] {
            let response = try_handle_query_binary_direct(&runtime, sql).unwrap();
            assert_eq!(response[4], reddb_wire::legacy::MSG_RESULT);
        }
        assert_eq!(query_bytes(&runtime), 0);
    }

    #[test]
    fn resident_hash_text_union_keeps_duplicates_and_does_not_retain_runtime() {
        use super::super::query_exec::try_hash_eq_lookup;
        use reddb_types::Value;
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(16 * 1024 * 1024),
        )
        .unwrap();
        for sql in [
            "CREATE TABLE candidate_budget (id INT, bucket TEXT)",
            "CREATE INDEX candidate_bucket ON candidate_budget (bucket) USING HASH",
            "INSERT INTO candidate_budget (id,bucket) VALUES (1,'alpha'),(2,'beta')",
        ] {
            runtime.execute_query(sql).unwrap();
        }
        let expected = ["beta", "alpha", "beta"]
            .into_iter()
            .flat_map(|key| {
                runtime
                    .index_store_ref()
                    .hash_lookup("candidate_budget", "candidate_bucket", key.as_bytes())
                    .unwrap()
                    .into_iter()
            })
            .collect::<Vec<_>>();
        let filter = in_filter(vec![
            Value::text("beta"),
            Value::text("alpha"),
            Value::text("beta"),
        ]);
        let mut candidates =
            try_hash_eq_lookup(&filter, "candidate_budget", runtime.index_store_ref())
                .unwrap()
                .unwrap();
        assert_eq!(candidates, expected);
        let charge = query_bytes(&runtime);
        candidates.truncate(1);
        assert_eq!(
            query_bytes(&runtime),
            charge,
            "truncate retains vector capacity"
        );
        let weak = std::sync::Arc::downgrade(&runtime.inner);
        let accounting = std::sync::Arc::clone(&runtime.inner.memory_accounting);
        drop(runtime);
        assert!(
            weak.upgrade().is_none(),
            "candidate owner must retain only accounting"
        );
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), charge);
        assert_eq!(candidates.into_iter().count(), 1);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), 0);
    }
}
