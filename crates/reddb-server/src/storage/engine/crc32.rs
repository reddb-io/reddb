//! CRC32 checksum implementation (IEEE 802.3 polynomial)
//!
//! Used for page integrity verification. Detects corruption on read.
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

/// CRC32 lookup table (pre-computed for polynomial 0xEDB88320)
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

/// Compute CRC32 checksum of data.
///
/// # Example
///
/// ```ignore
/// let checksum = crc32(b"hello world");
/// assert_eq!(checksum, 0x0D4A1185);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFFFFFF_u32;
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// Compute CRC32 checksum, continuing from a previous value.
///
/// Useful for computing checksum in chunks.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut crc = crc ^ 0xFFFFFFFF;
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// Verify data against expected CRC32.
#[inline]
pub fn crc32_verify(data: &[u8], expected: u32) -> bool {
    crc32(data) == expected
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_crc32_table_generation() {
        // Verify first few table entries match expected values
        assert_eq!(CRC32_TABLE[0], 0x00000000);
        assert_eq!(CRC32_TABLE[1], 0x77073096);
        assert_eq!(CRC32_TABLE[2], 0xEE0E612C);
        assert_eq!(CRC32_TABLE[255], 0x2D02EF8D);
    }
}
