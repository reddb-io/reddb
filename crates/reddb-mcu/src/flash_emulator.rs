//! Host-side flash behavior model behind the ordinary `VfsFile` boundary.
//! It models capacity and wear; it is not a CPU or filesystem emulator.
use crate::{BoardProfile, VfsFile};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, SeekFrom},
    sync::{Arc, Mutex, MutexGuard},
};

pub const ERASE_BLOCK_BYTES: usize = 4096;
pub const PROGRAM_UNIT_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashGeometry {
    pub capacity_bytes: usize,
    pub erase_block_bytes: usize,
    pub program_unit_bytes: usize,
}

impl FlashGeometry {
    pub const fn for_board(board: BoardProfile) -> Self {
        Self {
            capacity_bytes: board.database_bytes,
            erase_block_bytes: ERASE_BLOCK_BYTES,
            program_unit_bytes: PROGRAM_UNIT_BYTES,
        }
    }

    pub const fn with_capacity(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            erase_block_bytes: ERASE_BLOCK_BYTES,
            program_unit_bytes: PROGRAM_UNIT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlashStats {
    pub logical_bytes: u64,
    pub physical_bytes: usize,
    pub capacity_bytes: usize,
    pub allocated_blocks: usize,
    pub reserved_blocks: usize,
    pub write_bytes: u64,
    pub program_bytes: u64,
    pub program_operations: u64,
    pub erase_operations: u64,
    pub sync_operations: u64,
}

#[derive(Clone, Default)]
struct Image {
    len: u64,
    blocks: BTreeMap<u64, Box<[u8]>>,
    reservations: BTreeSet<u64>,
    physical_blocks: usize,
}

#[derive(Default)]
struct Counters {
    write_bytes: u64,
    program_bytes: u64,
    program_operations: u64,
    erase_operations: u64,
    sync_operations: u64,
}

struct State {
    geometry: FlashGeometry,
    live: Image,
    durable: Image,
    dirty_blocks: BTreeSet<u64>,
    dirty_reservations: BTreeSet<u64>,
    counters: Counters,
}

pub struct FlashEmulatorFile {
    state: Arc<Mutex<State>>,
    position: u64,
}

impl FlashEmulatorFile {
    pub fn for_board(board: BoardProfile) -> Self {
        Self::new(FlashGeometry::for_board(board))
    }

    pub fn new(geometry: FlashGeometry) -> Self {
        assert!(geometry.erase_block_bytes > 0);
        assert!(geometry.program_unit_bytes > 0);
        assert!(geometry.capacity_bytes >= geometry.erase_block_bytes);
        assert!(geometry
            .erase_block_bytes
            .is_multiple_of(geometry.program_unit_bytes));
        Self {
            state: Arc::new(Mutex::new(State {
                geometry,
                live: Image::default(),
                durable: Image::default(),
                dirty_blocks: BTreeSet::new(),
                dirty_reservations: BTreeSet::new(),
                counters: Counters::default(),
            })),
            position: 0,
        }
    }

    pub fn stats(&self) -> io::Result<FlashStats> {
        let state = self.lock()?;
        Ok(state.stats())
    }

    /// Discard writes after the last sync and retain cumulative wear counters.
    ///
    /// This is a host-model restart. A real adapter must additionally prove
    /// controller and filesystem behavior under power loss.
    pub fn power_cycle(mut self) -> io::Result<Self> {
        {
            let mut state = self.lock()?;
            state.live = state.durable.clone();
            state.dirty_blocks.clear();
            state.dirty_reservations.clear();
        }
        self.position = 0;
        Ok(self)
    }

    fn lock(&self) -> io::Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("flash emulator lock poisoned"))
    }
}

impl State {
    fn block_bytes(&self) -> usize {
        self.geometry.erase_block_bytes
    }

    fn max_blocks(&self) -> usize {
        self.geometry.capacity_bytes / self.block_bytes()
    }

    fn physical_blocks(&self) -> usize {
        self.live.physical_blocks
    }

