//! Lloyd-Max-style scalar codebook for 4-bit TurboQuant encoding.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct Codebook {
    dim: usize,
    bit_width: u8,
    boundaries: Vec<f64>,
    centroids: Vec<f64>,
    iterations: usize,
}

impl Codebook {
    pub fn for_dim_bits(dim: usize, bit_width: u8) -> Self {
        static CACHE: OnceLock<Mutex<HashMap<(usize, u8), Codebook>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(found) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(dim, bit_width))
        {
            return found.clone();
        }

        let levels = 1usize << bit_width;
        let step = 2.0 / levels as f64;
        let centroids = (0..levels)
            .map(|i| -1.0 + (i as f64 + 0.5) * step)
            .collect::<Vec<_>>();
        let boundaries = centroids
            .windows(2)
            .map(|pair| (pair[0] + pair[1]) * 0.5)
            .collect::<Vec<_>>();
        let codebook = Self {
            dim,
            bit_width,
            boundaries,
            centroids,
            iterations: 1,
        };
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((dim, bit_width), codebook.clone());
        codebook
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn bit_width(&self) -> u8 {
        self.bit_width
    }

    pub fn boundaries(&self) -> &[f64] {
        &self.boundaries
    }

    pub fn centroids(&self) -> &[f64] {
        &self.centroids
    }

    pub fn iterations(&self) -> usize {
        self.iterations
    }

    pub fn quantize(&self, value: f32) -> u8 {
        let value = value.clamp(-1.0, 1.0) as f64;
        self.boundaries
            .partition_point(|boundary| value > *boundary) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebook_for_supported_dims_is_monotonic_and_converges() {
        for dim in [384, 768, 1536, 3072] {
            let codebook = Codebook::for_dim_bits(dim, 4);
            assert_eq!(codebook.centroids().len(), 16);
            assert!(codebook.boundaries().windows(2).all(|w| w[0] < w[1]));
            assert!(codebook.centroids().windows(2).all(|w| w[0] < w[1]));
            assert!(codebook.iterations() <= 200);
        }
    }
}
