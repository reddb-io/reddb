#![allow(dead_code)]

#[path = "support/codec_corpus.rs"]
mod codec_corpus;
#[path = "support/pager.rs"]
mod support;

use reddb_engine::{
    query::local_database::PagedLocalDatabase,
    storage::{
        page::PAGE_SIZE,
        pager::{NoWal, Pager, PagerConfig},
    },
};
use reddb_file::VfsFile;
use reddb_types::Value;
use std::io::{self, SeekFrom};
use support::MemoryFile;

struct LimitedMemoryFile(MemoryFile);

impl LimitedMemoryFile {
    fn empty() -> Self {
        let mut file = MemoryFile::default();
        file.incremental_sync = true;
        Self(file)
    }

    fn reboot(self) -> Self {
        Self(MemoryFile::from_bytes(self.0.durable.into_inner()))
    }
}

#[test]
fn thirty_days_of_minute_measurements_stream_after_reboot() {
    const DAYS: u64 = 30;
    const ROWS: u64 = DAYS * 24 * 60;
    const BATCH: u64 = 128;

    let mut database = PagedLocalDatabase::create(LimitedMemoryFile::empty())
        .expect("create bounded paged history");
    database
        .query(
            "CREATE TABLE measurements (recorded_at BIGINT NOT NULL, temperature DOUBLE NOT NULL)",
            1_000,
        )
        .expect("create measurement table");
    for start in (0..ROWS).step_by(usize::try_from(BATCH).expect("batch fits usize")) {
        let end = (start + BATCH).min(ROWS);
        let values = (start..end)
            .map(|minute| format!("({minute}, {})", 18.0 + (minute % 100) as f64 / 10.0))
            .collect::<Vec<_>>()
            .join(", ");
        database
            .query(
                &format!("INSERT INTO measurements VALUES {values}"),
                2_000 + start,
            )
            .unwrap_or_else(|error| panic!("insert minute batch {start}..{end}: {error}"));
    }

    let file = database.into_inner();
    let logical_bytes = file.file_len().expect("logical .rdb bytes");
    let reopened = PagedLocalDatabase::open(file.reboot()).expect("reopen 30-day history");
    assert_eq!(reopened.cache_pages(), 1);
    let mut expected_minute = 0u64;
    let summary = reopened
        .query_stream(
            "SELECT recorded_at, temperature FROM measurements",
            &[],
            &mut |row| {
                assert_eq!(row[0], Value::BigInt(expected_minute as i64));
                assert!(matches!(row[1], Value::Float(_)));
                expected_minute += 1;
                Ok(true)
            },
        )
        .expect("stream every recovered measurement");
    assert_eq!(summary.rows, ROWS);
    assert!(summary.complete);
    assert_eq!(expected_minute, ROWS);
    eprintln!("rows={ROWS} logical_rdb_bytes={logical_bytes}");
}

impl VfsFile for LimitedMemoryFile {
    fn max_read_buffer_bytes(&self) -> usize {
        PAGE_SIZE
    }

    fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    fn file_len(&self) -> io::Result<u64> {
        self.0.file_len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        let required = offset.checked_add(len).ok_or(io::ErrorKind::InvalidInput)?;
        self.0.set_len(required)
    }

    fn write_all(&mut self, buffer: &[u8]) -> io::Result<()> {
        self.0.write_all(buffer)
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }
}

