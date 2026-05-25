//! Blocked-by-32 encoded-codes storage for TurboQuant.
//!
//! ADR 0024 makes blocked-by-32 the canonical encoded-codes layout for
//! the TurboQuant index: codes for 32 consecutive vectors are
//! pre-interleaved with the PERM0 permutation into one packed buffer
//! per block, with a trailing partial block at the end. SIMD scoring
//! kernels (NEON / AVX2 / AVX-512BW, added in later slices) read
//! aligned register-width slices straight from these buffers with no
//! per-query repack.
//!
//! MIT notice: the PERM0 layout is the upstream RyanCodrai/turbovec
//! shape (commit `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`, MIT). The
//! incremental insert path and the SIMD-free decode helpers are a
//! clean-room RedDB implementation.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::NonNull;

use super::assigner::{BlockAssigner, BlockPlacement};

/// Vectors per block. Matches the upstream turbovec block width and is
/// the widest contiguous lane group every supported SIMD kernel
/// (NEON 128b, AVX2 256b, AVX-512BW 512b) can consume.
pub const BLOCK_LANES: usize = 32;

/// Alignment required on every `block_codes` slice handed out. 64 bytes
/// is the widest SIMD load this index will ever issue (AVX-512BW, slice
/// #672); aligning here lets later kernels use aligned loads on every
/// supported target without re-walking the buffer.
pub const SIMD_ALIGN: usize = 64;

/// PERM0 permutation used by the upstream turbovec layout. Within each
/// byte group, the 32 lanes are split into two halves of 16 and
/// reordered by this permutation so that AVX2's `vpshufb` / NEON's
/// `vqtbl1q_u8` can table-lookup hi and lo nibbles in lockstep.
pub const PERM0: [usize; 16] = [0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15];

/// Manually-aligned heap buffer. `Vec<u8>` only guarantees alignment of
/// the element type (1 byte), but the SIMD kernels need 64-byte
/// alignment. This is the smallest possible wrapper that gives us that
/// without taking on a new dependency.
struct AlignedBlock {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl AlignedBlock {
    fn zeroed(size: usize) -> Self {
        let layout = Layout::from_size_align(size.max(SIMD_ALIGN), SIMD_ALIGN)
            .expect("aligned-block layout");
        // SAFETY: `layout` has size > 0 (size is rounded up to SIMD_ALIGN).
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).expect("aligned alloc must not return null");
        Self { ptr, layout }
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `self.ptr` is a valid, initialized, `layout.size()`-byte
        // allocation owned by this struct for its lifetime.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as in `as_slice`, with exclusive access through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for AlignedBlock {
    fn drop(&mut self) {
        // SAFETY: pairs with the `alloc_zeroed` in `zeroed`.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

// SAFETY: the buffer is owned exclusively by `AlignedBlock`; sharing it
// across threads is the standard `Send`/`Sync` story for `Box<[u8]>`.
unsafe impl Send for AlignedBlock {}
unsafe impl Sync for AlignedBlock {}

impl std::fmt::Debug for AlignedBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBlock")
            .field("size", &self.layout.size())
            .field("align", &self.layout.align())
            .finish()
    }
}

/// Handle for a single vector's encoded codes. Replaces the per-vector
/// `packed: Vec<u8>` ownership of the rejected layout from ADR 0024.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHandle {
    pub block_idx: u32,
    pub lane: u8,
}

/// Owns the encoded-codes storage for a TurboQuant collection.
///
/// Deep module: the only surface callers need is `append`, plus a few
/// read accessors used by the scoring kernels. The PERM0 interleave,
/// 64-byte alignment, and block-fill bookkeeping all live behind this
/// struct.
#[derive(Debug)]
pub struct BlockedCodeStorage {
    n_byte_groups: usize,
    blocks: Vec<AlignedBlock>,
    /// `1..=BLOCK_LANES` per block. The last entry may be `< BLOCK_LANES`
    /// (partial-block tail); every earlier entry is exactly `BLOCK_LANES`
    /// by construction.
    lanes_filled: Vec<u8>,
    /// Per-lane scale (`l2_norm` of the input vector). Held alongside
    /// the codes because every scoring path needs both together.
    scales: Vec<[f32; BLOCK_LANES]>,
}

