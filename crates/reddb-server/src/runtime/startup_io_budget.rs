//! Startup I/O must follow live roots, not the database file's logical extent.

#![cfg(any(target_os = "linux", windows))]

use crate::storage::engine::pager::measure_pager_read_bytes;
use crate::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

const LOGICAL_LEN_50_GIB: u64 = 50 * 1024 * 1024 * 1024;

#[cfg(target_os = "linux")]
fn allocated_bytes(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).expect("database metadata").blocks() * 512
}

#[cfg(windows)]
fn allocated_bytes(path: &std::path::Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0u32;
    // INVALID_FILE_SIZE is also a possible low word; clear and check the error.
    unsafe { SetLastError(0) };
    let low = unsafe { GetCompressedFileSizeW(path.as_ptr(), &mut high) };
    assert!(
        low != INVALID_FILE_SIZE || unsafe { GetLastError() } == 0,
        "cannot query allocated bytes: {}",
        std::io::Error::last_os_error()
    );
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(target_os = "linux")]
fn make_sparse(_file: &std::fs::File) {}

#[cfg(windows)]
fn make_sparse(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut returned = 0u32;
    // The live file handle belongs to the private test fixture. FSCTL_SET_SPARSE
    // without input sets the sparse flag before extending the file.
    let success = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        success,
        0,
        "cannot create sparse fixture: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(target_os = "linux")]
fn process_read_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(windows)]
fn process_read_bytes() -> u64 {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessIoCounters, IO_COUNTERS,
    };
    let mut counters = IO_COUNTERS::default();
    // The pseudo-handle addresses this test process and counters is writable.
    let success = unsafe { GetProcessIoCounters(GetCurrentProcess(), &mut counters) };
    assert_ne!(
        success,
        0,
        "cannot read process I/O: {}",
        std::io::Error::last_os_error()
    );
    counters.ReadTransferCount
}

fn options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true)
}

fn count_rows(runtime: &RedDBRuntime) -> String {
    runtime
        .execute_query("SELECT COUNT(*) AS n FROM startup_rows")
        .expect("count persisted rows")
        .result
        .records[0]
        .get("n")
        .expect("count column")
        .display_string()
}

#[test]
fn disk_resident_startup_io_is_independent_of_sparse_logical_size() {
    let directory = tempfile::tempdir().expect("startup I/O fixture");
    let path = directory.path().join("startup.rdb");
    compare_sparse_startup(&path, options(&path), "disk-resident");
}

