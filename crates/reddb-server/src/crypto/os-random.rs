//! OS-backed CSPRNG helpers.

#[cfg(unix)]
use std::io::Read;

/// Fill the buffer using the OS CSPRNG.
pub fn fill_bytes(buf: &mut [u8]) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let mut file = std::fs::File::open("/dev/urandom")
            .map_err(|e| format!("CSPRNG open failed: {}", e))?;
        file.read_exact(buf)
            .map_err(|e| format!("CSPRNG read failed: {}", e))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        fill_bytes_windows(buf)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = buf;
        return Err("OS CSPRNG not supported on this platform".to_string());
    }
}

#[cfg(windows)]
fn fill_bytes_windows(buf: &mut [u8]) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }

    let length = u32::try_from(buf.len())
        .map_err(|_| "OS CSPRNG buffer exceeds the Windows length limit".to_string())?;
    let ok = unsafe { SystemFunction036(buf.as_mut_ptr(), length) };
    if ok == 0 {
        return Err("OS CSPRNG failed".to_string());
    }

    Ok(())
}

#[cfg(windows)]
#[allow(non_snake_case)]
#[link(name = "advapi32")]
extern "system" {
    fn SystemFunction036(RandomBuffer: *mut u8, RandomBufferLength: u32) -> u8;
}
