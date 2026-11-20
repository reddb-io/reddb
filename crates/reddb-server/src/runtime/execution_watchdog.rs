use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Ticket state word: the low two bits hold the phase, the remaining bits hold
// the start of the current running span in microseconds since the ticket's
// origin. Packing both into one atomic lets the deadline marker decide
// "running and overdue" and publish TIMED_OUT in a single compare-exchange,
// so a park/resume that races the decision can never be mistaken for the
// span the marker measured.
const PHASE_MASK: u64 = 0b11;
const SPAN_SHIFT: u32 = 2;
const RUNNING: u64 = 0;
const TIMED_OUT: u64 = 1;
const COMPLETED: u64 = 2;
/// The worker is intentionally parked (e.g. `QUEUE READ … WAIT` on an empty
/// queue). A parked execution is idle by design, not wedged: the deadline does
/// not run while parked and restarts from zero when the worker resumes.
const PARKED: u64 = 3;

/// How often the deadline marker re-checks a parked ticket. Bounded below so
/// a zero `execution_queue_timeout` cannot spin the marker while parked.
const MIN_PARKED_POLL: Duration = Duration::from_millis(10);
/// How often a still-running overdue execution is named again in the log.
const OVERDUE_REPORT_INTERVAL: Duration = Duration::from_secs(60);
const OVERDUE_LABEL_BYTES_MAX: usize = 400;

thread_local! {
    static CURRENT_TICKET: RefCell<Option<Arc<ExecutionTicket>>> = const { RefCell::new(None) };
    /// Ticket that only receives the statement label. The HTTP edge owns its
    /// own deadline timer and must not expose its ticket to parking.
    static LABEL_TICKET: RefCell<Option<Arc<ExecutionTicket>>> = const { RefCell::new(None) };
}

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
            state: AtomicU64::new(RUNNING),
            origin: Instant::now(),
            timed_out_inflight: Arc::clone(&self.timed_out_inflight),
            label: std::sync::Mutex::new(None),
            origin_label: std::sync::Mutex::new(None),
        })
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.timed_out_inflight.load(Ordering::Acquire) == 0
    }
}

/// Outcome of one deadline check by the marker task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadlineCheck {
    /// This check published TIMED_OUT and degraded readiness.
    TimedOut,
    /// The ticket already completed or timed out; nothing left to watch.
    Settled,
    /// The worker is parked; its deadline is suspended.
    Parked,
    /// Still within the deadline; re-check after the remaining duration.
    Pending(Duration),
}

#[derive(Debug)]
pub(crate) struct ExecutionTicket {
    state: AtomicU64,
    origin: Instant,
    timed_out_inflight: Arc<AtomicUsize>,
    /// What this execution is running, for the overdue diagnostics only.
    label: std::sync::Mutex<Option<String>>,
    /// Where it came from (e.g. `HTTP POST /query`), same purpose.
    origin_label: std::sync::Mutex<Option<String>>,
}

fn phase(word: u64) -> u64 {
    word & PHASE_MASK
}

fn with_phase(word: u64, phase: u64) -> u64 {
    (word & !PHASE_MASK) | phase
}

impl ExecutionTicket {
    fn describe(&self) -> String {
        let read = |slot: &std::sync::Mutex<Option<String>>| {
            slot.lock().ok().and_then(|label| label.clone())
        };
        match (read(&self.origin_label), read(&self.label)) {
            (Some(origin), Some(statement)) => format!("{origin} :: {statement}"),
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => "<unlabelled execution>".to_string(),
        }
    }

    /// Records where this execution came from.
    pub(crate) fn set_origin(&self, origin: String) {
        if let Ok(mut slot) = self.origin_label.lock() {
            *slot = Some(bounded_label(origin));
        }
    }

    /// Lets engine code on this thread name the statement of this execution
    /// without making it parkable. Restores the enclosing ticket on drop.
    pub(crate) fn label_scope(self: &Arc<Self>) -> ExecutionLabelScope {
        let previous = LABEL_TICKET.with(|slot| slot.borrow_mut().replace(Arc::clone(self)));
        ExecutionLabelScope {
            previous,
            _thread: PhantomData,
        }
    }

