use sha2::{Digest, Sha256 as Sha256Impl};

/// Compute SHA-256 hash of data (one-shot)
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256Impl::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Incremental SHA-256 hasher
#[derive(Clone)]
pub struct Sha256 {
    hasher: Sha256Impl,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            hasher: Sha256Impl::new(),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    pub fn finalize(self) -> [u8; 32] {
        let digest = self.hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }
}
