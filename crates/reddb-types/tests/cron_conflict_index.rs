//! In-memory access-path experiment for the earliest overlap in one visible room.
//! Run with cargo test -j 1 --profile release-fast -p reddb-io-types
//! --test cron_conflict_index -- --include-ignored --nocapture --test-threads=1.
use reddb_types::cron::CronSchedule;
use std::hint::black_box;
use std::ops::Range;
use std::time::Instant;

const MINUTE: i64 = 60_000;
const DAY: i64 = 1_440 * MINUTE;
const SATURDAY: i64 = 1_795_824_000_000; // 2026-11-28 00:00 UTC
const HORIZON_END: i64 = SATURDAY + 7 * DAY;

struct Rule {
    schedule: CronSchedule,
    duration: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    start: i64,
    end: i64,
    id: usize,
}

// Input order breaks equal starts by rule ID, just like ORDER BY begins_at, id.
fn increasing_ends(slots: &[Slot]) -> Vec<Slot> {
    let mut index = Vec::new();
    let mut latest = i64::MIN;
    for slot in slots {
        if slot.end > latest {
            index.push(*slot);
            latest = slot.end;
        }
    }
    index
}

fn indexed_first(index: &[Slot], window: &Range<i64>) -> Option<Slot> {
    let first = index.partition_point(|slot| slot.end <= window.start);
    index
        .get(first)
        .copied()
        .filter(|slot| !window.is_empty() && slot.start < window.end)
}

// Model an ordered seek on start with a known maximum duration for this room.
fn ordered_first(slots: &[Slot], longest: i64, window: &Range<i64>) -> Option<Slot> {
    if window.is_empty() {
        return None;
    }
    let lower = slots.partition_point(|slot| slot.start <= window.start - longest);
    slots[lower..]
        .iter()
        .take_while(|slot| slot.start < window.end)
        .find(|slot| slot.end > window.start)
        .copied()
}

fn from_rules(rules: &[Rule], window: &Range<i64>) -> Option<Slot> {
    if window.is_empty() {
        return None;
    }
    rules
        .iter()
        .enumerate()
        .filter_map(|(id, rule)| {
            let start = rule
                .schedule
                .occurrences(window.start - rule.duration + 1, window.end)
                .expect("bounded query window")
                .next()?
                .expect("valid UTC schedule");
            Some(Slot {
                start,
                end: start + rule.duration,
                id,
            })
        })
        .min_by_key(|slot| (slot.start, slot.id))
}

fn materialize(rules: &[Rule]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (id, rule) in rules.iter().enumerate() {
        // Include starts before Saturday if their occupancy enters the horizon.
        let instants = rule
            .schedule
            .occurrences(SATURDAY - rule.duration + 1, HORIZON_END)
            .expect("bounded fixture horizon");
        for instant in instants {
            let start = instant.expect("valid UTC occurrence");
            slots.push(Slot {
                start,
                end: start + rule.duration,
                id,
            });
        }
    }
    slots.sort_unstable_by_key(|slot| (slot.start, slot.id));
    slots
}

fn fixture(count: usize, mixed: bool) -> Vec<Rule> {
    (0..count)
        .map(|id| {
            let minute = 8 * 60 + (37 * id) % 600;
            let mut expression = format!("{} {} * * *", minute % 60, minute / 60);
            let mut duration = 30;
            if mixed {
                duration = 5 + (id % 41) as i64;
                if id == 0 {
                    expression = "45 16 * * FRI".to_string();
                    duration = 36 * 60;
                }
            }
            Rule {
                schedule: CronSchedule::new(&expression, "UTC").expect("fixture rule"),
                duration: duration * MINUTE,
            }
        })
        .collect()
}

fn oracle(slots: &[Slot], window: &Range<i64>) -> Option<Slot> {
    slots
        .iter()
        .find(|slot| !window.is_empty() && slot.start < window.end && slot.end > window.start)
        .copied()
}

