//! Measure real, compressible values, not a sparse file full of holes.
#![cfg(any(target_os = "linux", windows))]

use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::path::Path;
use std::time::Instant;

const VALUE_BYTES: usize = 1024 * 1024;
const MEMORY_LIMIT: usize = 1536 * 1024 * 1024;
const DISK_LIMIT: u64 = 1024 * 1024 * 1024;
const ENV_PATH: &str = "REDDB_POPULATED_STARTUP_PATH";

#[cfg(target_os = "linux")]
fn limit_memory() {
    let limit = libc::rlimit {
        rlim_cur: MEMORY_LIMIT as _,
        rlim_max: MEMORY_LIMIT as _,
    };
    // Isolate this limit to the fixture child; the parent and host are unaffected.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) }, 0);
}

#[cfg(windows)]
fn limit_memory() {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        assert!(
            !job.is_null(),
            "create memory limit: {}",
            std::io::Error::last_os_error()
        );
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = MEMORY_LIMIT;
        assert_ne!(
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of_val(&info) as u32
            ),
            0,
            "set memory limit: {}",
            std::io::Error::last_os_error()
        );
        assert_ne!(
            AssignProcessToJobObject(job, GetCurrentProcess()),
            0,
            "apply memory limit: {}",
            std::io::Error::last_os_error()
        );
        // Keep the job handle alive for the lifetime of this short-lived child.
    }
}

#[cfg(target_os = "linux")]
fn peak_memory() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        * 1024
}

#[cfg(windows)]
fn peak_memory() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut info = PROCESS_MEMORY_COUNTERS::default();
    assert_ne!(
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut info,
                std::mem::size_of_val(&info) as u32,
            )
        },
        0
    );
    info.PeakWorkingSetSize as u64
}

fn config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(8),
        resident_memory_bytes_max: Some(64 * 1024 * 1024),
        ..Default::default()
    }
}

#[cfg(target_os = "linux")]
fn allocated_bytes(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().blocks() * 512
}

fn fixture_bytes(directory: &Path) -> u64 {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => fixture_bytes(&path),
                Ok(metadata) if metadata.is_file() => metadata.len(),
                _ => 0,
            }
        })
        .sum()
}

#[cfg(windows)]
fn allocated_bytes(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0;
    unsafe { SetLastError(0) };
    let low = unsafe { GetCompressedFileSizeW(path.as_ptr(), &mut high) };
    assert!(low != INVALID_FILE_SIZE || unsafe { GetLastError() } == 0);
    (u64::from(high) << 32) | u64::from(low)
}

