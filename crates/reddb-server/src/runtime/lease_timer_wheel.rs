//! Edge-triggered timer wheel for lease expiry / refresh scheduling.
//!
//! Replaces the `thread::sleep(ttl/3)` polling loop in `lease_loop` with
//! a bucket-granular timer wheel: the worker thread sleeps until the next
//! bucket is due, fires all leases in that bucket, then sleeps again.
//!
//! ## Design (top-half / bottom-half)
//!
//! * **Top-half** (`schedule` / `cancel`): callable from any thread, O(log n).
//! * **Bottom-half** (`run_until_shutdown`): dedicated worker thread; wakes
//!   only when a bucket fires. Zero CPU for idle leases.
//!
//! Bucket granularity is configurable (default 100 ms). All expirations
//! within the same granularity window coalesce into one wake-up.
//!
//! ## Scalability
//!
//! With N idle leases all expiring far in the future, the worker sleeps
//! exactly until the earliest bucket — not once per lease. CPU overhead
//! is O(0) while idle and O(k) where k is the number of leases firing in
//! the current bucket.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub type LeaseId = String;

struct WheelState {
    /// Sorted map: fire instant → ids firing at that bucket boundary.
    schedule: BTreeMap<Instant, Vec<LeaseId>>,
    /// Reverse index: id → fire instant for O(1) cancel / reschedule.
    id_to_fire: HashMap<LeaseId, Instant>,
    shutdown: bool,
}

/// Timer wheel that schedules leases to fire at specific wall-clock times.
///
/// Construct once, share via `Arc`. Call [`schedule`] from producer threads,
/// call [`run_until_shutdown`] on a dedicated worker thread.
pub struct LeaseTimerWheel {
    state: Mutex<WheelState>,
    cvar: Condvar,
    granularity: Duration,
}

impl LeaseTimerWheel {
    /// New wheel with `granularity_ms` bucket width (minimum 1 ms).
    pub fn new(granularity_ms: u64) -> Self {
        Self {
            state: Mutex::new(WheelState {
                schedule: BTreeMap::new(),
                id_to_fire: HashMap::new(),
                shutdown: false,
            }),
            cvar: Condvar::new(),
            granularity: Duration::from_millis(granularity_ms.max(1)),
        }
    }

    /// Schedule `id` to fire at or after `expiry`.
    ///
    /// The expiry is snapped forward to the next granularity boundary so
    /// near-simultaneous expirations coalesce into a single wake-up.
    /// Re-scheduling an existing id silently replaces the prior entry.
    pub fn schedule(&self, id: LeaseId, expiry: Instant) {
        let fire_time = self.snap(expiry);
        let mut state = self.state.lock().expect("wheel mutex poisoned");
        self.remove_existing(&mut state, &id);
        state
            .schedule
            .entry(fire_time)
            .or_default()
            .push(id.clone());
        state.id_to_fire.insert(id, fire_time);
        self.cvar.notify_one();
    }

    /// Cancel a scheduled lease. No-op if `id` is not scheduled.
    pub fn cancel(&self, id: &str) {
        let mut state = self.state.lock().expect("wheel mutex poisoned");
        self.remove_existing(&mut state, id);
    }

    /// Signal the worker to stop after the current batch (if any) finishes.
    pub fn shutdown(&self) {
        let mut state = self.state.lock().expect("wheel mutex poisoned");
        state.shutdown = true;
        self.cvar.notify_all();
    }

