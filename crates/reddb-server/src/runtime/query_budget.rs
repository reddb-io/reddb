//! Cooperative budgets for materialized read execution. Blocking storage calls,
//! parsing and operators without work checkpoints are not preempted by a timer.
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
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
        if duration_max.is_none() && concurrent_max.is_none() && output_max.is_none() {
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
    for record in &result.result.records {
        let value = crate::presentation::query_result_json::unified_record_json(
            record,
            &result.result.columns,
        );
        let encoded = value
            .to_string_compact_limited(limit.saturating_sub(used))
            .ok_or_else(|| result_limit_error(maximum))?;
        used = used.saturating_add(encoded.len());
    }
    crate::presentation::query_result_json::runtime_query_json(result, &None, &None)
        .to_string_compact_limited(limit)
        .ok_or_else(|| result_limit_error(maximum))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let size = crate::presentation::query_result_json::runtime_query_json(&result, &None, &None)
            .to_string_compact()
            .len() as u64;
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
}
