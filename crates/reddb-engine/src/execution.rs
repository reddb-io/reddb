//! Host execution capabilities used by canonical collection scans.

/// Native hosts retain the existing core-count threshold. Browser Workers run
/// scans serially; Rust thread spawning is unavailable on wasm32-unknown-unknown.
pub fn should_parallelize() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::available_parallelism()
            .map(|cores| cores.get() > 1)
            .unwrap_or(false)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}
