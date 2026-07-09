//! Deterministic simulation context for DST fault decisions.
//!
//! `buggify!()` is intentionally debug-only. Release builds compile the macro
//! to `false`, so production crash hooks keep their legacy env-var behavior.
//!
//! # Named fault classes (ADR 0074 §1)
//!
//! Beyond the crash/delay boundaries `buggify!` guards, the context owns the
//! vocabulary for the four modeled storage fault classes — [`FaultClass`]. Each
//! is individually addressable by name with its own probability (ppm), is **off
//! by default** (`0` ppm), and is release-inert exactly like `buggify!`: the
//! [`buggify_fault!`] macro compiles to `None` outside `debug_assertions`.
//!
//! A fired knob yields a [`FaultDecision`] whose parameters are drawn from the
//! same seeded stream as every other simulation decision, so a seed reproduces
//! the whole fault schedule byte for byte. Every applied injection is appended
//! to the campaign's **fault log** ([`FaultRecord`]) with class, target file,
//! offset and length, so an oracle can compute which durable objects a campaign
//! actually touched.

use crate::clock::{Clock, SimClock};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

const DEFAULT_BUGGIFY_PPM: u64 = 10_000;
const PPM_DENOMINATOR: u64 = 1_000_000;

/// The simulated sector size a torn write is cut at.
pub const SECTOR_BYTES: u64 = 512;

/// The `env` label the fault-class knobs record under in the buggify trace.
pub const FAULT_ENV: &str = "REDDB_DST_FAULT";

thread_local! {
    static ACTIVE: RefCell<Option<Rc<RefCell<ActiveSimulation>>>> = const { RefCell::new(None) };
}

/// The four modeled storage fault classes (ADR 0074 §1). Crash-at-any-point is
/// modeled separately by the existing `buggify!` crash boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaultClass {
    /// A write persists only a prefix of its buffer, cut at a sector boundary.
    TornWrite,
    /// A write persists the correct bytes at a wrong offset, reporting success.
    MisdirectedWrite,
    /// Stored bytes acquire a flipped bit between the write and a later read.
    BitRot,
    /// The write (or its fsync effect) is dropped entirely; success is reported.
    LostWrite,
}

impl FaultClass {
    /// Every class, in a stable order (the order knobs are rolled in).
    pub const ALL: [Self; 4] = [
        Self::TornWrite,
        Self::MisdirectedWrite,
        Self::BitRot,
        Self::LostWrite,
    ];

    /// The stable, machine-readable name a campaign selects the knob by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TornWrite => "torn_write",
            Self::MisdirectedWrite => "misdirected_write",
            Self::BitRot => "bit_rot",
            Self::LostWrite => "lost_write",
        }
    }

    /// Parse a class from its [`name`](Self::name).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.name() == name)
    }

    const fn index(self) -> usize {
        match self {
            Self::TornWrite => 0,
            Self::MisdirectedWrite => 1,
            Self::BitRot => 2,
            Self::LostWrite => 3,
        }
    }
}

impl fmt::Display for FaultClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A fired knob's deterministic parameters — what the backend must actually do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultDecision {
    /// Persist only `persisted` bytes of the buffer, then report the full length.
    TornWrite { persisted: u64 },
    /// Persist the whole buffer at `actual_offset` instead of the requested one.
    MisdirectedWrite { actual_offset: u64 },
    /// Flip bit `bit` of the byte at absolute `byte_offset` when it is read back.
    BitRot { byte_offset: u64, bit: u8 },
    /// Persist nothing; report success.
    LostWrite,
}

impl FaultDecision {
    /// The class this decision belongs to.
    pub const fn class(self) -> FaultClass {
        match self {
            Self::TornWrite { .. } => FaultClass::TornWrite,
            Self::MisdirectedWrite { .. } => FaultClass::MisdirectedWrite,
            Self::BitRot { .. } => FaultClass::BitRot,
            Self::LostWrite => FaultClass::LostWrite,
        }
    }
}

/// One applied injection, as an oracle consumes it: which class hit which file,
/// at which offset, over how many bytes, with the class-specific parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRecord {
    pub class: FaultClass,
    pub file: String,
    pub offset: u64,
    pub length: u64,
    pub decision: FaultDecision,
}

impl FaultRecord {
    /// Build a record for a decision applied to `file` over `[offset, offset + length)`.
    pub fn new(file: impl Into<String>, offset: u64, length: u64, decision: FaultDecision) -> Self {
        Self {
            class: decision.class(),
            file: file.into(),
            offset,
            length,
            decision,
        }
    }
}

