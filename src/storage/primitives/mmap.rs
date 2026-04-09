// Memory-mapped file I/O for maximum performance
// ZERO external dependencies - uses raw syscalls on Linux
// For portability, falls back to regular file I/O on non-Linux

use std::fs::File;
use std::io;
use std::ptr;

#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

// Linux syscall numbers and constants
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_MMAP: i64 = 9;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_MUNMAP: i64 = 11;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_MSYNC: i64 = 26;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_MADVISE: i64 = 28;
#[cfg(target_os = "linux")]
const PROT_READ: i32 = 1;
#[cfg(target_os = "linux")]
const PROT_WRITE: i32 = 2;
#[cfg(target_os = "linux")]
const MAP_SHARED: i32 = 1;
#[cfg(target_os = "linux")]
const MAP_FAILED: isize = -1;
#[cfg(target_os = "linux")]
const MS_SYNC: i32 = 4;
#[cfg(target_os = "linux")]
const MS_ASYNC: i32 = 1;
#[cfg(target_os = "linux")]
const MADV_NORMAL: i32 = 0;
#[cfg(target_os = "linux")]
const MADV_RANDOM: i32 = 1;
#[cfg(target_os = "linux")]
const MADV_SEQUENTIAL: i32 = 2;
#[cfg(target_os = "linux")]
const MADV_WILLNEED: i32 = 3;
#[cfg(target_os = "linux")]
const MADV_DONTNEED: i32 = 4;

