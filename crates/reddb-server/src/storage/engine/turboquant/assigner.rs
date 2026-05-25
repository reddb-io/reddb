//! Block placement for the TurboQuant blocked-by-32 layout.
//!
//! Trivial deep module: callers ask "where does the next vector go?"
//! and the assigner answers without exposing block bookkeeping. Split
//! out from [`super::storage::BlockedCodeStorage`] so the placement
//! policy can be reasoned about and unit-tested independently of the
//! aligned-buffer machinery.

use super::storage::BLOCK_LANES;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlockPlacement {
    pub block_idx: u32,
    pub lane: u8,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BlockAssigner;

impl BlockAssigner {
    pub fn new() -> Self {
        Self
    }

    /// Decide where the next vector should be written.
    ///
    /// - `n_blocks == 0` ⇒ open block 0 at lane 0.
    /// - trailing block has `< BLOCK_LANES` lanes filled ⇒ append at the
    ///   next free lane in that block.
    /// - trailing block is full ⇒ open a new block at lane 0.
    pub fn next_placement(&self, n_blocks: usize, trailing_lanes_filled: usize) -> BlockPlacement {
        if n_blocks == 0 {
            return BlockPlacement {
                block_idx: 0,
                lane: 0,
            };
        }
        if trailing_lanes_filled < BLOCK_LANES {
            return BlockPlacement {
                block_idx: (n_blocks - 1) as u32,
                lane: trailing_lanes_filled as u8,
            };
        }
        BlockPlacement {
            block_idx: n_blocks as u32,
            lane: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_assigns_block_zero_lane_zero() {
        let placement = BlockAssigner::new().next_placement(0, 0);
        assert_eq!(
            placement,
            BlockPlacement {
                block_idx: 0,
                lane: 0
            }
        );
    }

    #[test]
    fn partial_block_assigns_next_lane() {
        let assigner = BlockAssigner::new();
        for filled in 1..BLOCK_LANES {
            let placement = assigner.next_placement(1, filled);
            assert_eq!(
                placement,
                BlockPlacement {
                    block_idx: 0,
                    lane: filled as u8,
                },
                "partial trailing block, {filled} lanes filled",
            );
        }

        let placement = assigner.next_placement(3, 7);
        assert_eq!(
            placement,
            BlockPlacement {
                block_idx: 2,
                lane: 7
            }
        );
    }

    #[test]
    fn full_block_opens_new_block_at_lane_zero() {
        let placement = BlockAssigner::new().next_placement(1, BLOCK_LANES);
        assert_eq!(
            placement,
            BlockPlacement {
                block_idx: 1,
                lane: 0
            }
        );

        let placement = BlockAssigner::new().next_placement(4, BLOCK_LANES);
        assert_eq!(
            placement,
            BlockPlacement {
                block_idx: 4,
                lane: 0
            }
        );
    }
}
