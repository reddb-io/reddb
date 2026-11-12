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
        ofd_lock(file, WRITER_BYTE, 1, libc::F_WRLCK as _, false)
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
        fs2::FileExt::lock_shared(file)
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
        fs2::FileExt::try_lock_shared(file)
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
    // Darwin flock and OFD locks share a conflict domain. Keep publication
    // disjoint from the lifetime byte, just as on Windows.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        ofd_lock(file, 0, WRITER_BYTE, libc::F_UNLCK as _, false)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
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