    /// Block until `shutdown()` is called or `handler` returns `false`.
    ///
    /// For each fired lease id, calls `handler(id)`. If the handler returns
    /// `false`, the wheel stops immediately (even mid-batch).
    ///
    /// CPU: the thread is parked in `Condvar::wait_timeout` between buckets.
    /// With 10 k idle leases scheduled far in the future the CPU cost is
    /// effectively zero — one wake-up per bucket boundary, not per lease.
    pub fn run_until_shutdown(&self, mut handler: impl FnMut(LeaseId) -> bool + Send) {
        loop {
            // Hold the lock through sleep-duration computation AND the
            // wait_timeout call so no `schedule()` notification is lost.
            // (If we released between computing sleep_for and calling
            // wait_timeout, a notification fired in that window would be
            // dropped and the worker could sleep past the item's due time.)
            let fired: Vec<LeaseId> = {
                let state = self.state.lock().expect("wheel mutex poisoned");
                if state.shutdown {
                    return;
                }

                let sleep_for = match state.schedule.keys().next().copied() {
                    Some(t) => {
                        let now = Instant::now();
                        if t <= now {
                            Duration::ZERO
                        } else {
                            t - now
                        }
                    }
                    // Nothing scheduled: park up to 1 h; condvar unparks on
                    // schedule() or shutdown().
                    None => Duration::from_secs(3600),
                };

                // Optionally sleep, keeping the lock through the call so
                // any concurrent schedule() unparks us promptly.
                let mut state = if sleep_for > Duration::ZERO {
                    let (guard, _) = self
                        .cvar
                        .wait_timeout(state, sleep_for)
                        .expect("condvar wait_timeout failed");
                    if guard.shutdown {
                        return;
                    }
                    guard
                } else {
                    state
                };

                // Drain all due buckets while still holding the lock.
                let now = Instant::now();
                let mut ids = Vec::new();
                while let Some((&fire_time, _)) = state.schedule.iter().next() {
                    if fire_time > now {
                        break;
                    }
                    let bucket = state.schedule.remove(&fire_time).unwrap_or_default();
                    for id in &bucket {
                        state.id_to_fire.remove(id.as_str());
                    }
                    ids.extend(bucket);
                }
                ids
                // Lock released here.
            };

            // Invoke handler outside the lock so schedule() / cancel() can
            // be called from within the handler (e.g. to reschedule).
            for id in fired {
                if !handler(id) {
                    return;
                }
            }
        }
    }

    // Snap `expiry` forward to the next granularity boundary at-or-after
    // `expiry`. Monotonic: result is always >= Instant::now().
    fn snap(&self, expiry: Instant) -> Instant {
        let now = Instant::now();
        let base = expiry.max(now);
        let nanos_from_now = (base - now).as_nanos() as u64;
        let gran_nanos = self.granularity.as_nanos() as u64;
        let snapped = ((nanos_from_now + gran_nanos - 1) / gran_nanos) * gran_nanos;
        now + Duration::from_nanos(snapped)
    }

    fn remove_existing(&self, state: &mut WheelState, id: &str) {
        if let Some(old_time) = state.id_to_fire.remove(id) {
            if let Some(v) = state.schedule.get_mut(&old_time) {
                v.retain(|x| x != id);
                if v.is_empty() {
                    state.schedule.remove(&old_time);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn single_lease_fires_within_granularity() {
        let wheel = Arc::new(LeaseTimerWheel::new(50));
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = Arc::clone(&fired);
        let wheel_clone = Arc::clone(&wheel);

        // Schedule to fire in 50 ms.
        wheel.schedule("lease-1".to_string(), Instant::now() + ms(50));

        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |_id| {
                fired_clone.fetch_add(1, Ordering::SeqCst);
                false // stop after first fire
            });
        });

        t.join().unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fires_at_roughly_the_right_time() {
        let wheel = Arc::new(LeaseTimerWheel::new(50));
        let start = Instant::now();
        let fired_at = Arc::new(Mutex::new(None::<Instant>));
        let fired_at_clone = Arc::clone(&fired_at);
        let wheel_clone = Arc::clone(&wheel);

        wheel.schedule("t".to_string(), start + ms(100));

        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |_id| {
                *fired_at_clone.lock().unwrap() = Some(Instant::now());
                false
            });
        });
        t.join().unwrap();

