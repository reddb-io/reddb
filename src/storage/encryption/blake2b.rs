//! BLAKE2b Hash Function (RFC 7693)
//!
//! BLAKE2b is a cryptographic hash function faster than MD5, SHA-1, SHA-2, and SHA-3,
//! yet is at least as secure as the latest standard SHA-3.
//!
//! This implementation supports:
//! - Variable output length (1 to 64 bytes)
//! - Keyed hashing (MAC) up to 64 bytes key
//!
//! # References
//! - [RFC 7693](https://tools.ietf.org/html/rfc7693)

#![allow(clippy::needless_range_loop)]

use std::convert::TryInto;

/// BLAKE2b context state
#[derive(Clone)]
pub struct Blake2b {
    h: [u64; 8],
    t: [u64; 2],
    f: [u64; 2],
    buf: [u8; 128],
    buf_len: usize,
    out_len: usize,
}

const IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], // Duplicate of round 0 for 12 rounds
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3], // Duplicate of round 1
];

impl Blake2b {
    /// Create a new BLAKE2b context with specified output length (1-64 bytes)
    pub fn new(out_len: usize) -> Self {
        Self::new_keyed(out_len, &[])
    }

    /// Create a new keyed BLAKE2b context
    pub fn new_keyed(out_len: usize, key: &[u8]) -> Self {
        assert!(out_len > 0 && out_len <= 64);
        assert!(key.len() <= 64);

        let mut h = IV;
        h[0] ^= 0x01010000 ^ ((key.len() as u64) << 8) ^ (out_len as u64);

        let mut state = Self {
            h,
            t: [0, 0],
            f: [0, 0],
            buf: [0; 128],
            buf_len: 0,
            out_len,
        };

        if !key.is_empty() {
            state.update(key);
            state.buf_len = 128; // Pad with zeros to block size
        }

        state
    }

    /// Update hash state with input data
    pub fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        let mut len = data.len();

        while len > 0 {
            if self.buf_len == 128 {
                self.t[0] = self.t[0].wrapping_add(128);
                if self.t[0] < 128 {
                    self.t[1] = self.t[1].wrapping_add(1);
                }
                self.compress();
                self.buf_len = 0;
            }

            let space = 128 - self.buf_len;
            let chunk = if len < space { len } else { space };

            self.buf[self.buf_len..self.buf_len + chunk]
                .copy_from_slice(&data[offset..offset + chunk]);

            self.buf_len += chunk;
            offset += chunk;
            len -= chunk;
        }
    }

    /// Finalize hash and return output
    pub fn finalize(mut self) -> Vec<u8> {
        self.t[0] = self.t[0].wrapping_add(self.buf_len as u64);
        if self.t[0] < self.buf_len as u64 {
            self.t[1] = self.t[1].wrapping_add(1);
        }

        self.f[0] = !0; // Final block flag

        // Pad with zeros
        for i in self.buf_len..128 {
            self.buf[i] = 0;
        }

        self.compress();

        let mut out = Vec::with_capacity(self.out_len);
        for i in 0..8 {
            let bytes = self.h[i].to_le_bytes();
            out.extend_from_slice(&bytes);
        }

        out.truncate(self.out_len);
        out
    }

    fn compress(&mut self) {
        let mut v = [0u64; 16];
        let mut m = [0u64; 16];

        // Load message into u64 words
        for i in 0..16 {
            m[i] = u64::from_le_bytes(self.buf[i * 8..(i + 1) * 8].try_into().unwrap());
        }

        // Initialize state vector v
        v[0..8].copy_from_slice(&self.h);
        v[8..16].copy_from_slice(&IV);

        v[12] ^= self.t[0];
        v[13] ^= self.t[1];
        v[14] ^= self.f[0];
        v[15] ^= self.f[1];

        // Mixing rounds
        for i in 0..12 {
            // Column step
            mix(&mut v, 0, 4, 8, 12, m[SIGMA[i][0]], m[SIGMA[i][1]]);
            mix(&mut v, 1, 5, 9, 13, m[SIGMA[i][2]], m[SIGMA[i][3]]);
            mix(&mut v, 2, 6, 10, 14, m[SIGMA[i][4]], m[SIGMA[i][5]]);
            mix(&mut v, 3, 7, 11, 15, m[SIGMA[i][6]], m[SIGMA[i][7]]);

            // Diagonal step
            mix(&mut v, 0, 5, 10, 15, m[SIGMA[i][8]], m[SIGMA[i][9]]);
            mix(&mut v, 1, 6, 11, 12, m[SIGMA[i][10]], m[SIGMA[i][11]]);
            mix(&mut v, 2, 7, 8, 13, m[SIGMA[i][12]], m[SIGMA[i][13]]);
            mix(&mut v, 3, 4, 9, 14, m[SIGMA[i][14]], m[SIGMA[i][15]]);
        }

        // Finalize
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }
}

#[inline(always)]
fn mix(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);

    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blake2b_empty() {
        let mut hasher = Blake2b::new(64);
        hasher.update(b"");
        let hash = hasher.finalize();

        let expected = hex::decode("786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce").unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_blake2b_hello() {
        let mut hasher = Blake2b::new(64);
        hasher.update(b"Hello, world!");
        let hash = hasher.finalize();

        // Verified with Python hashlib.blake2b(b"Hello, world!").digest()
        let expected = hex::decode("a2764d133a16816b5847a737a786f2ece4c148095c5faa73e24b4cc5d666c3e45ec271504e14dc6127ddfce4e144fb23b91a6f7b04b53d695502290722953b0f").unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_blake2b_keyed() {
        let key = b"secret key";
        let mut hasher = Blake2b::new_keyed(64, key);
        hasher.update(b"Hello, world!");
        let hash = hasher.finalize();

        // Verify with another implementation or just stability
        // For now just ensure it's different from unkeyed
        let mut unkeyed = Blake2b::new(64);
        unkeyed.update(b"Hello, world!");
        assert_ne!(hash, unkeyed.finalize());
    }
}
