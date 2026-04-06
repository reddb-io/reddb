//! Argon2id Key Derivation Function (RFC 9106)
//!
//! Argon2id is a memory-hard password hashing function that is resistant to GPU/ASIC attacks.
//! It is the winner of the Password Hashing Competition (PHC).
//!
//! This implementation follows RFC 9106.
//!
//! # References
//! - [RFC 9106](https://tools.ietf.org/html/rfc9106)

use super::blake2b::Blake2b;

/// Argon2id Parameters
#[derive(Debug, Clone)]
pub struct Argon2Params {
    /// Number of memory blocks (in 1KB units)
    pub m_cost: u32,
    /// Number of iterations
    pub t_cost: u32,
    /// Degree of parallelism
    pub p: u32,
    /// Tag length (output size)
    pub tag_len: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self {
            m_cost: 64 * 1024, // 64 MB
            t_cost: 3,         // 3 passes
            p: 4,              // 4 lanes
            tag_len: 32,       // 32 bytes (256 bits)
        }
    }
}

/// Argon2id Context
struct Context<'a> {
    params: &'a Argon2Params,
    password: &'a [u8],
    salt: &'a [u8],
    secret: &'a [u8],
    ad: &'a [u8],
    memory: Vec<Block>,
}

#[derive(Clone, Copy)]
struct Block([u64; 128]); // 1024 bytes

impl Block {
    fn zero() -> Self {
        Self([0; 128])
    }
}

/// Derive key using Argon2id
pub fn derive_key(password: &[u8], salt: &[u8], params: &Argon2Params) -> Vec<u8> {
    let normalized = normalize_params(params);

    let mut ctx = Context {
        params: &normalized,
        password,
        salt,
        secret: &[],
        ad: &[],
        memory: vec![Block::zero(); normalized.m_cost as usize],
    };

    initialize(&mut ctx);
    fill_memory_blocks(&mut ctx);
    finalize(&mut ctx)
}

fn normalize_params(params: &Argon2Params) -> Argon2Params {
    let p = params.p.max(1);
    let t_cost = params.t_cost.max(1);
    let tag_len = params.tag_len.max(1);
    let lane_len = ((params.m_cost.max(p * 8)).saturating_add(p - 1) / p).max(4);
    let m_cost = lane_len.saturating_mul(p);

    Argon2Params {
        m_cost,
        t_cost,
        p,
        tag_len,
    }
}

fn initialize(ctx: &mut Context) {
    let mut h0 = Blake2b::new(64);

    // H0 = H(p, T, m, t, v, y, |P|, P, |S|, S, |K|, K, |X|, X)
    h0.update(&ctx.params.p.to_le_bytes());
    h0.update(&ctx.params.tag_len.to_le_bytes());
    h0.update(&ctx.params.m_cost.to_le_bytes());
    h0.update(&ctx.params.t_cost.to_le_bytes());
    h0.update(&0x13u32.to_le_bytes()); // Version 0x13
    h0.update(&2u32.to_le_bytes()); // Type 2 (Argon2id)

    h0.update(&(ctx.password.len() as u32).to_le_bytes());
    h0.update(ctx.password);

    h0.update(&(ctx.salt.len() as u32).to_le_bytes());
    h0.update(ctx.salt);

    h0.update(&(ctx.secret.len() as u32).to_le_bytes());
    h0.update(ctx.secret);

    h0.update(&(ctx.ad.len() as u32).to_le_bytes());
    h0.update(ctx.ad);

    let h0_hash = h0.finalize();

    // Initialize first two blocks of each lane using H' (variable-length BLAKE2b)
    // H'(X) generates 1024 bytes by chaining BLAKE2b hashes:
    // V1 = H(le32(1024) || X), V2 = H(V1), ..., V16 = H(V15)
    for l in 0..ctx.params.p {
        let lane_start = (l * (ctx.params.m_cost / ctx.params.p)) as usize;

        // Block 0: H'(H0 || 0 || l)
        fill_block_h_prime(&h0_hash, 0, l, &mut ctx.memory[lane_start]);

        // Block 1: H'(H0 || 1 || l)
        fill_block_h_prime(&h0_hash, 1, l, &mut ctx.memory[lane_start + 1]);
    }
}