#[test]
fn earliest_overlap_preserves_ties_boundaries_and_carry_in() {
    let slots = [
        Slot {
            start: -2,
            end: 2,
            id: 8,
        },
        Slot {
            start: 0,
            end: 1,
            id: 1,
        },
        Slot {
            start: 0,
            end: 4,
            id: 2,
        },
        Slot {
            start: 0,
            end: 5,
            id: 3,
        },
        Slot {
            start: 1,
            end: 5,
            id: 0,
        },
        Slot {
            start: 7,
            end: 8,
            id: 4,
        },
    ];
    let index = increasing_ends(&slots);
    assert_eq!(index, [slots[0], slots[2], slots[3], slots[5]]);
    for start in -4..=9 {
        for end in start..=10 {
            let window = start..end;
            assert_eq!(indexed_first(&index, &window), oracle(&slots, &window));
            assert_eq!(ordered_first(&slots, 5, &window), oracle(&slots, &window));
        }
    }
    assert_eq!(indexed_first(&[], &(0..1)), None);
    // Removing the first booking exposes a row the old summary had discarded.
    let remaining = &slots[1..];
    assert!(!index.contains(&remaining[0]));
    assert_eq!(
        indexed_first(&increasing_ends(remaining), &(0..1)),
        Some(remaining[0])
    );
    let rules = fixture(4, true);
    let carry = Slot {
        start: SATURDAY - 435 * MINUTE,
        end: SATURDAY + 1725 * MINUTE,
        id: 0,
    };
    let slots = materialize(&rules);
    assert_eq!(from_rules(&rules, &(SATURDAY..SATURDAY + 1)), Some(carry));
    assert_eq!(
        indexed_first(&increasing_ends(&slots), &(SATURDAY..SATURDAY + 1)),
        Some(carry)
    );
}

// Median of seven warm batch averages. Batch short index lookups to avoid
// interpreting timer resolution as individual-query latency.
fn ns_per_query(
    windows: &[Range<i64>],
    repeats: usize,
    lookup: impl Fn(&Range<i64>) -> Option<Slot>,
) -> u128 {
    let mut samples = [0; 7];
    for window in windows {
        black_box(lookup(black_box(window)));
    }
    for sample in &mut samples {
        let before = Instant::now();
        for _ in 0..repeats {
            for window in windows {
                black_box(lookup(black_box(window)));
            }
        }
        *sample = before.elapsed().as_nanos() / (windows.len() * repeats) as u128;
    }
    samples.sort_unstable();
    samples[3]
}

#[test]
#[ignore = "single-threaded conflict index experiment"]
fn first_conflict_hot_room() {
    for count in [4, 64, 1024] {
        for mixed in [false, true] {
            let rules = fixture(count, mixed);
            let longest = rules
                .iter()
                .map(|r| r.duration)
                .max()
                .expect("nonempty fixture");
            let slots = materialize(&rules);
            let index = increasing_ends(&slots);
            let mut build_times = [0; 7];
            let mut summary_times = [0; 7];
            for (elapsed, summary_elapsed) in build_times.iter_mut().zip(&mut summary_times) {
                let before = Instant::now();
                let all = materialize(black_box(&rules));
                let sorted_at = Instant::now();
                let summary = increasing_ends(&all);
                black_box(&summary);
                let finished = Instant::now();
                *elapsed = (finished - before).as_micros();
                *summary_elapsed = (finished - sorted_at).as_nanos();
            }
            build_times.sort_unstable();
            summary_times.sort_unstable();
            let mut occupied = Vec::new();
            let mut free = Vec::new();
            // One 15-minute request in each hour of the week, on a fixed,
            // non-chronological permutation so the lookup keys are repeatable.
            for i in 0..168 {
                let hour = (i * 97) % 168;
                let start = SATURDAY + (hour * 60 + (hour * 17) % 45) * MINUTE;
                let window = start..start + 15 * MINUTE;
                let expected = oracle(&slots, &window);
                assert_eq!(from_rules(&rules, &window), expected);
                assert_eq!(ordered_first(&slots, longest, &window), expected);
                assert_eq!(indexed_first(&index, &window), expected);
                if expected.is_some() {
                    occupied.push(window);
                } else {
                    free.push(window);
                }
            }
            // On a miss the start-ordered path inspects every interval in
            // its lookback range. Count these payloads outside the timing.
            let examined: usize = free
                .iter()
                .map(|w| {
                    slots.partition_point(|s| s.start < w.end)
                        - slots.partition_point(|s| s.start <= w.start - longest)
                })
                .sum();
            println!("rules={count} mixed={mixed} slots={} increasing_ends={} build_us={} summary_ns={} ordered_slots_per_miss={:.1}",
                slots.len(), index.len(), build_times[3], summary_times[3], examined as f64 / free.len() as f64);
            for (kind, windows) in [("occupied", occupied), ("free", free)] {
                assert!(!windows.is_empty());
                let direct = ns_per_query(&windows, 1, |w| from_rules(&rules, w));
                let ordered = ns_per_query(&windows, 64, |w| ordered_first(&slots, longest, w));
                let indexed = ns_per_query(&windows, 64, |w| indexed_first(&index, w));
                println!("{kind} windows={} ns/query rules={direct} ordered={ordered} increasing_ends={indexed}", windows.len());
            }
        }
    }
}
