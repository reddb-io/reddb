//! Dual-superblock checkpoint contract.
//!
//! Mirrors the engine's embedded `.rdb` two-slot superblock (see
//! `reddb_file::embedded`): two fixed-size slots, each carrying a generation,
//! the highest durably-committed LSN at that checkpoint, and a CRC. Checkpoints
//! alternate slots and fsync, so at most one slot is ever mid-write — recovery
//! always finds at least one intact slot once any checkpoint has completed.

/// Bytes per superblock slot (fixed; the rest is zero padding).
pub const SLOT_BYTES: usize = 64;
/// Two slots back to back.
pub const SUPERBLOCK_BYTES: usize = SLOT_BYTES * 2;
const MAGIC: &[u8; 8] = b"DSTSBLK1";

/// A decoded superblock slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub generation: u64,
    pub committed_lsn: u64,
}

impl Superblock {
    /// Encode this slot into its fixed-size byte form (magic, fields, CRC, pad).
    pub fn encode(&self) -> [u8; SLOT_BYTES] {
        let mut slot = [0u8; SLOT_BYTES];
        slot[0..8].copy_from_slice(MAGIC);
        slot[8..16].copy_from_slice(&self.generation.to_le_bytes());
        slot[16..24].copy_from_slice(&self.committed_lsn.to_le_bytes());
        let crc = crc32(&slot[0..24]);
        slot[24..28].copy_from_slice(&crc.to_le_bytes());
        slot
    }

    /// Decode a slot, returning `None` if the magic or CRC do not match (a torn
    /// or never-written slot).
    pub fn decode(slot: &[u8]) -> Option<Self> {
        if slot.len() < 28 || &slot[0..8] != MAGIC {
            return None;
        }
        let stored_crc = u32::from_le_bytes([slot[24], slot[25], slot[26], slot[27]]);
        if crc32(&slot[0..24]) != stored_crc {
            return None;
        }
        let generation = u64::from_le_bytes(slot[8..16].try_into().ok()?);
        let committed_lsn = u64::from_le_bytes(slot[16..24].try_into().ok()?);
        Some(Self {
            generation,
            committed_lsn,
        })
    }
}

/// The byte offset of the slot used by the given generation (alternating).
pub fn slot_offset(generation: u64) -> u64 {
    (generation % 2) * SLOT_BYTES as u64
}

/// Recover the authoritative superblock from the two raw slot regions: the
/// highest-generation slot whose CRC is intact. `None` means neither slot is
/// valid (no checkpoint has completed yet).
pub fn recover(bytes: &[u8]) -> Option<Superblock> {
    let mut best: Option<Superblock> = None;
    for slot in 0..2usize {
        let start = slot * SLOT_BYTES;
        let end = start + SLOT_BYTES;
        if end > bytes.len() {
            continue;
        }
        if let Some(sb) = Superblock::decode(&bytes[start..end]) {
            if best.is_none_or(|b| sb.generation > b.generation) {
                best = Some(sb);
            }
        }
    }
    best
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_decodes() {
        let sb = Superblock {
            generation: 5,
            committed_lsn: 42,
        };
        let bytes = sb.encode();
        assert_eq!(Superblock::decode(&bytes), Some(sb));
    }

    #[test]
    fn torn_slot_rejected() {
        let sb = Superblock {
            generation: 1,
            committed_lsn: 1,
        };
        let mut bytes = sb.encode();
        bytes[20] ^= 0xFF; // corrupt the committed_lsn field
        assert_eq!(Superblock::decode(&bytes), None);
    }

    #[test]
    fn slots_alternate() {
        assert_eq!(slot_offset(0), 0);
        assert_eq!(slot_offset(1), SLOT_BYTES as u64);
        assert_eq!(slot_offset(2), 0);
        assert_eq!(slot_offset(3), SLOT_BYTES as u64);
    }

    #[test]
    fn recover_picks_highest_intact_generation() {
        let mut img = vec![0u8; SUPERBLOCK_BYTES];
        let gen0 = Superblock {
            generation: 4,
            committed_lsn: 40,
        };
        let gen1 = Superblock {
            generation: 5,
            committed_lsn: 50,
        };
        img[0..SLOT_BYTES].copy_from_slice(&gen0.encode());
        img[SLOT_BYTES..].copy_from_slice(&gen1.encode());
        assert_eq!(recover(&img), Some(gen1));

        // Corrupt the newer slot: recovery falls back to the older intact one.
        img[SLOT_BYTES + 18] ^= 0xFF;
        assert_eq!(recover(&img), Some(gen0));
    }

    #[test]
    fn recover_none_when_empty() {
        assert_eq!(recover(&[0u8; SUPERBLOCK_BYTES]), None);
        assert_eq!(recover(&[]), None);
    }
}