// Macro for raw syscalls (ZERO dependencies!)
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
macro_rules! syscall {
    ($num:expr, $arg1:expr, $arg2:expr) => {{
        let mut ret: i64;
        unsafe {
            std::arch::asm!(
                "syscall",
                inlateout("rax") $num as i64 => ret,
                in("rdi") $arg1 as i64,
                in("rsi") $arg2 as i64,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }};
    ($num:expr, $arg1:expr, $arg2:expr, $arg3:expr) => {{
        let mut ret: i64;
        unsafe {
            std::arch::asm!(
                "syscall",
                inlateout("rax") $num as i64 => ret,
                in("rdi") $arg1 as i64,
                in("rsi") $arg2 as i64,
                in("rdx") $arg3 as i64,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }};
    ($num:expr, $arg1:expr, $arg2:expr, $arg3:expr, $arg4:expr, $arg5:expr, $arg6:expr) => {{
        let mut ret: i64;
        unsafe {
            std::arch::asm!(
                "syscall",
                inlateout("rax") $num as i64 => ret,
                in("rdi") $arg1 as i64,
                in("rsi") $arg2 as i64,
                in("rdx") $arg3 as i64,
                in("r10") $arg4 as i64,
                in("r8") $arg5 as i64,
                in("r9") $arg6 as i64,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }};
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn linux_mmap(
    addr: *mut u8,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> isize {
    syscall!(SYS_MMAP, addr, len, prot, flags, fd, offset) as isize
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
unsafe fn linux_mmap(
    _addr: *mut u8,
    _len: usize,
    _prot: i32,
    _flags: i32,
    _fd: i32,
    _offset: i64,
) -> isize {
    -1 // mmap not supported on non-x86_64 without libc
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn linux_msync(addr: *mut u8, len: usize, flags: i32) -> i64 {
    syscall!(SYS_MSYNC, addr, len, flags)
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
unsafe fn linux_msync(_addr: *mut u8, _len: usize, _flags: i32) -> i64 {
    -1
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn linux_madvise(addr: *mut u8, len: usize, advice: i32) -> i64 {
    syscall!(SYS_MADVISE, addr, len, advice)
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
unsafe fn linux_madvise(_addr: *mut u8, _len: usize, _advice: i32) -> i64 {
    -1
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn linux_munmap(addr: *mut u8, len: usize) -> i64 {
    syscall!(SYS_MUNMAP, addr, len)
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
unsafe fn linux_munmap(_addr: *mut u8, _len: usize) -> i64 {
    -1
}

/// Memory access advice for madvise
#[derive(Debug, Clone, Copy)]
pub enum MadviseAdvice {
    Normal,     // No special advice
    Random,     // Random access pattern
    Sequential, // Sequential access pattern
    WillNeed,   // Will need this data soon (prefetch)
    DontNeed,   // Don't need this data (can drop from cache)
}

#[cfg(target_os = "linux")]
pub struct MmapFile {
    ptr: *mut u8,
    len: usize,
    writable: bool,
    _file: File,
}

#[cfg(target_os = "linux")]
impl MmapFile {
    /// Memory-map a file for reading using raw syscalls
    pub fn new(file: File, len: usize) -> io::Result<Self> {
        let fd = file.as_raw_fd();

        // Direct mmap syscall (ZERO dependencies!)
        let ptr = unsafe { linux_mmap(ptr::null_mut::<u8>(), len, PROT_READ, MAP_SHARED, fd, 0) };

        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            ptr: ptr as *mut u8,
            len,
            writable: false,
            _file: file,
        })
    }

    /// Memory-map a file for read-write using raw syscalls
    pub fn new_mut(file: File, len: usize) -> io::Result<Self> {
        let fd = file.as_raw_fd();

        let ptr = unsafe {
            linux_mmap(
                ptr::null_mut::<u8>(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };

        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            ptr: ptr as *mut u8,
            len,
            writable: true,
            _file: file,
        })
    }

    /// Get slice view of mapped memory
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Get mutable slice view (only if writable)
    pub fn as_mut_slice(&mut self) -> io::Result<&mut [u8]> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mmap is read-only",
            ));
        }
        Ok(unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) })
    }

    /// Read u32 at offset
    pub fn read_u32(&self, offset: usize) -> io::Result<u32> {
        if offset + 4 > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Offset out of bounds",
            ));
        }

        let bytes = unsafe { std::slice::from_raw_parts(self.ptr.add(offset), 4) };

        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Read bytes at offset
    pub fn read_bytes(&self, offset: usize, len: usize) -> io::Result<&[u8]> {
        if offset + len > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Offset out of bounds",
            ));
        }

        Ok(unsafe { std::slice::from_raw_parts(self.ptr.add(offset), len) })
    }

    /// Write bytes at offset
    pub fn write_bytes(&mut self, offset: usize, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mmap is read-only",
            ));
        }

        if offset + data.len() > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Write out of bounds",
            ));
        }

        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(offset), data.len());
        }

        Ok(())
    }

    /// Read struct at offset (zero-copy)
    pub fn read_struct<T: Copy>(&self, offset: usize) -> io::Result<&T> {
        let size = std::mem::size_of::<T>();
        if offset + size > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Struct read out of bounds",
            ));
        }

        unsafe { Ok(&*(self.ptr.add(offset) as *const T)) }
    }

    /// Read mutable struct at offset (zero-copy)
    pub fn read_struct_mut<T: Copy>(&mut self, offset: usize) -> io::Result<&mut T> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mmap is read-only",
            ));
        }

        let size = std::mem::size_of::<T>();
        if offset + size > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Struct read out of bounds",
            ));
        }

        unsafe { Ok(&mut *(self.ptr.add(offset) as *mut T)) }
    }

    /// Flush changes to disk (sync)
    pub fn flush(&self) -> io::Result<()> {
        if !self.writable {
            return Ok(()); // No-op for read-only
        }

        let result = unsafe { linux_msync(self.ptr, self.len, MS_SYNC) };

        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Flush changes to disk (async)
    pub fn flush_async(&self) -> io::Result<()> {
        if !self.writable {
            return Ok(()); // No-op for read-only
        }

        let result = unsafe { linux_msync(self.ptr, self.len, MS_ASYNC) };

        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Advise kernel about access pattern
    pub fn advise(&self, advice: MadviseAdvice) -> io::Result<()> {
        let advice_flag = match advice {
            MadviseAdvice::Normal => MADV_NORMAL,
            MadviseAdvice::Random => MADV_RANDOM,
            MadviseAdvice::Sequential => MADV_SEQUENTIAL,
            MadviseAdvice::WillNeed => MADV_WILLNEED,
            MadviseAdvice::DontNeed => MADV_DONTNEED,
        };

        let result = unsafe { linux_madvise(self.ptr, self.len, advice_flag) };

        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Get size of mapped region
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(target_os = "linux")]
impl Drop for MmapFile {
    fn drop(&mut self) {
        let _ = unsafe { linux_munmap(self.ptr, self.len) };
    }
}

// Simpler fallback for non-Linux systems - just disable mmap
#[cfg(not(target_os = "linux"))]
pub struct MmapFile {
    _placeholder: (),
}

#[cfg(not(target_os = "linux"))]
impl MmapFile {
    pub fn new(_file: File, _len: usize) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "mmap only supported on Linux (use regular file I/O)",
        ))
    }

    pub fn as_slice(&self) -> &[u8] {
        &[]
    }

    pub fn read_u32(&self, _offset: usize) -> io::Result<u32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Not implemented",
        ))
    }

    pub fn read_bytes(&self, _offset: usize, _len: usize) -> io::Result<&[u8]> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Not implemented",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    #[test]
    #[cfg(target_os = "linux")]
    fn test_mmap_basic() {
        let path = "/tmp/mmap_test.dat";

        // Create test file
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .unwrap();

        file.write_all(b"Hello, mmap!").unwrap();
        drop(file);

        // Open for mmap
        let file = OpenOptions::new().read(true).open(path).unwrap();
        let len = file.metadata().unwrap().len() as usize;

        let mmap = MmapFile::new(file, len).unwrap();
        let data = mmap.as_slice();

        assert_eq!(data, b"Hello, mmap!");

        std::fs::remove_file(path).unwrap();
    }
}