impl BlockedCodeStorage {
    pub fn new(n_byte_groups: usize) -> Self {
        Self {
            n_byte_groups,
            blocks: Vec::new(),
            lanes_filled: Vec::new(),
            scales: Vec::new(),
        }
    }

    pub fn n_byte_groups(&self) -> usize {
        self.n_byte_groups
    }

    pub fn n_blocks(&self) -> usize {
        self.blocks.len()
    }

    pub fn n_vectors(&self) -> usize {
        self.lanes_filled.iter().map(|&n| n as usize).sum()
    }

    pub fn block_lanes_filled(&self, block_idx: usize) -> usize {
        self.lanes_filled[block_idx] as usize
    }

    /// Returns the raw PERM0-packed codes for `block_idx`. Guaranteed
    /// to be aligned to [`SIMD_ALIGN`] (64 bytes); SIMD kernels can
    /// load aligned register-width slices directly.
    pub fn block_codes(&self, block_idx: usize) -> &[u8] {
        self.blocks[block_idx].as_slice()
    }

    pub fn lane_scale(&self, block_idx: usize, lane: usize) -> f32 {
        self.scales[block_idx][lane]
    }

    /// Append a vector's per-vector packed bytes (`lo | hi << 4` per
    /// byte group, in dim-major order) to the open partial block,
    /// opening a new block if the trailing block is full.
    pub fn append(&mut self, packed: &[u8], scale: f32) -> BlockHandle {
        assert_eq!(
            packed.len(),
            self.n_byte_groups,
            "per-vector packed length must match codec's n_byte_groups"
        );
        let trailing = self.lanes_filled.last().copied().unwrap_or(0) as usize;
        let placement = BlockAssigner::new().next_placement(self.blocks.len(), trailing);
        if placement.lane == 0 {
            // Open a new block.
            self.blocks
                .push(AlignedBlock::zeroed(self.n_byte_groups * BLOCK_LANES));
            self.lanes_filled.push(0);
            self.scales.push([0.0; BLOCK_LANES]);
        }
        let block_idx = placement.block_idx as usize;
        let lane = placement.lane as usize;
        self.write_lane(block_idx, lane, packed);
        self.scales[block_idx][lane] = scale;
        self.lanes_filled[block_idx] += 1;
        BlockHandle {
            block_idx: placement.block_idx,
            lane: placement.lane,
        }
    }

    /// Decode the per-vector packed bytes that were written at
    /// `(block_idx, lane)`. The returned bytes match the original
    /// `packed` argument from [`Self::append`] exactly — PERM0 is
    /// fully internal to the storage layer.
    pub fn decode_lane(&self, block_idx: usize, lane: usize) -> Vec<u8> {
        let (perm_pos, half) = lane_to_perm(lane);
        let buf = self.blocks[block_idx].as_slice();
        let mut out = vec![0u8; self.n_byte_groups];
        for (g, slot) in out.iter_mut().enumerate() {
            let group_base = g * BLOCK_LANES;
            let hi_pair = buf[group_base + perm_pos];
            let lo_pair = buf[group_base + 16 + perm_pos];
            let (hi_nibble, lo_nibble) = if half == 0 {
                (hi_pair & 0x0f, lo_pair & 0x0f)
            } else {
                (hi_pair >> 4, lo_pair >> 4)
            };
            *slot = lo_nibble | (hi_nibble << 4);
        }
        out
    }

