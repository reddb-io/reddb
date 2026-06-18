//! Hash-slot routing primitives for cluster shard keys.
//!
//! The ownership catalog is still expressed as `collection -> range`: ranges are
//! the move/fencing unit. In hash mode, those range bounds are over stable hash
//! slots instead of the raw user shard key. This module is the explicit
//! `shard_key -> hash -> slot -> range-key` layer shared by routing code.

/// Fixed production hash-slot count, matching the cluster slot-map ADR.
pub const PRODUCTION_HASH_SLOT_COUNT: u16 = 16_384;

/// One logical hash bucket in the cluster slot map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HashSlot(u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashSlotError {
    attempted: u16,
}

impl HashSlotError {
    pub fn attempted(self) -> u16 {
        self.attempted
    }
}

impl std::fmt::Display for HashSlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hash slot {} is outside the valid 0..{} range",
            self.attempted, PRODUCTION_HASH_SLOT_COUNT
        )
    }
}

impl std::error::Error for HashSlotError {}

impl HashSlot {
    /// Construct a slot, rejecting values outside `0..PRODUCTION_HASH_SLOT_COUNT`.
    pub fn new(value: u16) -> Result<Self, HashSlotError> {
        if value >= PRODUCTION_HASH_SLOT_COUNT {
            return Err(HashSlotError { attempted: value });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> u16 {
        self.0
    }

    /// The lexicographically-sortable range key used in [`RangeBounds`].
    ///
    /// Big-endian encoding preserves numeric slot order under byte comparison,
    /// so `[slot_a, slot_b)` ranges work with the catalog's existing bounds
    /// predicate.
    ///
    /// [`RangeBounds`]: super::ownership::RangeBounds
    pub fn range_key(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

/// Hash a logical shard key into the fixed production slot map.
pub fn hash_shard_key_to_slot(shard_key: &[u8]) -> HashSlot {
    let digest = blake3::hash(shard_key);
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest.as_bytes()[..8]);
    let slot = (u64::from_be_bytes(prefix) % u64::from(PRODUCTION_HASH_SLOT_COUNT)) as u16;
    HashSlot(slot)
}

/// Hash a logical shard key into the catalog range key used by hash-mode ranges.
pub fn hash_shard_key_to_range_key(shard_key: &[u8]) -> [u8; 2] {
    hash_shard_key_to_slot(shard_key).range_key()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_slots_are_bounded_by_the_production_slot_count() {
        assert_eq!(HashSlot::new(0).unwrap().value(), 0);
        assert_eq!(
            HashSlot::new(PRODUCTION_HASH_SLOT_COUNT - 1)
                .unwrap()
                .value(),
            PRODUCTION_HASH_SLOT_COUNT - 1
        );
        let err = HashSlot::new(PRODUCTION_HASH_SLOT_COUNT).unwrap_err();
        assert_eq!(err.attempted(), PRODUCTION_HASH_SLOT_COUNT);
    }

    #[test]
    fn range_key_preserves_numeric_slot_order() {
        let before = HashSlot::new(255).unwrap().range_key();
        let after = HashSlot::new(256).unwrap().range_key();
        assert!(before < after);
    }

    #[test]
    fn shard_key_hashing_is_stable_and_in_range() {
        let first = hash_shard_key_to_slot(b"tenant:42");
        let second = hash_shard_key_to_slot(b"tenant:42");
        assert_eq!(first, second);
        assert!(first.value() < PRODUCTION_HASH_SLOT_COUNT);
    }
}