    fn stats(&self) -> FlashStats {
        FlashStats {
            logical_bytes: self.live.len,
            physical_bytes: self.physical_blocks() * self.block_bytes(),
            capacity_bytes: self.geometry.capacity_bytes,
            allocated_blocks: self.live.blocks.len(),
            reserved_blocks: self.live.reservations.len(),
            write_bytes: self.counters.write_bytes,
            program_bytes: self.counters.program_bytes,
            program_operations: self.counters.program_operations,
            erase_operations: self.counters.erase_operations,
            sync_operations: self.counters.sync_operations,
        }
    }

    fn programmed_units(&self, bytes: &[u8]) -> usize {
        bytes
            .chunks(self.geometry.program_unit_bytes)
            .filter(|unit| unit.iter().any(|byte| *byte != 0))
            .count()
    }

    fn apply_block(&mut self, index: u64, next: Option<Box<[u8]>>) {
        let occupied_before =
            self.live.blocks.contains_key(&index) || self.live.reservations.contains(&index);
        let occupied_after = next.is_some();
        let changed = match (self.live.blocks.get(&index), next.as_deref()) {
            (None, None) => false,
            (Some(current), Some(next)) => current.as_ref() != next,
            _ => true,
        };
        if changed {
            if self.live.blocks.contains_key(&index) {
                self.counters.erase_operations += 1;
            }
            if let Some(bytes) = next.as_deref() {
                let units = self.programmed_units(bytes) as u64;
                self.counters.program_operations += units;
                self.counters.program_bytes += units * self.geometry.program_unit_bytes as u64;
            }
            match next {
                Some(bytes) => {
                    self.live.blocks.insert(index, bytes);
                }
                None => {
                    self.live.blocks.remove(&index);
                }
            }
            self.dirty_blocks.insert(index);
        }
        if self.live.reservations.remove(&index) {
            self.dirty_reservations.insert(index);
        }
        self.live.physical_blocks = match (occupied_before, occupied_after) {
            (false, true) => self.live.physical_blocks.saturating_add(1),
            (true, false) => self.live.physical_blocks.saturating_sub(1),
            _ => self.live.physical_blocks,
        };
    }
}

