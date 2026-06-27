//! Deterministic simulation context for DST fault decisions.
//!
//! `buggify!()` is intentionally debug-only. Release builds compile the macro
//! to `false`, so production crash hooks keep their legacy env-var behavior.

use crate::clock::{Clock, SimClock};
use std::cell::RefCell;
use std::rc::Rc;

const DEFAULT_BUGGIFY_PPM: u64 = 10_000;
const PPM_DENOMINATOR: u64 = 1_000_000;

thread_local! {
    static ACTIVE: RefCell<Option<Rc<RefCell<ActiveSimulation>>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy)]
pub struct SimulationContext {
    seed: u64,
    clock_start_ms: u64,
    buggify_ppm: u64,
}

impl SimulationContext {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            clock_start_ms: seed,
            buggify_ppm: DEFAULT_BUGGIFY_PPM,
        }
    }

    pub fn with_buggify_ppm(seed: u64, buggify_ppm: u64) -> Self {
        Self {
            buggify_ppm: buggify_ppm.min(PPM_DENOMINATOR),
            ..Self::new(seed)
        }
    }

    pub fn install(self) -> SimulationGuard {
        let active = Rc::new(RefCell::new(ActiveSimulation::new(self)));
        let previous = ACTIVE.with(|slot| slot.replace(Some(Rc::clone(&active))));
        SimulationGuard { active, previous }
    }
}

pub struct SimulationGuard {
    active: Rc<RefCell<ActiveSimulation>>,
    previous: Option<Rc<RefCell<ActiveSimulation>>>,
}

impl SimulationGuard {
    pub fn trace(&self) -> Vec<u8> {
        self.active.borrow().trace.clone()
    }
}

impl Drop for SimulationGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        ACTIVE.with(|slot| {
            slot.replace(previous);
        });
    }
}

struct ActiveSimulation {
    clock: SimClock,
    rng: SplitMix64,
    buggify_ppm: u64,
    tick: u64,
    trace: Vec<u8>,
}

impl ActiveSimulation {
    fn new(context: SimulationContext) -> Self {
        let mut trace = Vec::new();
        trace.extend_from_slice(format!("seed={}\n", context.seed).as_bytes());
        Self {
            clock: SimClock::from_seed(context.clock_start_ms),
            rng: SplitMix64::new(context.seed ^ 0x4255_4747_4946_595F), // "BUGGIFY_"
            buggify_ppm: context.buggify_ppm,
            tick: 0,
            trace,
        }
    }

    fn boundary(&mut self, env: &str, point: &str, ppm: u64) -> bool {
        self.tick = self.tick.saturating_add(1);
        self.clock.advance_ms(1);
        let roll = self.rng.below(PPM_DENOMINATOR);
        let threshold = ppm.min(PPM_DENOMINATOR);
        let fired = roll < threshold;
        self.trace.extend_from_slice(
            format!(
                "tick={} clock_ms={} env={} point={} roll={} ppm={} fired={}\n",
                self.tick,
                self.clock.now_unix_millis(),
                env,
                point,
                roll,
                threshold,
                fired
            )
            .as_bytes(),
        );
        fired
    }
}

pub fn buggify(env: &str, point: &str) -> bool {
    ACTIVE.with(|slot| {
        let Some(active) = slot.borrow().clone() else {
            return false;
        };
        let ppm = { active.borrow().buggify_ppm };
        let fired = active.borrow_mut().boundary(env, point, ppm);
        fired
    })
}

pub fn buggify_at(env: &str, point: &str, ppm: u64) -> bool {
    ACTIVE.with(|slot| {
        let Some(active) = slot.borrow().clone() else {
            return false;
        };
        let fired = active.borrow_mut().boundary(env, point, ppm);
        fired
    })
}

pub fn is_active() -> bool {
    ACTIVE.with(|slot| slot.borrow().is_some())
}

#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }
}

#[macro_export]
macro_rules! buggify {
    ($env:expr, $point:expr) => {{
        #[cfg(debug_assertions)]
        {
            $crate::dst::buggify($env, $point)
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = ($env, $point);
            false
        }
    }};
}

#[cfg(test)]
mod tests {
    use crate::SimulationContext;

    const ENV: &str = "REDDB_EMBEDDED_RDB_CRASH_AT";

    fn trace_for(seed: u64) -> Vec<u8> {
        let context = SimulationContext::with_buggify_ppm(seed, 1_000_000);
        let guard = context.install();
        assert!(crate::buggify!(ENV, "wal_after_frame_write"));
        assert!(crate::buggify!(ENV, "wal_after_frame_sync"));
        assert!(crate::buggify!(ENV, "wal_after_superblock_write"));
        guard.trace()
    }

    #[test]
    fn same_seed_produces_byte_identical_buggify_trace() {
        let first = trace_for(0x5EED);
        let second = trace_for(0x5EED);
        assert_eq!(first, second);
    }

    #[test]
    fn buggify_can_fire_at_named_crash_boundary() {
        let context = SimulationContext::with_buggify_ppm(123, 1_000_000);
        let guard = context.install();
        assert!(crate::buggify!(ENV, "snapshot_after_manifest_write"));
        let trace = String::from_utf8(guard.trace()).unwrap();
        assert!(trace.contains("env=REDDB_EMBEDDED_RDB_CRASH_AT"));
        assert!(trace.contains("point=snapshot_after_manifest_write"));
        assert!(trace.contains("fired=true"));
    }
}
