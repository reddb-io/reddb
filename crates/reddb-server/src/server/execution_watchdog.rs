use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

const RUNNING: u8 = 0;
const TIMED_OUT: u8 = 1;
const COMPLETED: u8 = 2;

/// Tracks handler work that exceeded its public deadline but is still running
/// on the blocking executor. Readiness consults only the aggregate atomic, so
/// Kubernetes probes never contend with storage or add synthetic queries.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExecutionWatchdog {
    timed_out_inflight: Arc<AtomicUsize>,
}

impl ExecutionWatchdog {
    pub(crate) fn start(&self) -> Arc<ExecutionTicket> {
        Arc::new(ExecutionTicket {
            state: AtomicU8::new(RUNNING),
            timed_out_inflight: Arc::clone(&self.timed_out_inflight),
        })
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.timed_out_inflight.load(Ordering::Acquire) == 0
    }
}

#[derive(Debug)]
pub(crate) struct ExecutionTicket {
    state: AtomicU8,
    timed_out_inflight: Arc<AtomicUsize>,
}

impl ExecutionTicket {
    /// Returns a worker-owned guard whose drop records completion even when
    /// routing unwinds. The async edge retains the ticket to mark its deadline.
    pub(crate) fn completion(self: &Arc<Self>) -> ExecutionCompletion {
        ExecutionCompletion(Arc::clone(self))
    }

    pub(crate) fn mark_timed_out(&self) {
        // Reserve the aggregate slot before publishing TIMED_OUT. Otherwise a
        // concurrent completion can observe TIMED_OUT and decrement the
        // aggregate before this thread increments it.
        self.timed_out_inflight.fetch_add(1, Ordering::AcqRel);
        if self
            .state
            .compare_exchange(RUNNING, TIMED_OUT, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.timed_out_inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn mark_completed(&self) {
        if self.state.swap(COMPLETED, Ordering::AcqRel) == TIMED_OUT {
            let previous = self.timed_out_inflight.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "timed-out execution accounting underflow");
        }
    }
}

pub(crate) struct ExecutionCompletion(Arc<ExecutionTicket>);

impl Drop for ExecutionCompletion {
    fn drop(&mut self) {
        self.0.mark_completed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_degrades_until_that_worker_completes() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let completion = ticket.completion();

        ticket.mark_timed_out();
        assert!(!watchdog.is_ready());

        drop(completion);
        assert!(watchdog.is_ready());
    }

    #[test]
    fn completion_wins_a_race_with_a_late_timeout() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        drop(ticket.completion());

        ticket.mark_timed_out();
        assert!(watchdog.is_ready());
    }

    #[test]
    fn every_timed_out_worker_must_finish_before_readiness_recovers() {
        let watchdog = ExecutionWatchdog::default();
        let first = watchdog.start();
        let second = watchdog.start();
        let first_completion = first.completion();
        let second_completion = second.completion();
        first.mark_timed_out();
        second.mark_timed_out();

        drop(first_completion);
        assert!(!watchdog.is_ready());
        drop(second_completion);
        assert!(watchdog.is_ready());
    }
}
