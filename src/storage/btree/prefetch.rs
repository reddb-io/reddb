//! B-tree leaf prefetch — Phase 5 / PLAN.md backlog 3.6.
//!
//! Issues OS-level read-ahead hints for the next leaf block in a
//! range scan, so the kernel can DMA the page into the buffer pool
//! while the cursor is still consuming the current one.
//!
//! Mirrors PG's `BufferPrefetchPage` via `posix_fadvise(WILLNEED)`
//! on Linux and `madvise(MADV_WILLNEED)` on macOS / BSD.
//!
//! ## Why
//!
//! reddb's range scan in `btree/cursor.rs` walks leaves
//! sequentially. The buffer-pool fetch for leaf N+1 happens only
//! after the cursor finishes leaf N, so the disk read serializes
//! with the CPU's tuple processing. Prefetch breaks that
//! dependency: as soon as the cursor lands on leaf N's halfway
//! point, we tell the kernel to start fetching N+1 in the
//! background.
//!
//! ## Wiring
//!
//! Phase 5 wiring adds a single call site in
//! `btree/cursor.rs::advance_leaf` that checks "are we past 50%
//! of the current leaf?" and if so calls `prefetch_page(next_leaf_id)`.
//! The cursor already knows `next_leaf_id` from the leaf header.
//!
//! The actual `posix_fadvise` syscall is OS-specific and behind
//! a stub on platforms that don't support it (Windows). reddb
//! ships Linux-first so the Linux path is the one this module
//! actually exercises.

#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

/// Errors raised by the prefetch path. Most are silent — a
/// failed prefetch is a perf miss, not a correctness bug, so
/// callers should log and continue.
#[derive(Debug)]
pub enum PrefetchError {
    /// posix_fadvise / madvise returned non-zero. Wrapped so
    /// the caller can decide whether to log or escalate.
    SyscallFailed(std::io::Error),
    /// Platform doesn't support read-ahead hints; the call
    /// becomes a no-op but we surface the unsupported state
    /// for diagnostics.
    Unsupported,
}

impl std::fmt::Display for PrefetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SyscallFailed(e) => write!(f, "prefetch syscall failed: {e}"),
            Self::Unsupported => write!(f, "prefetch unsupported on this platform"),
        }
    }
}

impl std::error::Error for PrefetchError {}

/// Tell the OS to start fetching the byte range
/// `[offset, offset + length)` of `file` into the page cache.
/// Returns `Ok(())` when the syscall succeeds (no guarantee
/// the data actually arrives — that's up to the kernel).
///
/// **Linux**: invokes `posix_fadvise(fd, off, len, POSIX_FADV_WILLNEED)`.
/// **macOS / BSD**: stub returning `Unsupported`. A future commit
/// adds `fcntl(F_RDADVISE)` for Darwin.
/// **Windows**: stub returning `Unsupported`.
pub fn prefetch_range(file: &std::fs::File, offset: u64, length: u64) -> Result<(), PrefetchError> {
    #[cfg(target_os = "linux")]
    {
        // POSIX_FADV_WILLNEED == 3 on Linux. Hardcoded so we
        // don't pull libc into the dep graph.
        const POSIX_FADV_WILLNEED: i32 = 3;
        let fd = file.as_raw_fd();
        // SAFETY: fd is a valid open file descriptor for the
        // lifetime of `file`. The syscall takes raw integers.
        let ret = unsafe {
            libc_like::posix_fadvise(fd, offset as i64, length as i64, POSIX_FADV_WILLNEED)
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(PrefetchError::SyscallFailed(
                std::io::Error::from_raw_os_error(ret),
            ))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, offset, length);
        Err(PrefetchError::Unsupported)
    }
}

/// Prefetch a single page identified by `(file, page_id, page_size)`.
/// Convenience wrapper for `prefetch_range` that does the
/// `offset = page_id * page_size` math.
pub fn prefetch_page(
    file: &std::fs::File,
    page_id: u64,
    page_size: u32,
) -> Result<(), PrefetchError> {
    prefetch_range(file, page_id * page_size as u64, page_size as u64)
}

/// Tiny libc shim — only the one function we need, declared
/// extern so we don't pull the full `libc` crate into the dep
/// graph. Linux ABI is stable for this call.
#[cfg(target_os = "linux")]
mod libc_like {
    extern "C" {
        pub fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    }
}
