//! Cooperative query budgets and transport cancellation checkpoints. Blocking
//! work without checkpoints is not preempted. DML observes cancellation while
//! scanning and preparing, then defers it after durable publication begins.
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::resource_limits::ResourceLimits;
use super::RuntimeQueryResult;
use crate::{RedDBError, RedDBResult};

#[derive(Clone, Copy)]
struct Budget {
    started: Instant,
    duration_max: Option<Duration>,
    work: u64,
    failed: bool,
    output_max: Option<u64>,
    output_bytes: u64,
}
thread_local! { static CURRENT: Cell<Option<Budget>> = const { Cell::new(None) }; }
thread_local! {
    static REQUEST_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}
thread_local! { static DURABLE_PUBLICATION_DEPTH: Cell<u32> = const { Cell::new(0) }; }

/// Cancellation shared by an asynchronous transport deadline and the
/// synchronous query worker it spawned. The worker installs a scope before
/// dispatch; materialized-read checkpoints then observe cancellation even
/// when no operator duration limit was configured.
#[derive(Clone, Debug, Default)]
pub(crate) struct RequestCancellation {
    cancelled: Arc<AtomicBool>,
}

impl RequestCancellation {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn install(&self) -> RequestCancellationScope {
        let previous =
            REQUEST_CANCEL.with(|slot| slot.borrow_mut().replace(Arc::clone(&self.cancelled)));
        RequestCancellationScope {
            previous,
            _thread: PhantomData,
        }
    }
}

/// Thread-affine guard keeping the shared cancellation allocation installed in
/// the worker's request context and restoring any enclosing context on drop.
pub(crate) struct RequestCancellationScope {
    previous: Option<Arc<AtomicBool>>,
    _thread: PhantomData<Rc<()>>,
}

/// Defers cooperative cancellation after a mutation crosses its final durable
/// publication boundary. Returning a timeout from inside a multi-chunk publish
/// could otherwise expose only a prefix of the statement.
pub(crate) struct DurablePublicationScope {
    _thread: PhantomData<Rc<()>>,
}

impl Drop for DurablePublicationScope {
    fn drop(&mut self) {
        DURABLE_PUBLICATION_DEPTH.with(|depth| {
            depth.set(
                depth
                    .get()
                    .checked_sub(1)
                    .expect("durable publication scope balanced"),
            );
        });
    }
}

/// Establish the last cancellation checkpoint before durable mutation work.
/// Once this succeeds, callers must finish publication atomically even if the
/// transport deadline fires while storage or index maintenance is in flight.
pub(crate) fn begin_durable_publication() -> RedDBResult<DurablePublicationScope> {
    charge(0)?;
    DURABLE_PUBLICATION_DEPTH.with(|depth| {
        depth.set(
            depth
                .get()
                .checked_add(1)
                .expect("durable publication scope depth"),
        );
    });
    Ok(DurablePublicationScope {
        _thread: PhantomData,
    })
}

fn durable_publication_active() -> bool {
    DURABLE_PUBLICATION_DEPTH.with(|depth| depth.get() > 0)
}

impl Drop for RequestCancellationScope {
    fn drop(&mut self) {
        REQUEST_CANCEL.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

fn request_cancellation_installed() -> bool {
    REQUEST_CANCEL.with(|slot| slot.borrow().is_some())
}

fn request_cancelled() -> bool {
    REQUEST_CANCEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
    })
}

pub(crate) struct Scope<'a> {
    owns: bool,
    slot: Option<&'a AtomicU64>,
    _thread: PhantomData<Rc<()>>,
}

impl<'a> Scope<'a> {
    pub(crate) fn enter(limits: &ResourceLimits, slots: &'a AtomicU64) -> RedDBResult<Self> {
        let mut scope = Self {
            owns: false,
            slot: None,
            _thread: PhantomData,
        };
        if active() {
            return Ok(scope);
        }
        let duration_max = limits.max_query_duration.filter(|value| !value.is_zero());
        let concurrent_max = limits.max_concurrent_queries.filter(|value| *value > 0);
        let output_max = limits.max_result_bytes.filter(|value| *value > 0);
        if duration_max.is_none()
            && concurrent_max.is_none()
            && output_max.is_none()
            && !request_cancellation_installed()
        {
            return Ok(scope);
        }
        if let Some(maximum) = concurrent_max {
            slots
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
                    (current < maximum).then(|| current + 1)
                })
                .map_err(|current| {
                    RedDBError::QuotaExceeded(format!("rate:query_concurrency:{current}:{maximum}"))
                })?;
            scope.slot = Some(slots);
        }
        CURRENT.with(|slot| {
            slot.set(Some(Budget {
                started: Instant::now(),
                duration_max,
                work: 0,
                failed: false,
                output_max,
                output_bytes: 0,
            }))
        });
        scope.owns = true;
        Ok(scope)
    }
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if self.owns {
            CURRENT.with(|slot| slot.set(None));
        }
        if let Some(slot) = self.slot {
            slot.fetch_sub(1, Ordering::Release);
        }
    }
}

