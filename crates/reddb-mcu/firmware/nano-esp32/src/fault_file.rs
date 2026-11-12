use esp_idf_sys as sys;
use reddb_mcu::{fixed_file::FixedCapacityFile, VfsFile};
use std::{
    ffi::CStr,
    io::{self, SeekFrom},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

const NVS_NAMESPACE: &CStr = c"reddb";
const NVS_FAULT_KEY: &CStr = c"fault_v1";

struct FaultState {
    enabled: bool,
    writes_until_reset: AtomicUsize,
    nvs: sys::nvs_handle_t,
}

impl Drop for FaultState {
    fn drop(&mut self) {
        unsafe { sys::nvs_close(self.nvs) };
    }
}

#[derive(Clone)]
pub struct FaultControl {
    state: Arc<FaultState>,
}

impl FaultControl {
    pub fn open() -> io::Result<Self> {
        esp(unsafe { sys::nvs_flash_init() })?;
        let mut handle = 0;
        esp(unsafe {
            sys::nvs_open(
                NVS_NAMESPACE.as_ptr(),
                sys::nvs_open_mode_t_NVS_READWRITE,
                &mut handle,
            )
        })?;
        let mut fired = 0u8;
        let enabled = match unsafe { sys::nvs_get_u8(handle, NVS_FAULT_KEY.as_ptr(), &mut fired) } {
            code if code == sys::ESP_OK as sys::esp_err_t => false,
            code if code == sys::ESP_ERR_NVS_NOT_FOUND as sys::esp_err_t => true,
            code => {
                unsafe { sys::nvs_close(handle) };
                return Err(esp_error(code));
            }
        };
        Ok(Self {
            state: Arc::new(FaultState {
                enabled,
                writes_until_reset: AtomicUsize::new(0),
                nvs: handle,
            }),
        })
    }

    pub fn arm_after_writes(&self, writes: usize) -> bool {
        if !self.state.enabled || writes == 0 {
            return false;
        }
        self.state
            .writes_until_reset
            .compare_exchange(0, writes, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub struct ResetOnceFile {
    inner: FixedCapacityFile,
    fault: FaultControl,
}

impl ResetOnceFile {
    pub fn new(inner: FixedCapacityFile, fault: FaultControl) -> Self {
        Self { inner, fault }
    }

    pub fn capacity(&self) -> u64 {
        self.inner.capacity()
    }

    fn reset_if_armed(&self) -> io::Result<()> {
        let previous = self
            .fault
            .state
            .writes_until_reset
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .unwrap_or(0);
        if previous != 1 {
            return Ok(());
        }

        esp(unsafe { sys::nvs_set_u8(self.fault.state.nvs, NVS_FAULT_KEY.as_ptr(), 1) })?;
        esp(unsafe { sys::nvs_commit(self.fault.state.nvs) })?;
        println!("REDB_MCU_FAULT reset=during_write");
        unsafe {
            sys::vTaskDelay(10);
            sys::esp_restart();
        }
    }
}

fn esp(code: sys::esp_err_t) -> io::Result<()> {
    if code == sys::ESP_OK as sys::esp_err_t {
        Ok(())
    } else {
        Err(esp_error(code))
    }
}

fn esp_error(code: sys::esp_err_t) -> io::Error {
    io::Error::other(format!("ESP-IDF NVS error {code}"))
}

impl VfsFile for ResetOnceFile {
    fn max_read_buffer_bytes(&self) -> usize {
        self.inner.max_read_buffer_bytes()
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            inner: self.inner.try_clone()?,
            fault: self.fault.clone(),
        })
    }

    fn same_file(&self, other: &Self) -> io::Result<bool> {
        self.inner.same_file(&other.inner)
    }

    fn file_len(&self) -> io::Result<u64> {
        self.inner.file_len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        self.inner.reserve(offset, len)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.reset_if_armed()
    }

    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner.read(bytes)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn sync_data(&self) -> io::Result<()> {
        self.inner.sync_data()
    }
}