#[test]
fn canonical_sql_round_trips_with_one_page_cache_and_bounded_reads() {
    let mut database = PagedLocalDatabase::create(LimitedMemoryFile::empty())
        .expect("create canonical paged database");
    assert_eq!(database.cache_pages(), 1);
    database
        .query(
            "CREATE TABLE measurements (recorded_at BIGINT PRIMARY KEY, temperature DOUBLE NOT NULL)",
            1_000,
        )
        .expect("create through canonical SQL");

    let values = (0..32)
        .map(|minute| format!("({minute}, {})", 20.0 + f64::from(minute) / 10.0))
        .collect::<Vec<_>>()
        .join(", ");
    database
        .query(&format!("INSERT INTO measurements VALUES {values}"), 2_000)
        .expect("insert through canonical SQL");
    assert_eq!(
        database
            .query(
                "SELECT recorded_at, temperature FROM measurements ORDER BY recorded_at DESC LIMIT 2",
                3_000,
            )
            .expect("select through canonical SQL")
            .rows,
        [
            vec![Value::BigInt(31), Value::Float(23.1)],
            vec![Value::BigInt(30), Value::Float(23.0)],
        ]
    );
    let mut streamed = Vec::new();
    let summary = database
        .query_stream(
            "SELECT recorded_at, temperature FROM measurements WHERE recorded_at >= 30",
            &[],
            &mut |row| {
                streamed.push(row.to_vec());
                Ok(true)
            },
        )
        .expect("stream canonical filter and projection");
    assert_eq!(summary.columns, ["recorded_at", "temperature"]);
    assert_eq!(summary.rows, 2);
    assert!(summary.complete);
    assert_eq!(
        streamed,
        [
            vec![Value::BigInt(30), Value::Float(23.0)],
            vec![Value::BigInt(31), Value::Float(23.1)],
        ]
    );
    assert!(database
        .query_stream(
            "SELECT recorded_at FROM measurements ORDER BY recorded_at",
            &[],
            &mut |_| Ok(true),
        )
        .is_err());
    assert!(database.query("BEGIN", 3_000).is_err());
    assert!(database
        .query(
            "UPDATE measurements SET temperature = 0 WHERE recorded_at = 0",
            3_000,
        )
        .is_err());

    let file = database.into_inner().reboot();
    let mut reopened = PagedLocalDatabase::open(file).expect("recover after simulated reboot");
    assert_eq!(reopened.cache_pages(), 1);
    assert_eq!(
        reopened
            .query(
                "SELECT recorded_at FROM measurements ORDER BY recorded_at",
                4_000
            )
            .expect("query recovered rows")
            .rows
            .len(),
        32
    );
    assert_eq!(
        reopened
            .query(
                "SELECT recorded_at FROM measurements ORDER BY recorded_at DESC LIMIT 1",
                4_000,
            )
            .expect("ordered recovered rows")
            .rows,
        [vec![Value::BigInt(31)]]
    );
}

#[test]
fn bounded_writer_rejects_catalog_extensions_it_cannot_preserve() {
    let mut database = PagedLocalDatabase::create(LimitedMemoryFile::empty())
        .expect("create bounded paged database");
    database
        .query("CREATE TABLE samples (id BIGINT PRIMARY KEY)", 1_000)
        .expect("create sample table");
    let file = database.into_inner();

    let pager = Pager::open_owned(
        file,
        "extended.rdb".into(),
        PagerConfig {
            cache_size: 1,
            ..PagerConfig::default()
        },
        NoWal,
        false,
    )
    .expect("open canonical pager");
    let mut catalog = pager.read_manifest_page().expect("read catalog");
    catalog.as_bytes_mut()[PAGE_SIZE - 1] = 1;
    pager.write_page(1, catalog).expect("write extension byte");
    pager.sync().expect("publish extended catalog");
    let file = pager.into_inner().reboot();

    let error = PagedLocalDatabase::open(file)
        .err()
        .expect("bounded writer must reject unknown catalog extensions");
    assert!(error.to_string().contains("cannot preserve"));
}

#[test]
fn bounded_hosts_reject_guarded_recovery_that_exceeds_their_read_budget() {
    let pager = Pager::open_owned(
        LimitedMemoryFile::empty(),
        "guarded-bounded.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true,
    )
    .expect("create canonical file");
    pager
        .fence_required_capabilities(reddb_file::file_format::FILE_CAPABILITY_DWB_PUBLICATION)
        .expect("enable guarded recovery before dependent writes");
    let baseline = pager.into_inner().0.durable.into_inner();
    for read_only in [true, false] {
        let file = LimitedMemoryFile(MemoryFile::from_bytes(baseline.clone()));
        let operations = file.0.operations.clone();
        let error = Pager::open_owned(
            file,
            "guarded-bounded.rdb".into(),
            PagerConfig {
                read_only,
                ..PagerConfig::default()
            },
            NoWal,
            false,
        )
        .err()
        .expect("host cannot allocate the guarded recovery buffer");
        assert!(error.to_string().contains("owned read budget"), "{error}");
        assert!(operations.borrow().is_empty());
    }
}