impl VfsFile for FlashEmulatorFile {
    fn max_read_buffer_bytes(&self) -> usize {
        crate::engine::storage::page::PAGE_SIZE
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            state: Arc::clone(&self.state),
            position: self.position,
        })
    }

    fn same_file(&self, other: &Self) -> io::Result<bool> {
        Ok(Arc::ptr_eq(&self.state, &other.state))
    }

    fn file_len(&self) -> io::Result<u64> {
        Ok(self.lock()?.live.len)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        let mut state = self.lock()?;
        if len < state.live.len {
            let block_bytes = state.block_bytes() as u64;
            let retained_blocks = len.div_ceil(block_bytes);
            let removed: Vec<_> = state
                .live
                .blocks
                .range(retained_blocks..)
                .map(|(index, _)| *index)
                .collect();
            for index in removed {
                state.apply_block(index, None);
            }
            let reservations: Vec<_> = state
                .live
                .reservations
                .range(retained_blocks..)
                .copied()
                .collect();
            for index in reservations {
                if state.live.reservations.remove(&index) {
                    state.live.physical_blocks = state.live.physical_blocks.saturating_sub(1);
                    state.dirty_reservations.insert(index);
                }
            }
            if !len.is_multiple_of(block_bytes) {
                let index = len / block_bytes;
                if let Some(current) = state.live.blocks.get(&index) {
                    let mut next = current.clone();
                    next[to_usize(len % block_bytes)?..].fill(0);
                    let next = next.iter().any(|byte| *byte != 0).then_some(next);
                    state.apply_block(index, next);
                }
            }
        }
        state.live.len = len;
        Ok(())
    }

    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        let end = offset.checked_add(len).ok_or(io::ErrorKind::InvalidInput)?;
        let mut state = self.lock()?;
        let block_bytes = state.block_bytes() as u64;
        let first = offset / block_bytes;
        let last = (end - 1) / block_bytes;
        let missing = (first..=last)
            .filter(|index| {
                !state.live.blocks.contains_key(index) && !state.live.reservations.contains(index)
            })
            .count();
        if state.physical_blocks().saturating_add(missing) > state.max_blocks() {
            return Err(storage_full());
        }
        for index in first..=last {
            if !state.live.blocks.contains_key(&index) && state.live.reservations.insert(index) {
                state.live.physical_blocks += 1;
                state.dirty_reservations.insert(index);
            }
        }
        Ok(())
    }

    fn write_all(&mut self, buffer: &[u8]) -> io::Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        let end = self
            .position
            .checked_add(buffer.len() as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let mut state = self.lock()?;
        let block_bytes = state.block_bytes();
        let first = self.position / block_bytes as u64;
        let last = (end - 1) / block_bytes as u64;
        let mut candidates = Vec::with_capacity(to_usize(last - first + 1)?);
        for index in first..=last {
            let mut next = state
                .live
                .blocks
                .get(&index)
                .cloned()
                .unwrap_or_else(|| vec![0; block_bytes].into_boxed_slice());
            let block_start = index * block_bytes as u64;
            let write_start = self.position.max(block_start);
            let write_end = end.min(block_start + block_bytes as u64);
            let source_start = to_usize(write_start - self.position)?;
            let target_start = to_usize(write_start - block_start)?;
            let count = to_usize(write_end - write_start)?;
            next[target_start..target_start + count]
                .copy_from_slice(&buffer[source_start..source_start + count]);
            candidates.push((index, next.iter().any(|byte| *byte != 0).then_some(next)));
        }
        let mut final_blocks = state.physical_blocks();
        for (index, next) in &candidates {
            let before =
                state.live.blocks.contains_key(index) || state.live.reservations.contains(index);
            let after = next.is_some();
            final_blocks = match (before, after) {
                (false, true) => final_blocks.saturating_add(1),
                (true, false) => final_blocks.saturating_sub(1),
                _ => final_blocks,
            };
        }
        if final_blocks > state.max_blocks() {
            return Err(storage_full());
        }
        for (index, next) in candidates {
            state.apply_block(index, next);
        }
        state.live.len = state.live.len.max(end);
        state.counters.write_bytes = state
            .counters
            .write_bytes
            .saturating_add(buffer.len() as u64);
        drop(state);
        self.position = end;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let state = self.lock()?;
        let count = usize::try_from(state.live.len.saturating_sub(self.position))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        buffer[..count].fill(0);
        let block_bytes = state.block_bytes() as u64;
        let end = self.position + count as u64;
        if count != 0 {
            for index in self.position / block_bytes..=(end - 1) / block_bytes {
                let Some(block) = state.live.blocks.get(&index) else {
                    continue;
                };
                let block_start = index * block_bytes;
                let read_start = self.position.max(block_start);
                let read_end = end.min(block_start + block_bytes);
                let source_start = to_usize(read_start - block_start)?;
                let target_start = to_usize(read_start - self.position)?;
                let bytes = to_usize(read_end - read_start)?;
                buffer[target_start..target_start + bytes]
                    .copy_from_slice(&block[source_start..source_start + bytes]);
            }
        }
        drop(state);
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.file_len()?.checked_add_signed(offset),
        }
        .ok_or(io::ErrorKind::InvalidInput)?;
        self.position = next;
        Ok(next)
    }

    fn sync_all(&self) -> io::Result<()> {
        let mut state = self.lock()?;
        let dirty_blocks = std::mem::take(&mut state.dirty_blocks);
        for index in dirty_blocks {
            match state.live.blocks.get(&index).cloned() {
                Some(block) => {
                    state.durable.blocks.insert(index, block);
                }
                None => {
                    state.durable.blocks.remove(&index);
                }
            }
        }
        let dirty_reservations = std::mem::take(&mut state.dirty_reservations);
        for index in dirty_reservations {
            if state.live.reservations.contains(&index) {
                state.durable.reservations.insert(index);
            } else {
                state.durable.reservations.remove(&index);
            }
        }
        state.durable.len = state.live.len;
        state.durable.physical_blocks = state.live.physical_blocks;
        state.counters.sync_operations += 1;
        Ok(())
    }
}

fn storage_full() -> io::Error {
    io::Error::new(
        io::ErrorKind::WriteZero,
        "emulated flash capacity exhausted",
    )
}

fn to_usize(value: u64) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| io::ErrorKind::InvalidInput.into())
}