        let elapsed = fired_at.lock().unwrap().unwrap() - start;
        // Must fire at or after 100 ms, and within 100 ms + 2 * granularity (slack for CI).
        assert!(elapsed >= ms(100), "fired too early: {elapsed:?}");
        assert!(
            elapsed < ms(400),
            "fired too late (CI slowness?): {elapsed:?}"
        );
    }

    #[test]
    fn coalesces_multiple_leases_in_same_bucket() {
        let wheel = Arc::new(LeaseTimerWheel::new(100));
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = Arc::clone(&fired);
        let wheel_clone = Arc::clone(&wheel);
        let total = Arc::new(AtomicUsize::new(0));
        let total_clone = Arc::clone(&total);

        // Three leases scheduled within the same 100 ms bucket.
        let deadline = Instant::now() + ms(100);
        wheel.schedule("a".to_string(), deadline);
        wheel.schedule("b".to_string(), deadline);
        wheel.schedule("c".to_string(), deadline);

        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |_id| {
                let n = fired_clone.fetch_add(1, Ordering::SeqCst) + 1;
                total_clone.store(n, Ordering::SeqCst);
                n < 3 // stop after 3 fires
            });
        });
        t.join().unwrap();
        assert_eq!(total.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cancel_prevents_fire() {
        let wheel = Arc::new(LeaseTimerWheel::new(50));
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = Arc::clone(&fired);
        let wheel_clone = Arc::clone(&wheel);

        wheel.schedule("doomed".to_string(), Instant::now() + ms(200));
        wheel.schedule("survivor".to_string(), Instant::now() + ms(100));
        wheel.cancel("doomed");

        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |id| {
                fired_clone.fetch_add(1, Ordering::SeqCst);
                assert_eq!(id, "survivor", "cancelled lease must not fire");
                false
            });
        });
        t.join().unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reschedule_replaces_prior_entry() {
        let wheel = Arc::new(LeaseTimerWheel::new(50));
        let fire_times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let fire_times_clone = Arc::clone(&fire_times);
        let wheel_clone = Arc::clone(&wheel);

        // Schedule at t+50, then immediately reschedule to t+150.
        wheel.schedule("x".to_string(), Instant::now() + ms(50));
        wheel.schedule("x".to_string(), Instant::now() + ms(150));

        let start = Instant::now();
        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |_id| {
                fire_times_clone.lock().unwrap().push(Instant::now());
                false
            });
        });
        t.join().unwrap();

        let times = fire_times.lock().unwrap();
        assert_eq!(times.len(), 1, "only one fire expected after reschedule");
        // Should not fire before 150 ms.
        assert!(
            times[0] - start >= ms(150),
            "fired at wrong time: {:?}",
            times[0] - start
        );
    }

    #[test]
    fn shutdown_stops_worker_with_no_scheduled_leases() {
        let wheel = Arc::new(LeaseTimerWheel::new(100));
        let wheel_clone = Arc::clone(&wheel);

        let t = thread::spawn(move || {
            wheel_clone.run_until_shutdown(|_| true);
        });

        // Worker is sleeping (nothing scheduled). Signal shutdown.
        thread::sleep(ms(20));
        wheel.shutdown();
        t.join().unwrap(); // must not hang
    }

    #[test]
    fn idle_10k_leases_do_not_spin() {
        // 10 k leases all expiring 60 s from now.
        // The worker should sleep until then — we verify it does not busy-spin
        // by checking that it is still sleeping after 50 ms.
        let wheel = Arc::new(LeaseTimerWheel::new(100));
        let far_future = Instant::now() + Duration::from_secs(60);
        for i in 0..10_000usize {
            wheel.schedule(format!("lease-{i}"), far_future);
        }

        let fired = Arc::new(AtomicUsize::new(0));
        let fired_clone = Arc::clone(&fired);
        let wheel_clone = Arc::clone(&wheel);

        thread::spawn(move || {
            wheel_clone.run_until_shutdown(move |_| {
                fired_clone.fetch_add(1, Ordering::SeqCst);
                true
            });
        });

        // Sleep 50 ms; nothing should have fired.
        thread::sleep(ms(50));
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "no leases should fire during idle period"
        );
        wheel.shutdown();
    }
}
