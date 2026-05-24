//! Deterministic orthogonal rotation for TurboQuant.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

#[derive(Debug, Clone, PartialEq)]
pub struct RotationMatrix {
    dim: usize,
    seed: u64,
    permutation: Vec<usize>,
    signs: Vec<f32>,
}

impl RotationMatrix {
    pub fn new(dim: usize, seed: u64) -> Self {
        let mut permutation: Vec<usize> = (0..dim).collect();
        let mut state = seed ^ ((dim as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for i in (1..dim).rev() {
            let j = (splitmix64(&mut state) as usize) % (i + 1);
            permutation.swap(i, j);
        }
        let signs = (0..dim)
            .map(|_| {
                if splitmix64(&mut state) & 1 == 0 {
                    1.0
                } else {
                    -1.0
                }
            })
            .collect();
        Self {
            dim,
            seed,
            permutation,
            signs,
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn rotate(&self, input: &[f32]) -> Vec<f32> {
        assert_eq!(input.len(), self.dim, "rotation dimension mismatch");
        self.permutation
            .iter()
            .zip(&self.signs)
            .map(|(&source, &sign)| input[source] * sign)
            .collect()
    }

    pub fn row_entries(&self, row: usize) -> Option<(usize, f32)> {
        (row < self.dim).then(|| (self.permutation[row], self.signs[row]))
    }
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_is_bit_identical_for_same_dim_and_seed() {
        let a = RotationMatrix::new(1536, 42);
        let b = RotationMatrix::new(1536, 42);
        assert_eq!(a, b);
        assert_eq!(a.rotate(&vec![1.0; 1536]), b.rotate(&vec![1.0; 1536]));
    }
}