    /// Keeps naming an overdue execution until it settles. No-op outside a
    /// Tokio runtime.
    pub(crate) fn report_until_settled(self: &Arc<Self>) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(report_while_overdue(Arc::clone(self)));
        }
    }

    /// A parked server must say what it is waiting for: an idle process with
    /// readiness degraded is otherwise indistinguishable from a deadlock.
    fn report_overdue(&self) {
        tracing::warn!(
            target: "reddb::watchdog",
            statement = %self.describe(),
            running_secs = self.origin.elapsed().as_secs(),
            "execution exceeded its deadline and is still running; readiness is degraded until it finishes"
        );
    }

    fn is_overdue(&self) -> bool {
        phase(self.state.load(Ordering::Acquire)) == TIMED_OUT
    }

    /// Returns a worker-owned guard whose drop records completion even when
    /// routing unwinds. The async edge retains the ticket to mark its deadline.
    pub(crate) fn completion(self: &Arc<Self>) -> ExecutionCompletion {
        ExecutionCompletion(Arc::clone(self))
    }

    /// Installs this ticket as the current thread's execution so engine code
    /// that parks intentionally (see [`park_current_execution`]) can suspend
    /// the deadline. The scope restores any enclosing ticket on drop.
    pub(crate) fn install(self: &Arc<Self>) -> ExecutionTicketScope {
        let previous = CURRENT_TICKET.with(|slot| slot.borrow_mut().replace(Arc::clone(self)));
        ExecutionTicketScope {
            previous,
            _thread: PhantomData,
        }
    }

    /// Unconditionally declares a running execution timed out (the HTTP edge
    /// owns its own deadline timer). A parked execution is not wedged and is
    /// left alone; a completed one wins the race.
    pub(crate) fn mark_timed_out(&self) -> bool {
        // Reserve the aggregate slot before publishing TIMED_OUT. Otherwise a
        // concurrent completion can observe TIMED_OUT and decrement the
        // aggregate before this thread increments it.
        self.timed_out_inflight.fetch_add(1, Ordering::AcqRel);
        let mut word = self.state.load(Ordering::Acquire);
        loop {
            if phase(word) != RUNNING {
                self.timed_out_inflight.fetch_sub(1, Ordering::AcqRel);
                return false;
            }
            match self.state.compare_exchange_weak(
                word,
                with_phase(word, TIMED_OUT),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.report_overdue();
                    return true;
                }
                Err(current) => word = current,
            }
        }
    }

    /// Marks the ticket timed out only when its *current running span* has
    /// lasted at least `deadline`. Time spent parked never counts: resuming
    /// starts a fresh span, so the deadline restarts when work resumes.
    pub(crate) fn mark_timed_out_if_overdue(&self, deadline: Duration) -> DeadlineCheck {
        loop {
            let word = self.state.load(Ordering::Acquire);
            match phase(word) {
                TIMED_OUT | COMPLETED => return DeadlineCheck::Settled,
                PARKED => return DeadlineCheck::Parked,
                _ => {}
            }
            let span_start = Duration::from_micros(word >> SPAN_SHIFT);
            let running_for = self.origin.elapsed().saturating_sub(span_start);
            if running_for < deadline {
                return DeadlineCheck::Pending(deadline - running_for);
            }
            self.timed_out_inflight.fetch_add(1, Ordering::AcqRel);
            if self
                .state
                .compare_exchange(
                    word,
                    with_phase(word, TIMED_OUT),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return DeadlineCheck::TimedOut;
            }
            // The worker parked, resumed or completed between the read and
            // the publish: release the reservation and judge the new state.
            self.timed_out_inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// RUNNING -> PARKED. A ticket that already timed out stays timed out:
    /// only completion recovers readiness for an execution declared wedged.
    fn park(&self) -> bool {
        let mut word = self.state.load(Ordering::Acquire);
        loop {
            if phase(word) != RUNNING {
                return false;
            }
            match self.state.compare_exchange_weak(
                word,
                with_phase(word, PARKED),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => word = current,
            }
        }
    }

    /// PARKED -> RUNNING with a fresh span starting now.
    fn resume(&self) {
        let now_us = u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX >> 2);
        let resumed = (now_us << SPAN_SHIFT) | RUNNING;
        let mut word = self.state.load(Ordering::Acquire);
        while phase(word) == PARKED {
            match self.state.compare_exchange_weak(
                word,
                resumed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => word = current,
            }
        }
    }

    fn mark_completed(&self) {
        let previous = self.state.swap(COMPLETED, Ordering::AcqRel);
        if phase(previous) == TIMED_OUT {
            let previous = self.timed_out_inflight.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "timed-out execution accounting underflow");
            tracing::warn!(
                target: "reddb::watchdog",
                statement = %self.describe(),
                ran_secs = self.origin.elapsed().as_secs(),
                "overdue execution finished"
            );
        }
    }
}