    fn write_lane(&mut self, block_idx: usize, lane: usize, packed: &[u8]) {
        let (perm_pos, half) = lane_to_perm(lane);
        let buf = self.blocks[block_idx].as_mut_slice();
        for (g, &byte) in packed.iter().enumerate() {
            let lo = byte & 0x0f;
            let hi = byte >> 4;
            let group_base = g * BLOCK_LANES;
            let hi_idx = group_base + perm_pos;
            let lo_idx = group_base + 16 + perm_pos;
            if half == 0 {
                buf[hi_idx] = (buf[hi_idx] & 0xf0) | hi;
                buf[lo_idx] = (buf[lo_idx] & 0xf0) | lo;
            } else {
                buf[hi_idx] = (buf[hi_idx] & 0x0f) | (hi << 4);
                buf[lo_idx] = (buf[lo_idx] & 0x0f) | (lo << 4);
            }
        }
    }
}

fn lane_to_perm(lane: usize) -> (usize, usize) {
    debug_assert!(lane < BLOCK_LANES);
    let half = lane / 16;
    let within_half = lane % 16;
    let perm_pos = PERM0
        .iter()
        .position(|&v| v == within_half)
        .expect("lane must be present in perm0");
    (perm_pos, half)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_packed(seed: usize, n_byte_groups: usize) -> Vec<u8> {
        (0..n_byte_groups)
            .map(|g| {
                let lo = ((seed + g) & 0x0f) as u8;
                let hi = ((seed * 3 + g * 5) & 0x0f) as u8;
                lo | (hi << 4)
            })
            .collect()
    }

    #[test]
    fn round_trip_matches_original_for_required_sizes() {
        let n_byte_groups = 7;
        for n in [1usize, 31, 32, 33, 95, 96, 97] {
            let mut storage = BlockedCodeStorage::new(n_byte_groups);
            let mut originals = Vec::with_capacity(n);
            for i in 0..n {
                let packed = synth_packed(i, n_byte_groups);
                let h = storage.append(&packed, i as f32);
                assert_eq!(
                    h.block_idx as usize,
                    i / BLOCK_LANES,
                    "block placement for vector {i}"
                );
                assert_eq!(
                    h.lane as usize,
                    i % BLOCK_LANES,
                    "lane placement for vector {i}"
                );
                originals.push(packed);
            }
            assert_eq!(storage.n_vectors(), n);
            let expected_blocks = n.div_ceil(BLOCK_LANES);
            assert_eq!(storage.n_blocks(), expected_blocks);

            for i in 0..n {
                let decoded = storage.decode_lane(i / BLOCK_LANES, i % BLOCK_LANES);
                assert_eq!(decoded, originals[i], "round-trip for vector {i}, N={n}");
            }
        }
    }

    #[test]
    fn block_codes_slices_are_aligned_to_simd_alignment() {
        let n_byte_groups = 5;
        let mut storage = BlockedCodeStorage::new(n_byte_groups);
        for i in 0..(2 * BLOCK_LANES + 5) {
            storage.append(&synth_packed(i, n_byte_groups), 1.0);
        }
        assert_eq!(storage.n_blocks(), 3);
        for b in 0..storage.n_blocks() {
            let slice = storage.block_codes(b);
            assert_eq!(
                slice.len(),
                n_byte_groups * BLOCK_LANES,
                "block {b} sized to (n_byte_groups * lanes)"
            );
            assert_eq!(
                (slice.as_ptr() as usize) % SIMD_ALIGN,
                0,
                "block {b} aligned to {SIMD_ALIGN}"
            );
        }
    }

    #[test]
    fn unused_lanes_in_partial_block_decode_to_zero_bytes() {
        let n_byte_groups = 3;
        let mut storage = BlockedCodeStorage::new(n_byte_groups);
        storage.append(&synth_packed(7, n_byte_groups), 1.0);
        assert_eq!(storage.block_lanes_filled(0), 1);
        for lane in 1..BLOCK_LANES {
            assert_eq!(storage.decode_lane(0, lane), vec![0u8; n_byte_groups]);
        }
    }
}
