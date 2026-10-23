//! CRC32 checksum implementation (IEEE 802.3 polynomial), SIMD-accelerated.
//!
//! Used for page and WAL-record integrity verification. Detects corruption
//! on read.
//!
//! # Algorithm
//!
//! CRC-32/ISO-HDLC (polynomial 0x04C11DB7, reflected):
//! - Input reflected
//! - Output reflected
//! - Initial value: 0xFFFFFFFF
//! - Final XOR: 0xFFFFFFFF
//!
//! This matches zlib/gzip/PNG CRC32.
//!
//! # Implementation
//!
//! Backed by [`crc32fast`], which performs runtime CPU-feature detection and
//! selects a SIMD/PCLMULQDQ folding implementation (carry-less multiply over
//! the reflected IEEE polynomial) when the host supports it, falling back to a
//! table-based scalar routine otherwise. The polynomial, reflection, initial
//! value and final XOR are identical to the previous hand-rolled
//! byte-at-a-time software CRC32, so checksums are **byte-for-byte identical**:
//! WAL records (and pages) written before and after this change remain mutually
//! verifiable and the on-disk format — magic, version byte, layout — is
//! unchanged. See issue #883 / PRD #882.

/// Compute CRC32 checksum of data.
///
/// # Example
///
/// ```ignore
/// let checksum = crc32(b"hello world");
/// assert_eq!(checksum, 0x0D4A1185);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

/// Compute CRC32 checksum, continuing from a previous value.
///
/// `crc` is a previously finalized CRC32 (as returned by [`crc32`] or an
/// earlier `crc32_update`). Useful for computing a checksum in chunks; the
/// result is identical to hashing the concatenated input in one call.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(crc);
    hasher.update(data);
    hasher.finalize()
}

/// Verify data against expected CRC32.
#[inline]
pub fn crc32_verify(data: &[u8], expected: u32) -> bool {
    crc32(data) == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-computed lookup table for the previous hand-rolled software CRC32
    /// (polynomial 0xEDB88320). Retained here as the byte-parity reference the
    /// SIMD implementation is checked against.
    const CRC32_TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut j = 0;
            while j < 8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB88320;
                } else {
                    crc >>= 1;
                }
                j += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    };

    /// The previous byte-at-a-time software CRC32, used only as the parity
    /// oracle for the SIMD implementation.
    fn crc32_scalar_reference(data: &[u8]) -> u32 {
        let mut crc = 0xFFFFFFFF_u32;
        for &byte in data {
            let index = ((crc ^ byte as u32) & 0xFF) as usize;
            crc = CRC32_TABLE[index] ^ (crc >> 8);
        }
        crc ^ 0xFFFFFFFF
    }

    /// Deterministic pseudo-random corpus byte (no external rng dependency).
    fn corpus_byte(seed: u64) -> u8 {
        // splitmix64-style mixing — deterministic and well-distributed.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as u8
    }

    #[test]
    fn test_crc32_empty() {
        assert_eq!(crc32(b""), 0x00000000);
    }

    #[test]
    fn test_crc32_known_values() {
        // Standard test vectors
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b"hello"), 0x3610A686);
        assert_eq!(crc32(b"Hello, World!"), 0xEC4AC3D0);
    }

    #[test]
    fn test_crc32_single_byte() {
        // CRC32 values - just verify they're different and non-zero
        let crc_a = crc32(b"a");
        let crc_z = crc32(b"Z");
        assert_ne!(crc_a, 0);
        assert_ne!(crc_z, 0);
        assert_ne!(crc_a, crc_z);
    }

    #[test]
    fn test_crc32_zeros() {
        assert_eq!(crc32(&[0u8; 1]), 0xD202EF8D);
        assert_eq!(crc32(&[0u8; 4]), 0x2144DF1C);
        assert_eq!(crc32(&[0u8; 32]), 0x190A55AD);
    }

    #[test]
    fn test_crc32_all_ones() {
        assert_eq!(crc32(&[0xFFu8; 1]), 0xFF000000);
        assert_eq!(crc32(&[0xFFu8; 4]), 0xFFFFFFFF);
    }

    #[test]
    fn test_crc32_incremental() {
        let data = b"hello world";
        let full = crc32(data);

        // Same result when computed in parts
        let part1 = crc32(b"hello ");
        let part2 = crc32_update(part1, b"world");
        assert_eq!(full, part2);
    }

    #[test]
    fn test_crc32_verify() {
        let data = b"test data";
        let checksum = crc32(data);
        assert!(crc32_verify(data, checksum));
        assert!(!crc32_verify(data, checksum + 1));
    }

    /// Byte-parity: the SIMD implementation must produce identical checksums
    /// to the previous software implementation across a corpus spanning the
    /// SIMD folding boundaries (sub-16, single-block, multi-block, tail).
    #[test]
    fn simd_matches_software_across_corpus() {
        // Length 0 through 600 exercises the <16-byte scalar tail, the
        // 16-byte PCLMULQDQ block, and many full + partial folding rounds.
        for len in 0..=600usize {
            let data: Vec<u8> = (0..len as u64).map(corpus_byte).collect();
            assert_eq!(
                crc32(&data),
                crc32_scalar_reference(&data),
                "SIMD/software CRC32 diverged at len={len}"
            );
        }
    }

    /// The known CRC32 test vectors must match the software reference too,
    /// closing acceptance criterion #2 (corpus + known vectors).
    #[test]
    fn simd_matches_software_on_known_vectors() {
        for v in [
            &b""[..],
            &b"123456789"[..],
            &b"hello"[..],
            &b"Hello, World!"[..],
            &[0u8; 32][..],
            &[0xFFu8; 17][..],
        ] {
            assert_eq!(crc32(v), crc32_scalar_reference(v));
        }
    }

    /// Chunked `crc32_update` must equal a single-shot `crc32` regardless of
    /// where the input is split — the property the WAL reader relies on when
    /// it folds the running CRC field-by-field.
    #[test]
    fn crc32_update_chunking_is_split_invariant() {
        let data: Vec<u8> = (0..512u64).map(corpus_byte).collect();
        let expected = crc32(&data);
        for split in 0..=data.len() {
            let (head, tail) = data.split_at(split);
            let crc = crc32_update(crc32(head), tail);
            assert_eq!(crc, expected, "chunk split at {split} diverged");
        }
    }

    #[test]
    fn test_crc32_table_generation() {
        // Verify first few reference-table entries match expected values
        assert_eq!(CRC32_TABLE[0], 0x00000000);
        assert_eq!(CRC32_TABLE[1], 0x77073096);
        assert_eq!(CRC32_TABLE[2], 0xEE0E612C);
        assert_eq!(CRC32_TABLE[255], 0x2D02EF8D);
    }
}