/// Deadline marker for one execution. Runs on its own task so it degrades
/// readiness the moment a running span overruns `deadline`, while a parked
/// span (an intentional long-poll) suspends the deadline instead of tripping
/// it. Returns once the ticket times out or settles.
pub(crate) async fn enforce_deadline(ticket: Arc<ExecutionTicket>, deadline: Duration) {
    loop {
        match ticket.mark_timed_out_if_overdue(deadline) {
            DeadlineCheck::TimedOut => {
                ticket.report_overdue();
                // Keep naming it for as long as it stays stuck; the reporter
                // ends by itself once the worker settles.
                tokio::spawn(report_while_overdue(ticket));
                return;
            }
            DeadlineCheck::Settled => return,
            DeadlineCheck::Pending(remaining) => tokio::time::sleep(remaining).await,
            // Polling at `deadline` keeps the restart precise: the poll after
            // a resume lands inside the fresh span and sleeps exactly its
            // remainder.
            DeadlineCheck::Parked => tokio::time::sleep(deadline.max(MIN_PARKED_POLL)).await,
        }
    }
}

async fn report_while_overdue(ticket: Arc<ExecutionTicket>) {
    loop {
        tokio::time::sleep(OVERDUE_REPORT_INTERVAL).await;
        if !ticket.is_overdue() {
            return;
        }
        ticket.report_overdue();
    }
}

/// Names what the current thread's execution is running. No-op without a
/// ticket; `describe` is only evaluated when one is installed.
pub(crate) fn label_current_execution(describe: impl FnOnce() -> String) {
    let ticket = CURRENT_TICKET
        .with(|slot| slot.borrow().clone())
        .or_else(|| LABEL_TICKET.with(|slot| slot.borrow().clone()));
    if let Some(ticket) = ticket {
        if let Ok(mut label) = ticket.label.lock() {
            *label = Some(bounded_label(describe()));
        }
    }
}

fn bounded_label(mut text: String) -> String {
    if text.len() > OVERDUE_LABEL_BYTES_MAX {
        let mut end = OVERDUE_LABEL_BYTES_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Thread-affine guard restoring the enclosing label ticket (if any) on drop.
pub(crate) struct ExecutionLabelScope {
    previous: Option<Arc<ExecutionTicket>>,
    _thread: PhantomData<*const ()>,
}

impl Drop for ExecutionLabelScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        LABEL_TICKET.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Suspends the current thread's execution deadline for the guard's lifetime.
/// Engine code calls this around an *intentional* park — never around work —
/// so a long-poll does not read as a wedged worker. No-op when no ticket is
/// installed (embedded callers, HTTP) or the ticket already timed out.
pub(crate) fn park_current_execution() -> ParkedExecution {
    let ticket = CURRENT_TICKET
        .with(|slot| slot.borrow().clone())
        .filter(|ticket| ticket.park());
    ParkedExecution {
        ticket,
        _thread: PhantomData,
    }
}

pub(crate) struct ParkedExecution {
    ticket: Option<Arc<ExecutionTicket>>,
    _thread: PhantomData<*const ()>,
}

impl Drop for ParkedExecution {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            ticket.resume();
        }
    }
}

/// Thread-affine guard restoring the enclosing ticket (if any) on drop.
pub(crate) struct ExecutionTicketScope {
    previous: Option<Arc<ExecutionTicket>>,
    _thread: PhantomData<*const ()>,
}

