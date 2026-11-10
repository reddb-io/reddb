//! A file-identity writer fence, disjoint from per-operation publication locks.
//!
//! The reserved byte is beyond the supported artifact address space. Locking
//! it does not allocate space or change the file. Closing the owning handle,
//! including on process death, releases the fence.

use std::fs::File;
use std::io;

const WRITER_BYTE: u64 = 1 << 62;

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
        use std::os::fd::AsRawFd;
        // OFD locks belong to this open description, not to the process. A
        // temporary append/read handle closing must never release this lock.
        // SAFETY: zero initializes all flock padding; the descriptor and
        // pointer remain valid for the synchronous fcntl call.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = WRITER_BYTE as _;
        lock.l_len = 1;
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            return Err(error);
        }
        Ok(())
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
    #[cfg(not(windows))]
    {
        fs2::FileExt::lock_shared(file)
    }
}

pub(super) fn try_lock_shared(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, 0, WRITER_BYTE, false, true)
    }
    #[cfg(not(windows))]
    {
        fs2::FileExt::try_lock_shared(file)
    }
}

pub(super) fn lock_exclusive(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_lock(file, 0, WRITER_BYTE, true, false)
    }
    #[cfg(not(windows))]
    {
        fs2::FileExt::lock_exclusive(file)
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
    #[cfg(not(windows))]
    {
        fs2::FileExt::unlock(file)
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