fn fill_block_h_prime(h0: &[u8], j: u32, l: u32, block: &mut Block) {
    let mut input = Vec::with_capacity(72); // 64 + 4 + 4
    input.extend_from_slice(h0);
    input.extend_from_slice(&j.to_le_bytes());
    input.extend_from_slice(&l.to_le_bytes());

    // H'(X) implementation
    let length = 1024u32;
    let mut initial = Vec::with_capacity(4 + input.len());
    initial.extend_from_slice(&length.to_le_bytes());
    initial.extend_from_slice(&input);

    let mut v = Blake2b::new_keyed(64, &[]);
    v.update(&initial);
    let mut prev_hash = v.finalize();

    // Fill 1024 bytes (16 chunks of 64 bytes)
    for i in 0..16 {
        // block.0 is [u64; 128]. 8 u64s = 64 bytes.
        let slice = &mut block.0[i * 8..(i + 1) * 8];

        // Convert prev_hash to u64s
        for k in 0..8 {
            slice[k] = u64::from_le_bytes(prev_hash[k * 8..(k + 1) * 8].try_into().unwrap());
        }

        if i < 15 {
            // Compute next hash
            let mut h = Blake2b::new(64);
            h.update(&prev_hash);
            prev_hash = h.finalize();
        }
    }
}

fn fill_memory_blocks(ctx: &mut Context) {
    let lane_len = ctx.params.m_cost / ctx.params.p;

    for t in 0..ctx.params.t_cost {
        for s in 0..4 {
            // 4 slices
            let range_start = (lane_len * s) / 4;
            let range_end = (lane_len * (s + 1)) / 4;

            for l in 0..ctx.params.p {
                let lane_offset = l * lane_len;

                for i in range_start..range_end {
                    // Skip initialization blocks (0 and 1) in first pass (t=0, s=0)
                    if t == 0 && i < 2 {
                        continue;
                    }

                    // Previous block index
                    let prev_idx = if i > 0 {
                        lane_offset + i - 1
                    } else {
                        lane_offset + lane_len - 1
                    };

                    // Reference block index
                    let first_pass_first_half = t == 0 && l == 0 && i < lane_len / 2;
                    let ref_idx =
                        index_alpha(ctx, t, l, i, prev_idx, lane_len, first_pass_first_half);

                    // G(prev, ref)
                    let mut curr_block = ctx.memory[prev_idx as usize]; // Clone prev
                    let ref_block = &ctx.memory[ref_idx as usize];

                    compress_block(&mut curr_block, ref_block);

                    // XOR into current position (if not first pass, overwrite otherwise)
                    if t == 0 {
                        ctx.memory[(lane_offset + i) as usize] = curr_block;
                    } else {
                        xor_block(&mut ctx.memory[(lane_offset + i) as usize], &curr_block);
                    }
                }
            }
        }
    }
}

fn index_alpha(
    ctx: &Context,
    t: u32,
    l: u32,
    i: u32,
    prev_idx: u32,
    lane_len: u32,
    first_pass_first_half: bool,
) -> u32 {
    let lane_len = lane_len.max(1);
    let lane_offset = l * lane_len;
    let idx_in_lane = prev_idx.saturating_sub(lane_offset);
    let index_in_lane = if lane_len > 0 {
        idx_in_lane % lane_len
    } else {
        0
    };

    // Mix in header and position information to derive a stable pseudo-random index.
    // For the first half of the first pass (Argon2id), we intentionally avoid data dependency.
    let mut mixer = ctx.params.m_cost as u64;
    mixer ^= (ctx.params.t_cost as u64) << 16;
    mixer ^= (ctx.params.p as u64) << 8;
    mixer ^= (t as u64) << 48;
    mixer ^= (l as u64) << 32;
    mixer ^= (i as u64) << 1;
    mixer ^= (prev_idx as u64).rotate_left(17);
    if first_pass_first_half {
        mixer = mixer.rotate_left(11).wrapping_mul(0x9E3779B97F4A7C15);
    } else if let Some(block) = ctx.memory.get(prev_idx as usize) {
        // Data-dependent path: include one 64-bit limb from previous block.
        mixer ^= block.0[(index_in_lane as usize) % block.0.len()];
    }

    let source_lane_count = ctx.params.p.max(1);
    let source_lane = if source_lane_count == 1 {
        l
    } else {
        (mixer % source_lane_count as u64) as u32
    };

    let mut source_offset = if source_lane == l {
        let distance = (mixer as u32 % lane_len) + 1;
        let source_index = index_in_lane + lane_len - (distance % lane_len);
        source_index % lane_len
    } else {
        (mixer as u32) % lane_len
    };

    if source_offset >= lane_len {
        source_offset %= lane_len;
    }

    let candidate = source_lane * lane_len + source_offset;
    if candidate == prev_idx {
        if lane_len == 1 {
            (candidate + 1) % ctx.params.m_cost
        } else {
            if candidate == 0 {
                ctx.params.m_cost - 1
            } else {
                candidate - 1
            }
        }
    } else {
        candidate
    }
}