/// One `key=value` line per injection: the machine-readable fault-log form.
impl fmt::Display for FaultRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "class={} file={} offset={} length={}",
            self.class, self.file, self.offset, self.length
        )?;
        match self.decision {
            FaultDecision::TornWrite { persisted } => write!(f, " persisted={persisted}"),
            FaultDecision::MisdirectedWrite { actual_offset } => {
                write!(f, " actual_offset={actual_offset}")
            }
            FaultDecision::BitRot { byte_offset, bit } => {
                write!(f, " byte_offset={byte_offset} bit={bit}")
            }
            FaultDecision::LostWrite => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SimulationContext {
    seed: u64,
    clock_start_ms: u64,
    buggify_ppm: u64,
    /// Per-class probability in parts-per-million, indexed by [`FaultClass::index`].
    /// All zero: every named fault class is off unless a campaign asks for it.
    fault_ppm: [u64; 4],
}

impl SimulationContext {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            clock_start_ms: seed,
            buggify_ppm: DEFAULT_BUGGIFY_PPM,
            fault_ppm: [0; 4],
        }
    }

    pub fn with_buggify_ppm(seed: u64, buggify_ppm: u64) -> Self {
        Self {
            buggify_ppm: buggify_ppm.min(PPM_DENOMINATOR),
            ..Self::new(seed)
        }
    }

    /// Arm one named fault class at `ppm`. `0` leaves it off (the default).
    #[must_use]
    pub fn with_fault_class(mut self, class: FaultClass, ppm: u64) -> Self {
        self.fault_ppm[class.index()] = ppm.min(PPM_DENOMINATOR);
        self
    }

    /// The probability currently armed for `class`.
    pub fn fault_ppm(&self, class: FaultClass) -> u64 {
        self.fault_ppm[class.index()]
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

    /// Every injection applied under this context, in order.
    pub fn fault_log(&self) -> Vec<FaultRecord> {
        self.active.borrow().fault_log.clone()
    }

    /// The fault log as newline-terminated `key=value` lines.
    pub fn fault_log_lines(&self) -> String {
        self.fault_log()
            .iter()
            .map(|record| format!("{record}\n"))
            .collect()
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
    fault_ppm: [u64; 4],
    tick: u64,
    trace: Vec<u8>,
    fault_log: Vec<FaultRecord>,
}

impl ActiveSimulation {
    fn new(context: SimulationContext) -> Self {
        let mut trace = Vec::new();
        trace.extend_from_slice(format!("seed={}\n", context.seed).as_bytes());
        Self {
            clock: SimClock::from_seed(context.clock_start_ms),
            rng: SplitMix64::new(context.seed ^ 0x4255_4747_4946_595F), // "BUGGIFY_"
            buggify_ppm: context.buggify_ppm,
            fault_ppm: context.fault_ppm,
            tick: 0,
            trace,
            fault_log: Vec::new(),
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

/// Roll the seeded coin for one named fault class over `[offset, offset + length)`.
///
/// Returns `None` when no simulation is installed, when the class is not armed
/// (`0` ppm — the default), or when the coin simply did not fire. A fired knob
/// consumes further draws from the same stream to derive its parameters, so the
/// whole schedule is a pure function of the seed and the armed probabilities.
///
/// This only *decides*; the backend that applies the fault calls
/// [`record_fault`] with the target file so the log names a durable object.
pub fn roll_fault(class: FaultClass, offset: u64, length: u64) -> Option<FaultDecision> {
    ACTIVE.with(|slot| {
        let active = slot.borrow().clone()?;
        let mut active = active.borrow_mut();
        let ppm = active.fault_ppm[class.index()];
        // An unarmed class draws nothing: "off by default" must not perturb the
        // stream a campaign without it would have seen.
        if ppm == 0 || !active.boundary(FAULT_ENV, class.name(), ppm) {
            return None;
        }
        let decision = derive_fault(class, offset, length, &mut || active.rng.next_u64());
        Some(decision)
    })
}

/// Append an applied injection to the active campaign's fault log. A no-op when
/// no simulation is installed.
pub fn record_fault(record: FaultRecord) {
    ACTIVE.with(|slot| {
        if let Some(active) = slot.borrow().clone() {
            active.borrow_mut().fault_log.push(record);
        }
    });
}

/// Derive a fired knob's parameters from `next`, the campaign's seeded stream.
///
/// Pure and stream-driven so both the simulation context and an out-of-context
/// backend (a `SimVfs` rolling its own coins) produce the same fault shapes.
///
/// A [`FaultClass::TornWrite`] is cut at the first simulated sector boundary the
/// write crosses, choosing uniformly among them. A write that never crosses one
/// — the common case for small WAL frames — is cut at a byte prefix instead,
/// modeling sub-sector tearing; the cut is always a strict prefix, so a fired
/// knob always loses bytes.
pub fn derive_fault(
    class: FaultClass,
    offset: u64,
    length: u64,
    next: &mut dyn FnMut() -> u64,
) -> FaultDecision {
    match class {
        FaultClass::TornWrite => FaultDecision::TornWrite {
            persisted: torn_prefix(offset, length, next),
        },
        FaultClass::MisdirectedWrite => {
            let delta = SECTOR_BYTES * (1 + next() % 4);
            let backwards = next() % 2 == 1;
            let actual_offset = if backwards && offset >= delta {
                offset - delta
            } else {
                offset.saturating_add(delta)
            };
            FaultDecision::MisdirectedWrite { actual_offset }
        }
        FaultClass::BitRot => {
            let byte_offset = offset + if length == 0 { 0 } else { next() % length };
            let bit = u8::try_from(next() % 8).unwrap_or(0);
            FaultDecision::BitRot { byte_offset, bit }
        }
        FaultClass::LostWrite => FaultDecision::LostWrite,
    }
}

/// How many bytes of a torn write reach the platter: a sector-aligned cut when
/// the write crosses a sector boundary, otherwise a strict byte prefix.
fn torn_prefix(offset: u64, length: u64, next: &mut dyn FnMut() -> u64) -> u64 {
    if length == 0 {
        return 0;
    }
    let end = offset + length;
    let first_boundary = (offset / SECTOR_BYTES + 1) * SECTOR_BYTES;
    if first_boundary >= end {
        // Entirely inside one sector: tear at a strict byte prefix (0..length-1).
        return next() % length;
    }
    let boundaries = (end - 1 - first_boundary) / SECTOR_BYTES + 1;
    let chosen = first_boundary + (next() % boundaries) * SECTOR_BYTES;
    chosen - offset
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

/// Roll one named fault class, release-inert like [`buggify!`].
///
/// Debug builds consult the installed simulation context; release builds
/// compile to `None`, so no production write path can ever be perturbed.
#[macro_export]
macro_rules! buggify_fault {
    ($class:expr, $offset:expr, $length:expr) => {{
        #[cfg(debug_assertions)]
        {
            $crate::dst::roll_fault($class, $offset, $length)
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = ($class, $offset, $length);
            ::core::option::Option::<$crate::dst::FaultDecision>::None
        }
    }};
}

#[cfg(test)]
mod tests {
    use crate::dst::{FaultClass, FaultDecision, FaultRecord, SECTOR_BYTES};
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

    #[test]
    fn fault_classes_round_trip_through_their_names() {
        for class in FaultClass::ALL {
            assert_eq!(FaultClass::from_name(class.name()), Some(class));
        }
        assert_eq!(FaultClass::from_name("not_a_class"), None);
    }

    #[test]
    fn every_fault_class_is_off_by_default() {
        let context = SimulationContext::new(7);
        for class in FaultClass::ALL {
            assert_eq!(context.fault_ppm(class), 0, "{class} must default to off");
        }
        let guard = context.install();
        for class in FaultClass::ALL {
            assert_eq!(crate::buggify_fault!(class, 0, 64), None);
        }
        assert!(guard.fault_log().is_empty());
        // An unarmed class draws nothing, so it cannot perturb the seed stream.
        assert!(!String::from_utf8(guard.trace())
            .unwrap()
            .contains("env=REDDB_DST_FAULT"));
    }

    #[test]
    fn each_class_is_individually_triggerable_by_name_and_ppm() {
        for armed in FaultClass::ALL {
            let guard = SimulationContext::new(99)
                .with_fault_class(armed, 1_000_000)
                .install();
            for class in FaultClass::ALL {
                let decision = crate::buggify_fault!(class, SECTOR_BYTES + 8, 64);
                if class == armed {
                    let decision = decision.unwrap_or_else(|| panic!("{armed} must fire at 1e6"));
                    assert_eq!(decision.class(), armed);
                } else {
                    assert_eq!(
                        decision, None,
                        "{class} must stay off while {armed} is armed"
                    );
                }
            }
            let trace = String::from_utf8(guard.trace()).unwrap();
            assert!(trace.contains(&format!("point={armed}")));
        }
    }

    #[test]
    fn a_torn_write_crossing_a_sector_is_cut_at_a_sector_boundary() {
        let guard = SimulationContext::new(5)
            .with_fault_class(FaultClass::TornWrite, 1_000_000)
            .install();
        // Spans sectors 0..=2, so the cut must land on 512 or 1024.
        let offset = 8;
        let Some(FaultDecision::TornWrite { persisted }) =
            crate::buggify_fault!(FaultClass::TornWrite, offset, 1_200)
        else {
            panic!("torn_write must fire at 1e6")
        };
        assert!(
            (offset + persisted) % SECTOR_BYTES == 0,
            "cut at {persisted}"
        );
        assert!(persisted < 1_200, "a torn write always loses bytes");
        drop(guard);
    }

    #[test]
    fn a_torn_write_inside_one_sector_is_cut_at_a_strict_byte_prefix() {
        let _guard = SimulationContext::new(6)
            .with_fault_class(FaultClass::TornWrite, 1_000_000)
            .install();
        // A 48-byte WAL frame never crosses a sector: sub-sector tearing.
        let Some(FaultDecision::TornWrite { persisted }) =
            crate::buggify_fault!(FaultClass::TornWrite, 64, 48)
        else {
            panic!("torn_write must fire at 1e6")
        };
        assert!(persisted < 48, "a torn write always loses bytes");
    }

    #[test]
    fn a_misdirected_write_lands_a_whole_number_of_sectors_away() {
        let _guard = SimulationContext::new(11)
            .with_fault_class(FaultClass::MisdirectedWrite, 1_000_000)
            .install();
        for offset in [0, 64, 4_096, 10_000] {
            let Some(FaultDecision::MisdirectedWrite { actual_offset }) =
                crate::buggify_fault!(FaultClass::MisdirectedWrite, offset, 64)
            else {
                panic!("misdirected_write must fire at 1e6")
            };
            assert_ne!(actual_offset, offset, "the write must land elsewhere");
            assert_eq!(actual_offset.abs_diff(offset) % SECTOR_BYTES, 0);
        }
    }

    #[test]
    fn bit_rot_targets_a_byte_inside_the_read_region() {
        let _guard = SimulationContext::new(13)
            .with_fault_class(FaultClass::BitRot, 1_000_000)
            .install();
        for _ in 0..32 {
            let Some(FaultDecision::BitRot { byte_offset, bit }) =
                crate::buggify_fault!(FaultClass::BitRot, 100, 40)
            else {
                panic!("bit_rot must fire at 1e6")
            };
            assert!((100..140).contains(&byte_offset));
            assert!(bit < 8);
        }
    }

    #[test]
    fn the_fault_log_records_class_file_offset_and_length() {
        let guard = SimulationContext::new(21)
            .with_fault_class(FaultClass::LostWrite, 1_000_000)
            .install();
        let decision = crate::buggify_fault!(FaultClass::LostWrite, 4_096, 512)
            .expect("lost_write must fire at 1e6");
        crate::dst::record_fault(FaultRecord::new("/db/wal.log", 4_096, 512, decision));

        let log = guard.fault_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].class, FaultClass::LostWrite);
        assert_eq!(log[0].file, "/db/wal.log");
        assert_eq!(log[0].offset, 4_096);
        assert_eq!(log[0].length, 512);
        assert_eq!(
            guard.fault_log_lines(),
            "class=lost_write file=/db/wal.log offset=4096 length=512\n"
        );
    }

    #[test]
    fn same_seed_produces_an_identical_fault_schedule() {
        fn schedule(seed: u64) -> Vec<String> {
            let mut context = SimulationContext::new(seed);
            for class in FaultClass::ALL {
                context = context.with_fault_class(class, 300_000);
            }
            let guard = context.install();
            for step in 0..64u64 {
                for class in FaultClass::ALL {
                    let offset = step * 97;
                    if let Some(decision) = crate::buggify_fault!(class, offset, 64) {
                        crate::dst::record_fault(FaultRecord::new(
                            "/db/wal.log",
                            offset,
                            64,
                            decision,
                        ));
                    }
                }
            }
            guard.fault_log().iter().map(ToString::to_string).collect()
        }

        let first = schedule(0xDEAD);
        assert!(!first.is_empty(), "the sweep must inject something");
        assert_eq!(first, schedule(0xDEAD), "same seed → same fault schedule");
        assert_ne!(first, schedule(0xBEEF), "different seeds must diverge");
    }

    #[test]
    fn fault_classes_compose_with_the_crash_knob_deterministically() {
        fn run(seed: u64) -> Vec<u8> {
            let guard = SimulationContext::with_buggify_ppm(seed, 500_000)
                .with_fault_class(FaultClass::TornWrite, 400_000)
                .install();
            for _ in 0..32 {
                let _crashed = crate::buggify!(ENV, "wal_after_frame_write");
                if let Some(decision) = crate::buggify_fault!(FaultClass::TornWrite, 64, 48) {
                    crate::dst::record_fault(FaultRecord::new("/db/wal.log", 64, 48, decision));
                }
            }
            let mut out = guard.trace();
            out.extend_from_slice(guard.fault_log_lines().as_bytes());
            out
        }
        assert_eq!(
            run(1234),
            run(1234),
            "crash + torn_write must replay exactly"
        );
    }

    #[test]
    fn no_fault_fires_without_an_installed_context() {
        for class in FaultClass::ALL {
            assert_eq!(crate::buggify_fault!(class, 0, 64), None);
        }
    }
}
