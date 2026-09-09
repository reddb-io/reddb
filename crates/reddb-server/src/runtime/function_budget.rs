//! Cooperative CALL budget. Only synchronous execution on the calling thread is covered.
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::storage::unified::entity::UnifiedEntity;
use crate::{RedDBError, RedDBResult};

#[derive(Clone, Copy)]
struct Budget {
    started: Instant,
    timeout: Duration,
    work_max: u64,
    remaining: u64,
    failure: Option<&'static str>,
}

thread_local! {
    static CURRENT: Cell<Option<Budget>> = const { Cell::new(None) };
}

/// Cannot move across threads: the synchronous runtime's scope and its cleanup
/// belong to the thread that installed it. Drop before transaction cleanup.
pub(super) struct Scope(PhantomData<Rc<()>>);

impl Scope {
    pub(super) fn enter(work_max: u64, timeout_ms: u64) -> RedDBResult<Self> {
        if work_max == 0 || timeout_ms == 0 {
            return Err(super::function_validation::error(
                "execution work_max and timeout_ms must be positive",
            ));
        }
        CURRENT.with(|slot| {
            assert!(
                slot.get().is_none(),
                "nested function budget is unsupported"
            );
            slot.set(Some(Budget {
                started: Instant::now(),
                timeout: Duration::from_millis(timeout_ms),
                work_max,
                remaining: work_max,
                failure: None,
            }));
        });
        Ok(Self(PhantomData))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.set(None));
    }
}

pub(crate) fn active() -> bool {
    CURRENT.with(|slot| slot.get().is_some())
}

/// Charge work before doing it. Zero forces a deadline check at a statement or
/// phase boundary. Row loops sample the clock every 256 units; no timer thread,
/// allocation, lock, or atomic operation is needed per unit.
pub(crate) fn charge(work: u64) -> RedDBResult<()> {
    CURRENT.with(|slot| {
        let Some(mut budget) = slot.get() else {
            return Ok(());
        };
        if budget.failure.is_none() {
            if work > budget.remaining {
                budget.failure = Some("work_max");
            } else {
                let consumed = budget.work_max - budget.remaining;
                if (work == 0 || work >= 256 || consumed % 256 + work >= 256)
                    && budget.started.elapsed() >= budget.timeout
                {
                    budget.failure = Some("timeout_ms");
                } else {
                    budget.remaining -= work;
                }
            }
        }
        slot.set(Some(budget));
        match budget.failure {
            Some(resource) => Err(RedDBError::Query(format!(
                "stored function: execution {resource} exceeded (work_max={}, timeout_ms={})",
                budget.work_max,
                budget.timeout.as_millis(),
            ))),
            None => Ok(()),
        }
    })
}

/// Adapt the storage visitor's stop flag to a query error. The caller receives
/// Err immediately after the scan stops, never a successful truncated result.
/// Ordinary queries take the original visitor without per-row budget checks.
pub(crate) fn scan<T>(
    visit: impl FnOnce(&mut dyn FnMut(&UnifiedEntity) -> bool) -> T,
    mut consume: impl FnMut(&UnifiedEntity) -> bool,
) -> RedDBResult<T> {
    if !active() {
        return Ok(visit(&mut consume));
    }
    charge(0)?;
    let mut failure = None;
    let result = visit(&mut |entity| {
        if let Err(cause) = charge(1) {
            failure = Some(cause);
            return false;
        }
        consume(entity)
    });
    if let Some(cause) = failure {
        return Err(cause);
    }
    charge(0)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_work_boundary_is_sticky_and_scope_does_not_leak() {
        let scope = Scope::enter(3, 60_000).expect("budget");
        charge(1).expect("first statement");
        charge(2).expect("second statement consumes remaining work");
        charge(0).expect("exact boundary");
        assert!(charge(1)
            .expect_err("exhausted")
            .to_string()
            .contains("work_max"));
        assert!(
            charge(0).is_err(),
            "exhaustion cannot be reset by a later phase"
        );
        drop(scope);
        charge(u64::MAX).expect("cleanup is unbudgeted");
        let _next = Scope::enter(1, 60_000).expect("new call");
        charge(1).expect("fresh budget");
    }

    #[test]
    fn deadline_is_observed_and_scope_is_thread_local() {
        let _scope = Scope::enter(1_000, 1).expect("budget");
        CURRENT.with(|slot| {
            let mut budget = slot.get().expect("active budget");
            budget.started = Instant::now() - Duration::from_secs(1);
            slot.set(Some(budget));
        });
        assert!(charge(0)
            .expect_err("deadline")
            .to_string()
            .contains("timeout_ms"));
        std::thread::spawn(|| {
            assert!(!active());
            charge(u64::MAX).expect("other connection thread");
        })
        .join()
        .expect("worker");
    }

    #[test]
    fn charged_loop_observes_deadline_without_a_statement_boundary() {
        let _scope = Scope::enter(1_000, 1).expect("budget");
        CURRENT.with(|slot| {
            let mut budget = slot.get().expect("active budget");
            budget.started = Instant::now() - Duration::from_secs(1);
            slot.set(Some(budget));
        });
        for _ in 0..255 {
            charge(1).expect("clock sample is deferred");
        }
        assert!(charge(1)
            .expect_err("periodic check")
            .to_string()
            .contains("timeout_ms"));
    }

    #[test]
    fn scan_stops_before_consuming_over_budget_entity() {
        let _scope = Scope::enter(3, 60_000).expect("budget");
        let entity = UnifiedEntity::table_row(
            crate::storage::unified::entity::EntityId::new(1),
            "t",
            1,
            vec![],
        );
        let mut consumed = 0;
        let result = scan(
            |visit| {
                for _ in 0..100 {
                    if !visit(&entity) {
                        break;
                    }
                }
            },
            |_| {
                consumed += 1;
                true
            },
        );
        assert!(result.is_err());
        assert_eq!(consumed, 3);
    }
}
