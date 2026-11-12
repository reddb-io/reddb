mod fault_file;

use esp_idf_sys as sys;
use fault_file::{FaultControl, ResetOnceFile};
use reddb_mcu::{
    file::database_header_magic_matches,
    fixed_file::{FixedCapacityFile, FixedFileState},
    types::Value,
    Database, NANO_ESP32,
};
use std::{
    error::Error,
    ffi::CStr,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

const MOUNT_PATH: &CStr = c"/reddb";
const PARTITION_LABEL: &CStr = c"storage";
const DATABASE_PATH: &str = "/reddb/data.rdb";
const MEASUREMENTS: u64 = 43_200;
const INSERT_BATCH: u64 = 64;
const ALLOCATION_UNIT: u64 = 4096;
const FAULT_ARM_ROW: u64 = 16_384;
const FAULT_AFTER_WRITES: usize = 3;
const MIN_INTERNAL_HEADROOM_BYTES: usize = 32 * 1024;
const MIN_SPIRAM_HEADROOM_BYTES: usize = 512 * 1024;

fn main() {
    sys::link_patches();
    if let Err(error) = run() {
        panic!("RedDB MCU firmware failed: {error}");
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    report_heap("startup")?;
    let _mount = mount_fatfs()?;
    let (file, state) = provision_database_file()?;
    let fault = FaultControl::open()?;
    let file = ResetOnceFile::new(file, fault.clone());
    let mut database = match state {
        FixedFileState::Empty => Database::create(file)?,
        FixedFileState::Existing => Database::open(file)?,
    };
    if state == FixedFileState::Empty {
        database.query(
            "CREATE TABLE measurements (recorded_at BIGINT NOT NULL, temperature DOUBLE NOT NULL)",
            1,
        )?;
    }

    let mut rows = count_and_verify(&database)?;
    if rows != MEASUREMENTS && !rows.is_multiple_of(INSERT_BATCH) {
        return Err(format!("recovery exposed a partial SQL batch at row {rows}").into());
    }
    println!("REDB_MCU_RECOVERY state={state:?} rows={rows}");
    report_heap("recovered")?;
    while rows < MEASUREMENTS {
        let count = INSERT_BATCH.min(MEASUREMENTS - rows);
        if rows == FAULT_ARM_ROW && fault.arm_after_writes(FAULT_AFTER_WRITES) {
            println!("REDB_MCU_FAULT_ARM rows={rows} writes_until_reset={FAULT_AFTER_WRITES}");
        }
        database.query(
            &format!(
                "INSERT INTO measurements VALUES {}",
                measurement_values(rows, count)
            ),
            2 + rows,
        )?;
        rows += count;
        if rows.is_multiple_of(4096) || rows == MEASUREMENTS {
            println!("REDB_MCU_PROGRESS rows={rows}");
            report_heap("ingest")?;
        }
    }

    let recovered = count_and_verify(&database)?;
    if recovered != MEASUREMENTS {
        return Err(format!("expected {MEASUREMENTS} rows, recovered {recovered}").into());
    }
    report_heap("verified")?;
    println!(
        "REDB_MCU_OK rows={recovered} capacity_bytes={}",
        database.into_inner().capacity()
    );
    Ok(())
}

fn count_and_verify(database: &Database<ResetOnceFile>) -> Result<u64, Box<dyn Error>> {
    let mut seen = vec![false; MEASUREMENTS as usize];
    let mut count = 0u64;
    database.query_stream(
        "SELECT recorded_at, temperature FROM measurements",
        &[],
        &mut |row| {
            let minute = match row[0] {
                Value::BigInt(value) if value >= 0 && value < MEASUREMENTS as i64 => value as usize,
                _ => {
                    return Err(reddb_mcu::engine::query::local_read::LocalReadError::Query(
                        "measurement timestamp is out of range".to_string(),
                    ));
                }
            };
            let expected_temperature = 18.0 + (minute % 100) as f64 / 10.0;
            let temperature = match row[1] {
                Value::Float(value) => value,
                _ => {
                    return Err(reddb_mcu::engine::query::local_read::LocalReadError::Query(
                        format!("measurement {minute} has a non-float temperature"),
                    ));
                }
            };
            if (temperature - expected_temperature).abs() > f64::EPSILON || seen[minute] {
                return Err(reddb_mcu::engine::query::local_read::LocalReadError::Query(
                    format!("measurement {minute} failed verification"),
                ));
            }
            seen[minute] = true;
            count += 1;
            if count.is_multiple_of(256) {
                unsafe { sys::vTaskDelay(1) };
            }
            Ok(true)
        },
    )?;
    let prefix = count as usize;
    if seen[..prefix].iter().any(|present| !present)
        || seen[prefix..].iter().any(|present| *present)
    {
        return Err("measurements do not form a contiguous prefix".into());
    }
    Ok(count)
}

fn report_heap(stage: &str) -> Result<(), Box<dyn Error>> {
    let internal = heap_stats(sys::MALLOC_CAP_INTERNAL);
    let spiram = heap_stats(sys::MALLOC_CAP_SPIRAM);
    println!(
        "REDB_MCU_HEAP stage={stage} internal_total={} internal_free={} internal_min={} internal_largest={} spiram_total={} spiram_free={} spiram_min={} spiram_largest={}",
        internal.total,
        internal.free,
        internal.minimum_free,
        internal.largest_free,
        spiram.total,
        spiram.free,
        spiram.minimum_free,
        spiram.largest_free
    );
    if internal.minimum_free < MIN_INTERNAL_HEADROOM_BYTES {
        return Err(format!(
            "internal heap headroom {} fell below {} bytes",
            internal.minimum_free, MIN_INTERNAL_HEADROOM_BYTES
        )
        .into());
    }
    if spiram.minimum_free < MIN_SPIRAM_HEADROOM_BYTES {
        return Err(format!(
            "PSRAM heap headroom {} fell below {} bytes",
            spiram.minimum_free, MIN_SPIRAM_HEADROOM_BYTES
        )
        .into());
    }
    Ok(())
}

struct HeapStats {
    total: usize,
    free: usize,
    minimum_free: usize,
    largest_free: usize,
}

fn heap_stats(capability: u32) -> HeapStats {
    unsafe {
        HeapStats {
            total: sys::heap_caps_get_total_size(capability),
            free: sys::heap_caps_get_free_size(capability),
            minimum_free: sys::heap_caps_get_minimum_free_size(capability),
            largest_free: sys::heap_caps_get_largest_free_block(capability),
        }
    }
}

fn measurement_values(first: u64, count: u64) -> String {
    (first..first + count)
        .map(|minute| format!("({minute}, {})", 18.0 + (minute % 100) as f64 / 10.0))
        .collect::<Vec<_>>()
        .join(", ")
}

struct FatMount {
    handle: sys::wl_handle_t,
}

impl Drop for FatMount {
    fn drop(&mut self) {
        unsafe {
            sys::esp_vfs_fat_spiflash_unmount_rw_wl(MOUNT_PATH.as_ptr(), self.handle);
        }
    }
}

fn mount_fatfs() -> io::Result<FatMount> {
    let config = sys::esp_vfs_fat_mount_config_t {
        format_if_mount_failed: true,
        max_files: 2,
        allocation_unit_size: ALLOCATION_UNIT as usize,
        disk_status_check_enable: false,
        use_one_fat: true,
    };
    let mut handle = sys::WL_INVALID_HANDLE as sys::wl_handle_t;
    esp(unsafe {
        sys::esp_vfs_fat_spiflash_mount_rw_wl(
            MOUNT_PATH.as_ptr(),
            PARTITION_LABEL.as_ptr(),
            &config,
            &mut handle,
        )
    })?;
    Ok(FatMount { handle })
}

fn provision_database_file() -> io::Result<(FixedCapacityFile, FixedFileState)> {
    let path = Path::new(DATABASE_PATH);
    let created = !path.exists();
    if created {
        let mut total = 0u64;
        let mut free = 0u64;
        esp(unsafe { sys::esp_vfs_fat_info(MOUNT_PATH.as_ptr(), &mut total, &mut free) })?;
        let capacity = free
            .saturating_sub(ALLOCATION_UNIT)
            .min(NANO_ESP32.database_bytes as u64)
            / ALLOCATION_UNIT
            * ALLOCATION_UNIT;
        if capacity < 4 * 1024 * 1024 {
            return Err(io::Error::other(format!(
                "FAT partition has only {free} free bytes"
            )));
        }
        let full_path = c"/reddb/data.rdb";
        esp(unsafe {
            sys::esp_vfs_fat_create_contiguous_file(
                MOUNT_PATH.as_ptr(),
                full_path.as_ptr(),
                capacity,
                true,
            )
        })?;
        println!("REDB_MCU_PROVISION capacity_bytes={capacity}");
        zero_file(path, capacity)?;
    }

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let capacity = file.metadata()?.len();
    let state = inspect_state(&mut file)?;
    if state == FixedFileState::Empty && !created {
        zero_file(path, capacity)?;
        file = OpenOptions::new().read(true).write(true).open(path)?;
    }
    println!("REDB_MCU_STORAGE state={state:?} capacity_bytes={capacity}");
    FixedCapacityFile::open(file, capacity, state).map(|file| (file, state))
}

fn inspect_state(file: &mut File) -> io::Result<FixedFileState> {
    let mut page = vec![0; reddb_mcu::engine::storage::page::PAGE_SIZE];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut page)?;
    if database_header_magic_matches(&page) {
        return Ok(FixedFileState::Existing);
    }
    if page.iter().all(|byte| *byte == 0) {
        return Ok(FixedFileState::Empty);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "preallocated database has an ambiguous or corrupt header",
    ))
}

fn zero_file(path: &Path, capacity: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(0))?;
    let zeros = [0u8; ALLOCATION_UNIT as usize];
    for _ in 0..capacity / ALLOCATION_UNIT {
        file.write_all(&zeros)?;
    }
    file.sync_all()
}

fn esp(code: sys::esp_err_t) -> io::Result<()> {
    if code == sys::ESP_OK as sys::esp_err_t {
        Ok(())
    } else {
        Err(io::Error::other(format!("ESP-IDF error {code}")))
    }
}