pub(crate) fn active() -> bool {
    CURRENT.with(|slot| slot.get().is_some())
}
pub(crate) fn is_materialized_read(expr: &reddb_rql::ast::QueryExpr) -> bool {
    use reddb_rql::ast::QueryExpr;
    matches!(
        expr,
        QueryExpr::Table(_)
            | QueryExpr::Graph(_)
            | QueryExpr::Join(_)
            | QueryExpr::Path(_)
            | QueryExpr::Vector(_)
            | QueryExpr::Hybrid(_)
    )
}
pub(crate) fn charge_output(bytes: usize) -> RedDBResult<()> {
    charge(0)?;
    CURRENT.with(|slot| {
        let Some(mut budget) = slot.get() else {
            return Ok(());
        };
        budget.output_bytes = budget
            .output_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        slot.set(Some(budget));
        if budget
            .output_max
            .is_some_and(|maximum| budget.output_bytes > maximum)
        {
            return Err(result_limit_error(
                budget.output_max.expect("configured output limit"),
            ));
        }
        Ok(())
    })
}
pub(crate) fn charge(work: u64) -> RedDBResult<()> {
    if durable_publication_active() {
        return Ok(());
    }
    if request_cancelled() {
        return Err(RedDBError::QuotaExceeded(
            "duration:query_cancelled:request_deadline".to_string(),
        ));
    }
    CURRENT.with(|slot| {
        let Some(mut budget) = slot.get() else {
            return Ok(());
        };
        let check = work == 0 || work >= 256 || budget.work % 256 + work >= 256;
        budget.work = budget.work.saturating_add(work);
        if check
            && budget
                .duration_max
                .is_some_and(|maximum| budget.started.elapsed() >= maximum)
        {
            budget.failed = true;
        }
        slot.set(Some(budget));
        if budget.failed {
            Err(RedDBError::QuotaExceeded(format!(
                "duration:query_ms:{}:{}",
                budget.started.elapsed().as_millis(),
                budget.duration_max.map_or(0, |value| value.as_millis())
            )))
        } else {
            Ok(())
        }
    })
}

fn result_limit_error(maximum: u64) -> RedDBError {
    RedDBError::QuotaExceeded(format!(
        "payload:result_bytes:{}:{maximum}",
        maximum.saturating_add(1)
    ))
}

/// Account rows individually before constructing a complete JSON envelope.
/// This bounds serialization buffers, not every executor's intermediate heap.
pub(super) fn check_result(
    limits: &ResourceLimits,
    result: &RuntimeQueryResult,
) -> RedDBResult<()> {
    let Some(maximum) = limits.max_result_bytes.filter(|value| *value > 0) else {
        return Ok(());
    };
    if result.statement_type != "select" {
        return Ok(());
    }
    let limit = usize::try_from(maximum).unwrap_or(usize::MAX);
    let mut used = result.query.len();
    for column in &result.result.columns {
        used = used.saturating_add(column.len());
    }
    if used > limit
        || result
            .result
            .pre_serialized_json
            .as_ref()
            .is_some_and(|value| value.len() > limit)
    {
        return Err(result_limit_error(maximum));
    }
    let mut used = empty_payload(result)
        .to_string_compact_limited(limit)
        .ok_or_else(|| result_limit_error(maximum))?
        .len();
    for (index, record) in result.result.records.iter().enumerate() {
        used = used.saturating_add(usize::from(index > 0));
        let value =
            crate::query_payload_render::unified_record_json(record, &result.result.columns);
        let encoded = value
            .to_string_compact_limited(limit.saturating_sub(used))
            .ok_or_else(|| result_limit_error(maximum))?;
        used = used.saturating_add(encoded.len());
    }
    Ok(())
}