#[test]
fn resident_paged_startup_io_is_independent_of_sparse_logical_size() {
    let directory = tempfile::tempdir().expect("startup I/O fixture");
    let path = directory.path().join("startup.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(crate::storage::StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("paged profile");
    compare_sparse_startup(&path, options, "resident");
}

fn compare_sparse_startup(path: &std::path::Path, options: RedDBOptions, profile: &str) {
    let seed = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
    seed.execute_query("CREATE TABLE startup_rows (id INTEGER PRIMARY KEY, payload TEXT)")
        .expect("create startup table");
    for id in 0..64 {
        seed.execute_query_with_params(
            "INSERT INTO startup_rows (id, payload) VALUES ($1, $2)",
            &[Value::Integer(id), Value::text("payload".repeat(512))],
        )
        .expect("insert startup row");
    }
    seed.flush().expect("checkpoint startup fixture");
    drop(seed);

    // The first disk-resident reopen may publish missing derived roots. Keep
    // both measured opens on the same stable checkpoint generation.
    let prepared = RedDBRuntime::with_options(options.clone()).expect("prepare cold reopen");
    assert_eq!(count_rows(&prepared), "64");
    prepared.flush().expect("publish prepared roots");
    drop(prepared);

    let (small, small_read_bytes) = measure_pager_read_bytes(|| {
        let start = Instant::now();
        let runtime = RedDBRuntime::with_options(options.clone()).expect("open compact file");
        let ready = start.elapsed();
        assert_eq!(count_rows(&runtime), "64");
        eprintln!(
            "compact: ready={ready:?}, first_result={:?}",
            start.elapsed()
        );
        runtime
    });
    drop(small);
    measure_fresh_process(path, profile, "compact");

    let allocated_bytes_before = allocated_bytes(&path);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open sparse fixture");
    make_sparse(&file);
    file.set_len(LOGICAL_LEN_50_GIB)
        .expect("extend sparse logical length");
    drop(file);
    let sparse_metadata = std::fs::metadata(&path).expect("sparse database metadata");
    let allocated_bytes_after = allocated_bytes(&path);
    assert_eq!(sparse_metadata.len(), LOGICAL_LEN_50_GIB);
    // NTFS sparse ranges are accounted in compression units rather than the
    // 4 KiB blocks reported by Linux filesystems.
    let allocation_margin = if cfg!(windows) { 64 * 1024 } else { 4096 };
    assert!(
        allocated_bytes_after <= allocated_bytes_before.saturating_add(allocation_margin),
        "50 GiB logical fixture allocated {} additional bytes",
        allocated_bytes_after.saturating_sub(allocated_bytes_before)
    );
    measure_fresh_process(path, profile, "sparse-50gib");

    let (large, large_read_bytes) = measure_pager_read_bytes(|| {
        let start = Instant::now();
        let runtime =
            RedDBRuntime::with_options(options.clone()).expect("open 50 GiB logical file");
        let ready = start.elapsed();
        assert_eq!(count_rows(&runtime), "64");
        eprintln!(
            "sparse: ready={ready:?}, first_result={:?}",
            start.elapsed()
        );
        runtime
    });
    eprintln!("startup sparse fixture: logical={} allocated_before={} allocated_after={} compact_read={} sparse_read={}", sparse_metadata.len(), allocated_bytes_before, allocated_bytes_after, small_read_bytes, large_read_bytes);
    let read_byte_margin = 8_u64.saturating_mul(
        u64::try_from(crate::storage::engine::PAGE_SIZE).expect("page size fits u64"),
    );
    assert!(
        large_read_bytes <= small_read_bytes.saturating_add(read_byte_margin),
        "startup and first-query data-file reads grew from {} to {} bytes",
        small_read_bytes,
        large_read_bytes
    );
    large
        .execute_query("INSERT INTO startup_rows (id, payload) VALUES (100, 'tail-proof')")
        .expect("write after sparse reopen");
    large.flush().expect("checkpoint after sparse reopen");
    drop(large);
    let reopened = RedDBRuntime::with_options(options).expect("reopen after sparse write");
    assert_eq!(count_rows(&reopened), "65");
    let row = reopened
        .execute_query("SELECT payload FROM startup_rows WHERE id = 100")
        .unwrap();
    assert!(
        matches!(row.result.records[0].get("payload"), Some(Value::Text(text)) if text.as_ref() == "tail-proof")
    );
    let original = reopened
        .execute_query("SELECT payload FROM startup_rows WHERE id = 0")
        .unwrap();
    let expected = "payload".repeat(512);
    assert!(
        matches!(original.result.records[0].get("payload"), Some(Value::Text(text)) if text.as_ref() == expected.as_str())
    );
}

fn measure_fresh_process(path: &std::path::Path, profile: &str, size: &str) {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime::startup_io_budget::paged_startup_child",
            "--nocapture",
        ])
        .env("REDDB_TEST_PAGED_STARTUP_PATH", path)
        .env("REDDB_TEST_PAGED_STARTUP_PROFILE", profile)
        .output()
        .expect("run startup child");
    eprintln!(
        "fresh process profile={profile} size={size}\n{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success(),
        "startup child failed: {}",
        result.status
    );
}