impl Drop for ExecutionTicketScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_TICKET.with(|slot| *slot.borrow_mut() = previous);
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
    fn overdue_execution_is_named_by_its_statement() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        assert_eq!(ticket.describe(), "<unlabelled execution>");
        // Without an installed ticket the label is neither built nor stored.
        label_current_execution(|| unreachable!("no ticket installed"));
        {
            let _scope = ticket.install();
            label_current_execution(|| "UPDATE t SET v = 1 WHERE id = 7".to_string());
            label_current_execution(|| "é".repeat(OVERDUE_LABEL_BYTES_MAX));
        }
        let label = ticket.describe();
        assert!(label.len() <= OVERDUE_LABEL_BYTES_MAX && label.starts_with('é'));

        // The HTTP edge names the route and lets the engine add the statement
        // through a label-only scope that parking cannot see.
        let http = watchdog.start();
        http.set_origin("HTTP POST /query".to_string());
        assert_eq!(http.describe(), "HTTP POST /query");
        {
            let _scope = http.label_scope();
            label_current_execution(|| "SELECT 1".to_string());
            assert!(park_current_execution().ticket.is_none());
        }
        assert_eq!(http.describe(), "HTTP POST /query :: SELECT 1");
        label_current_execution(|| unreachable!("label scope ended"));
        assert!(!ticket.is_overdue());
        let _completion = ticket.completion();
        ticket.mark_timed_out();
        assert!(ticket.is_overdue());
    }

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

    #[test]
    fn running_span_past_the_deadline_is_marked() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let _completion = ticket.completion();

        assert!(matches!(
            ticket.mark_timed_out_if_overdue(Duration::from_secs(60)),
            DeadlineCheck::Pending(_)
        ));
        assert!(watchdog.is_ready());

        assert_eq!(
            ticket.mark_timed_out_if_overdue(Duration::ZERO),
            DeadlineCheck::TimedOut
        );
        assert!(!watchdog.is_ready());
        assert_eq!(
            ticket.mark_timed_out_if_overdue(Duration::ZERO),
            DeadlineCheck::Settled
        );
    }

    #[test]
    fn parked_execution_is_never_marked() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let _completion = ticket.completion();
        let _scope = ticket.install();

        let parked = park_current_execution();
        assert_eq!(
            ticket.mark_timed_out_if_overdue(Duration::ZERO),
            DeadlineCheck::Parked
        );
        assert!(!ticket.mark_timed_out(), "parked worker is not wedged");
        assert!(watchdog.is_ready());
        drop(parked);

        assert_eq!(
            ticket.mark_timed_out_if_overdue(Duration::ZERO),
            DeadlineCheck::TimedOut,
            "a resumed worker is watched again"
        );
        assert!(!watchdog.is_ready());
    }

    #[test]
    fn resume_restarts_the_deadline() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let _completion = ticket.completion();
        let _scope = ticket.install();
        let deadline = Duration::from_millis(40);

        let parked = park_current_execution();
        std::thread::sleep(deadline * 2);
        drop(parked);

        match ticket.mark_timed_out_if_overdue(deadline) {
            DeadlineCheck::Pending(remaining) => assert!(
                remaining > deadline / 2,
                "parked time must not count toward the deadline: {remaining:?}"
            ),
            other => panic!("resumed span must start fresh, got {other:?}"),
        }
        assert!(watchdog.is_ready());
    }

    #[test]
    fn parking_does_not_clear_a_timeout() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let completion = ticket.completion();
        let _scope = ticket.install();
        assert!(ticket.mark_timed_out());

        drop(park_current_execution());
        assert!(!watchdog.is_ready(), "only completion recovers readiness");
        drop(completion);
        assert!(watchdog.is_ready());
    }

    #[test]
    fn park_without_installed_ticket_is_a_noop() {
        let parked = park_current_execution();
        assert!(parked.ticket.is_none());
    }

    #[test]
    fn ticket_scope_restores_the_enclosing_ticket() {
        let watchdog = ExecutionWatchdog::default();
        let outer = watchdog.start();
        let inner = watchdog.start();
        let _outer_scope = outer.install();
        drop(inner.install());

        let parked = park_current_execution();
        assert_eq!(
            outer.mark_timed_out_if_overdue(Duration::ZERO),
            DeadlineCheck::Parked
        );
        drop(parked);
    }

    #[tokio::test]
    async fn marker_skips_parked_spans_and_trips_on_a_stuck_resume() {
        let watchdog = ExecutionWatchdog::default();
        let ticket = watchdog.start();
        let _completion = ticket.completion();
        let deadline = Duration::from_millis(50);
        assert!(ticket.park());
        let marker = tokio::spawn(enforce_deadline(Arc::clone(&ticket), deadline));

        tokio::time::sleep(deadline * 4).await;
        assert!(watchdog.is_ready(), "a parked long-poll is not wedged");
        assert!(!marker.is_finished());

        ticket.resume();
        tokio::time::timeout(deadline * 20, marker)
            .await
            .expect("marker trips a stuck resumed span")
            .expect("marker task");
        assert!(!watchdog.is_ready(), "a stuck resumed worker still trips");
    }
}
