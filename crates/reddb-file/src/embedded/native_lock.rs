//! A file-identity writer fence, disjoint from per-operation publication locks.
//!
//! The reserved byte is beyond the supported artifact address space. Locking
//! it does not allocate space or change the file. Closing the owning handle,
//! including on process death, releases the fence.
//!
//! On Android every lock call goes through [`skip_unsupported`], see there.

use std::fs::File;
use std::io;

const WRITER_BYTE: u64 = 1 << 62;

/// Whether a lock call that fails because the filesystem has no advisory
/// locks counts as taken. True only on Android, see [`skip_unsupported`].
const SKIP_UNSUPPORTED_LOCKS: bool = cfg!(target_os = "android");

/// Android shared storage (`/storage/emulated/0`) is a FUSE mount without
/// advisory locks: `flock` and OFD `fcntl` locks fail with `ENOSYS`, so every
/// open of a store there failed with "Function not implemented (os error 38)".
/// On Android a lock call that fails because the filesystem has no locks
/// counts as taken. The caller then has to make sure the file has a single
/// writer. A lock that exists but is held by someone else still fails as
/// before, on every platform, and so does any other lock error.
pub(super) fn skip_unsupported(result: io::Result<()>) -> io::Result<()> {
    skip_unsupported_if(SKIP_UNSUPPORTED_LOCKS, result)
}

fn skip_unsupported_if(skip: bool, result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if skip && lock_unsupported(&error) => Ok(()),
        other => other,
    }
}

/// The filesystem does not implement the lock call at all.
fn lock_unsupported(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EOPNOTSUPP))
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

pub(super) fn try_lock_writer(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, WRITER_BYTE, 1, true, true)
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    {
        skip_unsupported(ofd_lock(file, WRITER_BYTE, 1, libc::F_WRLCK as _, false))
    }
    #[cfg(not(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "embedded writer identity locks are unavailable on this platform",
        ))
    }
}

pub(super) fn lock_shared(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, 0, WRITER_BYTE, false, false)
    }
    // Darwin flock and OFD locks share a conflict domain. Keep publication
    // disjoint from the lifetime byte, just as on Windows.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        ofd_lock(file, 0, WRITER_BYTE, libc::F_RDLCK as _, true)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    {
        skip_unsupported(fs2::FileExt::lock_shared(file))
    }
}

pub(super) fn try_lock_shared(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, 0, WRITER_BYTE, false, true)
    }
    // Darwin flock and OFD locks share a conflict domain. Keep publication
    // disjoint from the lifetime byte, just as on Windows.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        ofd_lock(file, 0, WRITER_BYTE, libc::F_RDLCK as _, false)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    {
        skip_unsupported(fs2::FileExt::try_lock_shared(file))
    }
}

pub(super) fn lock_exclusive(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, 0, WRITER_BYTE, true, false)
    }
    // Darwin flock and OFD locks share a conflict domain. Keep publication
    // disjoint from the lifetime byte, just as on Windows.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        ofd_lock(file, 0, WRITER_BYTE, libc::F_WRLCK as _, true)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    {
        skip_unsupported(fs2::FileExt::lock_exclusive(file))
    }
}

pub(super) fn unlock(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: the synchronous file handle is live and the range exactly
        // matches the publication lock acquired by this module.
        let result = unsafe {
            UnlockFileEx(
                file.as_raw_handle(),
                0,
                WRITER_BYTE as u32,
                (WRITER_BYTE >> 32) as u32,
                &mut overlapped,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    // Darwin flock and OFD locks share a conflict domain. Keep publication
    // disjoint from the lifetime byte, just as on Windows.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        ofd_lock(file, 0, WRITER_BYTE, libc::F_UNLCK as _, false)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    {
        skip_unsupported(fs2::FileExt::unlock(file))
    }
}

#[cfg(windows)]
fn windows_lock(
    file: &File,
    offset: u64,
    length: u64,
    exclusive: bool,
    nonblocking: bool,
) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let mut overlapped = OVERLAPPED::default();
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    let flags = if exclusive {
        LOCKFILE_EXCLUSIVE_LOCK
    } else {
        0
    } | if nonblocking {
        LOCKFILE_FAIL_IMMEDIATELY
    } else {
        0
    };
    // SAFETY: std files use synchronous handles. OVERLAPPED only supplies the
    // offset; it remains live until this call completes (or fails immediately).
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            length as u32,
            (length >> 32) as u32,
            &mut overlapped,
        )
    };
    if result == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn ofd_lock(
    file: &File,
    offset: u64,
    length: u64,
    kind: libc::c_short,
    wait: bool,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // OFD ownership survives closing unrelated read/append handles. Never
    // fall back to process-owned F_SETLK, whose locks disappear on any close.
    // SAFETY: zero initializes every field and padding byte of flock.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as _;
    lock.l_start = offset as _;
    lock.l_len = length as _;
    let command = if wait {
        libc::F_OFD_SETLKW
    } else {
        libc::F_OFD_SETLK
    };
    loop {
        // SAFETY: file and the flock pointer stay live for this synchronous call.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), command, &lock) };
        if result != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        return Err(error);
    }
}

#[cfg(all(test, unix))]
mod skip_unsupported_tests {
    use super::*;

    fn os_error(code: i32) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(code))
    }

    #[test]
    fn missing_lock_support_counts_as_taken_when_enabled() {
        assert!(skip_unsupported_if(true, os_error(libc::ENOSYS)).is_ok());
        assert!(skip_unsupported_if(true, os_error(libc::EOPNOTSUPP)).is_ok());
    }

    #[test]
    fn contention_and_other_errors_still_fail_when_enabled() {
        for code in [libc::EWOULDBLOCK, libc::EACCES, libc::EIO, libc::ENOLCK] {
            let error = skip_unsupported_if(true, os_error(code)).expect_err("must fail");
            assert_eq!(error.raw_os_error(), Some(code));
        }
        let blocked = skip_unsupported_if(true, Err(io::ErrorKind::WouldBlock.into()));
        assert_eq!(
            blocked.expect_err("must fail").kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn missing_lock_support_fails_when_disabled() {
        let error = skip_unsupported_if(false, os_error(libc::ENOSYS)).expect_err("must fail");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSYS));
    }

    #[test]
    fn only_android_skips() {
        assert_eq!(SKIP_UNSUPPORTED_LOCKS, cfg!(target_os = "android"));
    }
}