#[test]
fn populated_startup_child() {
    let Some(path) = std::env::var_os(ENV_PATH) else {
        return;
    };
    limit_memory();
    if std::env::var_os("REDDB_POPULATED_STARTUP_TRACE").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("reddb::storage::startup=debug")
            .try_init();
    }
    let path = Path::new(&path);
    let rows: u64 = std::env::var("REDDB_POPULATED_STARTUP_ROWS")
        .unwrap()
        .parse()
        .unwrap();
    let phase = std::env::var("REDDB_POPULATED_STARTUP_PHASE").unwrap();
    let start = Instant::now();
    if phase == "seed" {
        let store = UnifiedStore::open(path).expect("create fixture");
        store.create_collection("rows").unwrap();
        let payload = Value::text("x".repeat(VALUE_BYTES));
        store
            .get_collection("rows")
            .unwrap()
            .bulk_insert(
                (1..=rows)
                    .map(|id| {
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::Integer(id as i64), payload.clone()],
                        )
                    })
                    .collect(),
            )
            .expect("insert fixture rows");
        store.persist().expect("persist fixture");
    } else {
        let runtime = if phase.starts_with("runtime") {
            Some(
                RedDBRuntime::with_options(
                    RedDBOptions::persistent(path)
                        .with_memory_budget(64 * 1024 * 1024)
                        .with_bounded_disk_residency(true),
                )
                .expect("start runtime with populated fixture"),
            )
        } else {
            None
        };
        let store = match &runtime {
            Some(runtime) => runtime.db().store(),
            None => std::sync::Arc::new(
                UnifiedStore::open_with_config(path, config()).expect("open populated fixture"),
            ),
        };
        let ready = start.elapsed();
        assert_eq!(store.get_collection("rows").unwrap().count() as u64, rows);
        assert!(store.segment_memory_bytes() <= 64 * 1024 * 1024);
        for id in [1, rows / 2, rows] {
            let row = store
                .try_get("rows", EntityId::new(id), 64 * 1024 * 1024)
                .unwrap()
                .unwrap();
            let values = &row.data.as_row().unwrap().columns;
            assert_eq!(values[0], Value::Integer(id as i64));
            assert!(
                matches!(&values[1], Value::Text(text) if text.len() == VALUE_BYTES && text.as_bytes().iter().all(|&b| b == b'x'))
            );
        }
        let first_samples = start.elapsed();
        if phase == "prepare" {
            store.persist().expect("publish derived roots");
        }
        eprintln!("phase={phase} ready={ready:?} first_samples={first_samples:?} resident_segment_bytes={} peak_working_set_bytes={}", store.segment_memory_bytes(), peak_memory());
        if phase == "runtime" {
            let build_started = Instant::now();
            store
                .create_collection("fixture_writes")
                .expect("collection during projection");
            if rows >= 1024 {
                std::thread::sleep(std::time::Duration::from_millis(200));
                assert!(
                    store.try_count_from_index("rows", None).unwrap().is_none(),
                    "fixture must exercise an unfinished projection"
                );
            }
            let mut samples = 0u64;
            let mut max_write = std::time::Duration::ZERO;
            let mut max_read = std::time::Duration::ZERO;
            loop {
                let ready = store.try_count_from_index("rows", None).unwrap();
                if ready.is_some() && samples > 0 {
                    assert_eq!(ready, Some(rows));
                    break;
                }
                assert!(
                    build_started.elapsed()
                        < std::time::Duration::from_secs(if rows <= 1024 { 60 } else { 1800 }),
                    "count projection did not complete"
                );
                if samples >= 128 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                samples += 1;
                let write_started = Instant::now();
                store
                    .insert(
                        "fixture_writes",
                        UnifiedEntity::table_row(
                            EntityId::new(rows + samples),
                            "fixture_writes",
                            samples,
                            vec![Value::text("concurrent")],
                        ),
                    )
                    .expect("write during projection");
                max_write = max_write.max(write_started.elapsed());
                let read_started = Instant::now();
                assert!(store
                    .try_get(
                        "rows",
                        EntityId::new((samples - 1) % rows + 1),
                        64 * 1024 * 1024
                    )
                    .unwrap()
                    .is_some());
                max_read = max_read.max(read_started.elapsed());
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            assert_eq!(
                store.try_count_from_index("rows", None).unwrap(),
                Some(rows)
            );
            eprintln!(
                "projection_elapsed={:?} concurrent_samples={samples} max_write={max_write:?} max_read={max_read:?}", build_started.elapsed()
            );
            let checkpoint = Instant::now();
            runtime
                .as_ref()
                .unwrap()
                .flush()
                .expect("checkpoint completed projection");
            eprintln!("visibility_checkpoint={:?}", checkpoint.elapsed());
        }
        if phase == "runtime-reopen" {
            let samples = store.get_collection("fixture_writes").unwrap().count() as u64;
            assert!(samples > 0);
            for sample in 1..=samples {
                assert!(
                    store
                        .try_get(
                            "fixture_writes",
                            EntityId::new(rows + sample),
                            64 * 1024 * 1024
                        )
                        .unwrap()
                        .is_some(),
                    "concurrent write survives reopen"
                );
            }
        }
    }
    eprintln!(
        "phase={phase} elapsed={:?} peak_working_set_bytes={}",
        start.elapsed(),
        peak_memory()
    );
}

fn run_fixture(rows: u64) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("populated.rdb");
    let legacy_runtime_path = directory.path().join("legacy-runtime.rdb");
    for phase in [
        "seed",
        "runtime-legacy",
        "prepare",
        "open",
        "runtime",
        "runtime-reopen",
    ] {
        eprintln!("starting phase={phase}");
        let phase_path = if phase == "runtime-legacy" {
            &legacy_runtime_path
        } else {
            &path
        };
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "populated_startup_child", "--nocapture"])
            .env(ENV_PATH, phase_path)
            .env("REDDB_POPULATED_STARTUP_ROWS", rows.to_string())
            .env("REDDB_POPULATED_STARTUP_PHASE", phase)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run fixture child");
        loop {
            if fixture_bytes(directory.path()) > DISK_LIMIT {
                child.kill().expect("stop oversized fixture child");
                let _ = child.wait();
                panic!("fixture exceeded the 1 GiB total file-size guard");
            }
            if child.try_wait().expect("poll fixture child").is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let result = child
            .wait_with_output()
            .expect("collect fixture child output");
        eprint!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.status.success(),
            "{phase} child failed: {}",
            result.status
        );
        assert!(
            fixture_bytes(directory.path()) <= DISK_LIMIT,
            "fixture exceeded 1 GiB on disk"
        );
        if phase == "seed" {
            assert!(
                fixture_bytes(directory.path()) + std::fs::metadata(&path).unwrap().len()
                    <= DISK_LIMIT
            );
            std::fs::copy(&path, &legacy_runtime_path).expect("copy legacy checkpoint");
        }
    }
    eprintln!(
        "logical_value_bytes={} file_bytes={} allocated_database_bytes={}",
        rows * VALUE_BYTES as u64,
        std::fs::metadata(&path).unwrap().len(),
        allocated_bytes(&path)
    );
}

#[test]
fn populated_startup_smoke() {
    run_fixture(32);
}

#[test]
#[ignore = "manual 50 GiB logical-data measurement"]
fn populated_startup_50_gib() {
    let rows = std::env::var("REDDB_POPULATED_ROWS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(50 * 1024);
    run_fixture(rows);
}