#[test]
fn paged_startup_child() {
    let Some(path) = std::env::var_os("REDDB_TEST_PAGED_STARTUP_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let profile = std::env::var("REDDB_TEST_PAGED_STARTUP_PROFILE").unwrap();
    let options = match profile.as_str() {
        "disk-resident" => options(&path),
        "resident" => RedDBOptions::persistent(&path)
            .with_storage_profile(
                crate::storage::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .unwrap(),
        _ => panic!("unknown startup profile"),
    };
    let before = process_read_bytes();
    let ((runtime, ready, first_result), pager_bytes) = measure_pager_read_bytes(|| {
        let start = Instant::now();
        let runtime = RedDBRuntime::with_options(options).expect("open paged fixture");
        let ready = start.elapsed();
        assert_eq!(count_rows(&runtime), "64");
        (runtime, ready, start.elapsed())
    });
    let bytes = process_read_bytes().saturating_sub(before);
    eprintln!("ready={ready:?}, first_result={first_result:?}, pager_read_bytes={pager_bytes}, process_read_bytes={bytes}");
    drop(runtime);
}

fn embedded_fixture_payload() -> String {
    let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_";
    let mut state = 0x8a5c_769d_3124_bef1u64;
    (0..256 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(alphabet[(state as usize) & 63])
        })
        .collect()
}

#[test]
fn embedded_startup_bounds_snapshot_reads() {
    const ENV_PATH: &str = "REDDB_TEST_EMBEDDED_STARTUP_IO_PATH";
    if let Some(path) = std::env::var_os(ENV_PATH) {
        let path = std::path::PathBuf::from(path);
        let read_only = std::env::var("REDDB_TEST_EMBEDDED_STARTUP_READ_ONLY").unwrap() == "1";
        let before = process_read_bytes();
        let start = Instant::now();
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path).with_read_only(read_only))
                .expect("embedded reopen");
        let ready = start.elapsed();
        let count = count_rows(&runtime);
        let first_result = start.elapsed();
        let row = runtime
            .execute_query("SELECT payload FROM startup_rows WHERE id = 0")
            .unwrap();
        let expected = embedded_fixture_payload();
        assert!(
            matches!(row.result.records[0].get("payload"), Some(Value::Text(text)) if text.as_ref() == expected.as_str())
        );
        let bytes = process_read_bytes().saturating_sub(before);
        let snapshot_len: u64 = std::env::var("REDDB_TEST_EMBEDDED_STARTUP_SNAPSHOT_BYTES")
            .unwrap()
            .parse()
            .unwrap();
        eprintln!("embedded: read_only={read_only}, snapshot_bytes={snapshot_len}, read_bytes={bytes}, ready={ready:?}, first_result={first_result:?}");
        assert_eq!(count, "32");
        // A writable boot durably replaces system keys in one batch,
        // whose publication validates the existing snapshot once more.
        let snapshot_passes = if read_only { 1 } else { 2 };
        assert!(
            bytes <= snapshot_len * snapshot_passes + 2 * 1024 * 1024,
            "startup read {bytes} bytes for a {snapshot_len}-byte snapshot"
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("embedded.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        runtime
            .execute_query("CREATE TABLE startup_rows (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        let payload = Value::text(embedded_fixture_payload());
        for id in 0..32 {
            runtime
                .execute_query_with_params(
                    "INSERT INTO startup_rows (id, payload) VALUES ($1, $2)",
                    &[Value::Integer(id), payload.clone()],
                )
                .unwrap();
        }
        // Keep the measured cold boots focused on steady-state startup I/O.
        // Activating the result-cache snapshot format is a one-time migration
        // that checkpoints the embedded file and has its own regressions.
        assert!(runtime.result_cache_snapshot().0.is_some());
        runtime.flush().unwrap();
    }
    let artifact = crate::storage::EmbeddedRdbArtifact::open(&path).unwrap();
    let snapshot_len = artifact.manifest.snapshot_bytes;
    assert!(
        snapshot_len >= 3 * 1024 * 1024,
        "fixture snapshot too small: {snapshot_len}"
    );
    for read_only in ["1", "0"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::startup_io_budget::embedded_startup_bounds_snapshot_reads",
                "--nocapture",
            ])
            .env(ENV_PATH, &path)
            .env("REDDB_TEST_EMBEDDED_STARTUP_READ_ONLY", read_only)
            .env(
                "REDDB_TEST_EMBEDDED_STARTUP_SNAPSHOT_BYTES",
                snapshot_len.to_string(),
            )
            .output()
            .unwrap();
        eprint!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.status.success(),
            "embedded startup child failed: {}",
            result.status
        );
    }
}