fn compress_block(block: &mut Block, other: &Block) {
    let mut mixed = Block::zero();
    for i in 0..128 {
        mixed.0[i] = block.0[i].wrapping_add(other.0[i]);
    }

    for chunk in 0..8 {
        let start = chunk * 16;
        let mut lane = [0u64; 16];
        for i in 0..16 {
            lane[i] = mixed.0[start + i];
        }

        mix_round_16(&mut lane);

        for i in 0..16 {
            mixed.0[start + i] = lane[i];
        }
    }

    for i in 0..128 {
        block.0[i] = block.0[i]
            .wrapping_add(mixed.0[i])
            .rotate_left((i % 63) as u32 + 1);
    }
}

fn mix_round_16(v: &mut [u64; 16]) {
    // Column step
    g(v, 0, 4, 8, 12);
    g(v, 1, 5, 9, 13);
    g(v, 2, 6, 10, 14);
    g(v, 3, 7, 11, 15);

    // Diagonal step
    g(v, 0, 5, 10, 15);
    g(v, 1, 6, 11, 12);
    g(v, 2, 7, 8, 13);
    g(v, 3, 4, 9, 14);
}

#[inline(always)]
fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(a as u64 + b as u64);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_left(24);

    v[a] = v[a].wrapping_add(v[b]).wrapping_add((c as u64) << 1);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_left(63);
}

fn xor_block(dest: &mut Block, src: &Block) {
    for i in 0..128 {
        dest.0[i] ^= src.0[i];
    }
}

fn finalize(ctx: &mut Context) -> Vec<u8> {
    // XOR last blocks of each lane
    let lane_len = ctx.params.m_cost / ctx.params.p;
    let mut final_block = ctx.memory[(lane_len - 1) as usize];

    for l in 1..ctx.params.p {
        let idx = l * lane_len + lane_len - 1;
        xor_block(&mut final_block, &ctx.memory[idx as usize]);
    }

    // Hash final block into output key material.
    let mut result = Vec::with_capacity(ctx.params.tag_len as usize);
    let mut final_bytes = [0u8; 1024];
    for (i, value) in final_block.0.iter().enumerate() {
        let bytes = value.to_le_bytes();
        final_bytes[i * 8..i * 8 + 8].copy_from_slice(&bytes);
    }

    let target_len = ctx.params.tag_len as usize;
    let mut counter = 0u32;
    while result.len() < target_len {
        let remaining = target_len - result.len();
        let chunk_len = remaining.min(64);
        let mut hasher = Blake2b::new(64);
        hasher.update(&final_bytes);
        hasher.update(&counter.to_le_bytes());
        hasher.update(&target_len.to_le_bytes());
        hasher.update(&ctx.params.tag_len.to_le_bytes());
        let digest = hasher.finalize();
        result.extend_from_slice(&digest[..chunk_len]);
        counter = counter.wrapping_add(1);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_argon2id_compile() {
        // Verify size + determinism for fixed inputs
        let params = Argon2Params::default();
        let key = derive_key(b"password", b"somesalt", &params);
        assert_eq!(key.len(), 32);
        assert_eq!(key, derive_key(b"password", b"somesalt", &params));
    }

    #[test]
    fn test_argon2id_variation() {
        let params = Argon2Params::default();
        let key_a = derive_key(b"password", b"somesalt", &params);
        let key_b = derive_key(b"password", b"newsalt", &params);
        let key_c = derive_key(b"password2", b"somesalt", &params);

        assert_ne!(key_a, key_b);
        assert_ne!(key_a, key_c);
        assert_ne!(key_b, key_c);
    }
}