// Transport-neutral core: presentation adds its own bounded descriptor/envelope.
fn empty_payload(result: &RuntimeQueryResult) -> crate::json::Value {
    crate::json!({
        "query": result.query.clone(),
        "columns": crate::json::Value::Array(result.result.columns.iter().cloned().map(crate::json::Value::String).collect()),
        "records": crate::json::Value::Array(Vec::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::index_store::MutationTestPhase;

    #[test]
    fn text_parameterized_and_prepared_reads_share_admission_and_release_it() {
        let runtime = super::super::RedDBRuntime::with_pool_and_limits(
            crate::RedDBOptions::in_memory(),
            super::super::ConnectionPoolConfig::default(),
            ResourceLimits {
                max_concurrent_queries: Some(1),
                ..Default::default()
            },
        )
        .expect("runtime");
        runtime
            .execute_query("CREATE TABLE budget_reads (id INTEGER)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO budget_reads (id) VALUES (1)")
            .expect("row");
        runtime.inner.query_budget_slots.store(1, Ordering::Relaxed);
        let sql = "SELECT id FROM budget_reads";
        assert!(matches!(
            runtime.execute_query(sql),
            Err(RedDBError::QuotaExceeded(_))
        ));
        assert!(matches!(
            runtime.execute_query_with_params(
                "SELECT id FROM budget_reads WHERE id = $1",
                &[reddb_types::Value::Integer(1)]
            ),
            Err(RedDBError::QuotaExceeded(_))
        ));
        let expr = reddb_rql::modes::parse_multi(sql).expect("read AST");
        assert!(matches!(
            runtime.execute_prepared_query(sql, expr),
            Err(RedDBError::QuotaExceeded(_))
        ));
        assert!(runtime
            .execute_query("INSERT INTO budget_reads (id) VALUES (2)")
            .is_ok());
        runtime.inner.query_budget_slots.store(0, Ordering::Relaxed);
        assert!(runtime.execute_query(sql).is_ok());
        assert_eq!(runtime.inner.query_budget_slots.load(Ordering::Relaxed), 0);
        assert!(!active());
    }

    #[test]
    fn canonical_result_limit_honors_exact_boundary_and_zero_disables_it() {
        let result = RuntimeQueryResult::ok_message(
            "SELECT message".to_string(),
            &"é\n".repeat(400),
            "select",
        );
        let size = (empty_payload(&result).to_string_compact().len()
            + result
                .result
                .records
                .iter()
                .map(|record| {
                    crate::query_payload_render::unified_record_json(record, &result.result.columns)
                        .to_string_compact()
                        .len()
                })
                .sum::<usize>()
            + result.result.records.len().saturating_sub(1)) as u64;
        let limits = |maximum| ResourceLimits {
            max_result_bytes: Some(maximum),
            ..Default::default()
        };
        assert!(check_result(&limits(size), &result).is_ok());
        assert!(check_result(&limits(size - 1), &result).is_err());
        assert!(check_result(&limits(64), &result).is_err());
        assert!(check_result(&limits(0), &result).is_ok());
    }

    #[test]
    fn output_limit_does_not_change_committed_mutation_acknowledgements() {
        let result = RuntimeQueryResult::ok_message(
            "CREATE TABLE t (id INTEGER)".to_string(),
            "created",
            "create",
        );
        let limits = ResourceLimits {
            max_result_bytes: Some(1),
            ..Default::default()
        };
        assert!(check_result(&limits, &result).is_ok());
    }

    #[test]
    fn deadline_failure_is_sticky_and_does_not_leak_after_scope_drop() {
        let limits = ResourceLimits {
            max_query_duration: Some(Duration::from_millis(1)),
            ..Default::default()
        };
        let slots = AtomicU64::new(0);
        let scope = Scope::enter(&limits, &slots).expect("read budget");
        CURRENT.with(|slot| {
            let mut budget = slot.get().expect("installed budget");
            budget.started = Instant::now() - Duration::from_secs(1);
            budget.work = 255;
            slot.set(Some(budget));
        });
        assert!(charge(1).is_err());
        assert!(charge(0).is_err());
        assert!(std::thread::spawn(|| charge(0))
            .join()
            .expect("other thread")
            .is_ok());
        drop(scope);
        assert!(charge(0).is_ok());
        assert!(!active());
    }

    #[test]
    fn read_admission_is_shared_across_threads_and_nested_reads_reuse_the_slot() {
        let limits = ResourceLimits {
            max_concurrent_queries: Some(1),
            ..Default::default()
        };
        let slots = AtomicU64::new(0);
        let outer = Scope::enter(&limits, &slots).expect("first read");
        let nested = Scope::enter(&limits, &slots).expect("nested read");
        assert_eq!(slots.load(Ordering::Relaxed), 1);
        std::thread::scope(|threads| {
            threads.spawn(|| assert!(Scope::enter(&limits, &slots).is_err()));
        });
        drop(nested);
        assert_eq!(slots.load(Ordering::Relaxed), 1);
        drop(outer);
        assert_eq!(slots.load(Ordering::Relaxed), 0);
        assert!(Scope::enter(&limits, &slots).is_ok());
    }

    #[test]
    fn streamed_payload_is_cumulative_and_failed_budget_never_accepts_an_end_frame() {
        let limits = ResourceLimits {
            max_result_bytes: Some(64),
            ..Default::default()
        };
        let slots = AtomicU64::new(0);
        let scope = Scope::enter(&limits, &slots).expect("stream budget");
        assert!(charge_output(32).is_ok());
        assert!(charge_output(32).is_ok());
        assert!(charge_output(1).is_err());
        assert!(charge_output(0).is_err());
        drop(scope);
        assert!(charge_output(1000).is_ok());
    }

    #[test]
    fn zero_settings_do_not_install_a_budget() {
        let limits = ResourceLimits {
            max_query_duration: Some(Duration::ZERO),
            max_result_bytes: Some(0),
            max_concurrent_queries: Some(0),
            ..Default::default()
        };
        let slots = AtomicU64::new(0);
        let _scope = Scope::enter(&limits, &slots).expect("unbounded settings");
        assert!(!active());
        assert!(charge_output(usize::MAX).is_ok());
        assert_eq!(slots.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn request_deadline_cancels_worker_and_releases_query_admission() {
        let limits = ResourceLimits {
            max_concurrent_queries: Some(1),
            ..Default::default()
        };
        let slots = AtomicU64::new(0);
        let cancellation = RequestCancellation::new();
        std::thread::scope(|threads| {
            let worker_cancellation = cancellation.clone();
            let worker_limits = &limits;
            let worker_slots = &slots;
            let worker = threads.spawn(move || {
                let _request = worker_cancellation.install();
                let _query = Scope::enter(worker_limits, worker_slots).expect("query admitted");
                while charge(1).is_ok() {
                    std::thread::yield_now();
                }
            });
            while slots.load(Ordering::Acquire) != 1 {
                std::thread::yield_now();
            }
            cancellation.cancel();
            worker.join().expect("cancelled worker exits");
        });
        assert_eq!(slots.load(Ordering::Acquire), 0);
        assert!(Scope::enter(&limits, &slots).is_ok());
    }

    #[test]
    fn request_cancellation_scope_restores_nested_context() {
        let outer = RequestCancellation::new();
        let inner = RequestCancellation::new();
        let _outer_scope = outer.install();
        outer.cancel();
        assert!(charge(0).is_err());
        {
            let _inner_scope = inner.install();
            assert!(charge(0).is_ok());
        }
        assert!(charge(0).is_err());
    }

    fn assert_request_deadline_cancels_mutation_before_publication(
        query: &'static str,
        pause_phase: MutationTestPhase,
    ) {
        use std::sync::mpsc;

        let runtime = super::super::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE cancellation_rows (id INTEGER, state TEXT)")
            .expect("table");
        runtime
            .execute_query(
                "INSERT INTO cancellation_rows (id, state) VALUES (1, 'original'), (2, 'original')",
            )
            .expect("rows");

        let (candidate_send, candidate_receive) = mpsc::channel();
        let (resume_send, resume_receive) = mpsc::channel();
        let resume_receive = std::sync::Mutex::new(resume_receive);
        let first_candidate = Arc::new(AtomicBool::new(true));
        let hook_first_candidate = Arc::clone(&first_candidate);
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection == "cancellation_rows"
                    && phase == pause_phase
                    && hook_first_candidate.swap(false, Ordering::AcqRel)
                {
                    candidate_send.send(()).expect("candidate reached");
                    resume_receive
                        .lock()
                        .expect("resume receiver")
                        .recv()
                        .expect("resume scan");
                }
            }));

        let cancellation = RequestCancellation::new();
        std::thread::scope(|threads| {
            let worker_cancellation = cancellation.clone();
            let runtime = &runtime;
            let worker = threads.spawn(move || {
                let _request = worker_cancellation.install();
                runtime.execute_query(query)
            });
            if let Err(error) = candidate_receive.recv_timeout(Duration::from_secs(2)) {
                let _ = resume_send.send(());
                panic!("mutation did not reach cancellation checkpoint: {error}");
            }
            cancellation.cancel();
            resume_send.send(()).expect("resume cancelled scan");
            let error = worker
                .join()
                .expect("mutation worker")
                .expect_err("cancelled mutation must not succeed");
            assert!(
                error.to_string().contains("query_cancelled"),
                "unexpected cancellation error: {error}"
            );
        });

        let result = runtime
            .execute_query("SELECT state FROM cancellation_rows ORDER BY id")
            .expect("read unchanged rows");
        assert_eq!(result.result.records.len(), 2);
        assert!(result
            .result
            .records
            .iter()
            .all(|record| { record.get("state") == Some(&reddb_types::Value::text("original")) }));
    }

    #[test]
    fn request_deadline_cancels_update_during_target_scan_before_publication() {
        assert_request_deadline_cancels_mutation_before_publication(
            "UPDATE cancellation_rows SET state = 'changed'",
            MutationTestPhase::DmlTargetScanCandidate,
        );
    }

    #[test]
    fn request_deadline_cancels_update_during_post_image_preparation_before_publication() {
        assert_request_deadline_cancels_mutation_before_publication(
            "UPDATE cancellation_rows SET state = 'changed'",
            MutationTestPhase::DmlPostImageCandidate,
        );
    }

    #[test]
    fn request_deadline_cancels_delete_during_target_scan_before_publication() {
        assert_request_deadline_cancels_mutation_before_publication(
            "DELETE FROM cancellation_rows",
            MutationTestPhase::DmlTargetScanCandidate,
        );
    }

    #[test]
    fn request_deadline_after_durable_delete_publication_finishes_statement() {
        let runtime = super::super::RedDBRuntime::with_options(
            crate::RedDBOptions::in_memory().with_memory_budget(128 * 1024 * 1024),
        )
        .expect("runtime");
        runtime
            .execute_query("CREATE TABLE cancellation_rows (id INTEGER)")
            .expect("table");
        let count = super::super::impl_dml::UPDATE_APPLY_CHUNK_SIZE + 16;
        for batch in (0..count).collect::<Vec<_>>().chunks(512) {
            let values = batch
                .iter()
                .map(|id| format!("({id})"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO cancellation_rows (id) VALUES {values}"
                ))
                .expect("seed rows");
        }

        let cancellation = RequestCancellation::new();
        let hook_cancellation = cancellation.clone();
        let first_publication = Arc::new(AtomicBool::new(true));
        let hook_first_publication = Arc::clone(&first_publication);
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection == "cancellation_rows"
                    && phase == MutationTestPhase::StoragePublished
                    && hook_first_publication.swap(false, Ordering::AcqRel)
                {
                    hook_cancellation.cancel();
                }
            }));

        let result = {
            let _request = cancellation.install();
            let result = runtime
                .execute_query("DELETE FROM cancellation_rows")
                .expect("durable publication must finish");
            assert!(
                charge(0).is_err(),
                "cancellation must resume after durable publication"
            );
            result
        };
        assert_eq!(
            result.affected_rows,
            u64::try_from(count).expect("small fixture")
        );
        assert!(
            runtime
                .execute_query("SELECT id FROM cancellation_rows")
                .expect("read deleted rows")
                .result
                .records
                .is_empty(),
            "deadline after publication must not leave a deleted prefix"
        );
    }

    #[test]
    fn request_deadline_after_durable_update_publication_finishes_maintenance() {
        let runtime = super::super::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE cancellation_rows (id INTEGER, state TEXT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO cancellation_rows (id, state) VALUES (1, 'original')")
            .expect("row");

        let cancellation = RequestCancellation::new();
        let hook_cancellation = cancellation.clone();
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection != "cancellation_rows" {
                    return;
                }
                if phase == MutationTestPhase::StoragePublished {
                    hook_cancellation.cancel();
                }
                if phase == MutationTestPhase::BeforeEvents {
                    assert!(
                        charge(0).is_ok(),
                        "index and event maintenance must remain inside durable publication"
                    );
                }
            }));

        {
            let _request = cancellation.install();
            let result = runtime
                .execute_query("UPDATE cancellation_rows SET state = 'changed'")
                .expect("durable publication must finish maintenance");
            assert_eq!(result.affected_rows, 1);
            assert!(
                charge(0).is_err(),
                "cancellation must resume after durable publication"
            );
        }
        let result = runtime
            .execute_query("SELECT state FROM cancellation_rows")
            .expect("read updated row");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("state"),
            Some(&reddb_types::Value::text("changed"))
        );
    }
}
