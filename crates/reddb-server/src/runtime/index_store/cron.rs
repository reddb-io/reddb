//! Per-zone inverted calendar postings. All five axes contain physical row IDs.
use super::{EntityId, Value};
use reddb_types::cron::index_window_fields;
use roaring::RoaringTreemap;
use std::collections::HashMap;

const ALL: [u64; 5] = [(1 << 60) - 1, (1 << 24) - 1, 0x1ffe, 0xffff_fffe, 0x7f];
// A wildcard owns one posting, even for a schedule firing every minute.
const WILDCARD_BIT: u8 = 63;
const WILDCARD: u64 = 1 << WILDCARD_BIT;

fn bits(mut mask: u64) -> impl Iterator<Item = u8> {
    std::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let bit = mask.trailing_zeros() as u8;
        mask &= mask - 1;
        Some(bit)
    })
}

fn stored_mask(mask: u64, axis: usize) -> u64 {
    if mask == ALL[axis] {
        WILDCARD
    } else {
        mask
    }
}

#[derive(Default)]
struct Partition {
    axes: [HashMap<u8, RoaringTreemap>; 5],
    rows: RoaringTreemap,
}

impl Partition {
    fn matches_axis(&self, axis: usize, mask: u64) -> RoaringTreemap {
        let mut ids = RoaringTreemap::new();
        for bit in bits(mask | WILDCARD) {
            if let Some(posting) = self.axes[axis].get(&bit) {
                ids |= posting;
            }
        }
        ids
    }

    fn candidates(&self, masks: [u64; 5], day_and_weekday: bool) -> RoaringTreemap {
        if masks[0] == 0 {
            return RoaringTreemap::new();
        }
        let unrestricted = |axis: usize| {
            masks[axis] == ALL[axis]
                || (self.axes[axis].len() == 1 && self.axes[axis].contains_key(&WILDCARD_BIT))
        };
        let mut selected: Option<RoaringTreemap> = None;
        let axes = if day_and_weekday { 5 } else { 3 };
        for (axis, &mask) in masks.iter().enumerate().take(axes) {
            if unrestricted(axis) {
                continue;
            }
            let matching = self.matches_axis(axis, mask);
            if let Some(ids) = &mut selected {
                *ids &= matching;
            } else {
                selected = Some(matching);
            }
            if selected.as_ref().is_some_and(RoaringTreemap::is_empty) {
                return RoaringTreemap::new();
            }
        }
        if !day_and_weekday && !unrestricted(3) && !unrestricted(4) {
            let mut dates = self.matches_axis(3, masks[3]);
            dates |= self.matches_axis(4, masks[4]);
            if let Some(ids) = &mut selected {
                *ids &= dates;
            } else {
                selected = Some(dates);
            }
        }
        selected.unwrap_or_else(|| self.rows.clone())
    }
}

#[derive(Default)]
pub(in crate::runtime) struct CronIndex {
    // Separate partitions also preserve cron's AND/OR day semantics.
    partitions: HashMap<(String, bool), Partition>,
    // Untyped writes and schema changes can leave values that raise a SQL error.
    // Keep them for the residual evaluator instead of silently dropping them.
    other: RoaringTreemap,
    memberships: u64,
}

impl CronIndex {
    pub(in crate::runtime) fn growth(value: &Value) -> u64 {
        match value {
            Value::Null => 0,
            Value::Cron(schedule) => {
                let (_, masks, _) = schedule.index_fields();
                let count: u64 = masks
                    .iter()
                    .enumerate()
                    .map(|(axis, mask)| u64::from(stored_mask(*mask, axis).count_ones()))
                    .sum();
                1024 + 256 * count
            }
            _ => 256,
        }
    }

    pub(in crate::runtime) fn memory_bytes(&self) -> u64 {
        let postings: usize = self
            .partitions
            .values()
            .flat_map(|p| &p.axes)
            .map(HashMap::len)
            .sum();
        self.partitions.len() as u64 * 1024
            + postings as u64 * 256
            + (self.memberships + self.other.len()) * 16
    }

    pub(in crate::runtime) fn len(&self) -> u64 {
        self.partitions.values().map(|p| p.rows.len()).sum::<u64>() + self.other.len()
    }

    pub(in crate::runtime) fn update(&mut self, id: EntityId, value: &Value, insert: bool) {
        let id = id.raw();
        let Value::Cron(schedule) = value else {
            if !value.is_null() {
                if insert {
                    self.other.insert(id);
                } else {
                    self.other.remove(id);
                }
            }
            return;
        };
        let (zone, masks, day_and_weekday) = schedule.index_fields();
        let key = (zone.to_owned(), day_and_weekday);
        let partition = if insert {
            self.partitions.entry(key.clone()).or_default()
        } else if let Some(partition) = self.partitions.get_mut(&key) {
            partition
        } else {
            return;
        };
        for (axis, mask) in masks.into_iter().enumerate() {
            let entries = &mut partition.axes[axis];
            for bit in bits(stored_mask(mask, axis)) {
                let changed = if insert {
                    entries.entry(bit).or_default().insert(id)
                } else if let Some(posting) = entries.get_mut(&bit) {
                    let changed = posting.remove(id);
                    if posting.is_empty() {
                        entries.remove(&bit);
                    }
                    changed
                } else {
                    false
                };
                if changed {
                    if insert {
                        self.memberships += 1;
                    } else {
                        self.memberships -= 1;
                    }
                }
            }
        }
        if insert {
            partition.rows.insert(id);
        } else {
            partition.rows.remove(id);
        }
        if partition.rows.is_empty() {
            self.partitions.remove(&key);
        }
    }

    pub(in crate::runtime) fn candidates(&self, start: i64, end: i64) -> Option<Vec<EntityId>> {
        let mut ids = self.other.clone();
        for ((zone, day_and_weekday), partition) in &self.partitions {
            let masks = index_window_fields(zone, start, end)?;
            ids |= partition.candidates(masks, *day_and_weekday);
        }
        // Dense windows are cheaper to scan than to materialize and look up by ID.
        if ids.len() > 65_536 || (ids.len() > 128 && ids.len() > self.len() / 2) {
            return None;
        }
        Some(ids.iter().map(EntityId::new).collect())
    }
}
