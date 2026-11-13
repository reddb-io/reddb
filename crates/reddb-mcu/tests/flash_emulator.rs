use reddb_mcu::{
    flash_emulator::{FlashEmulatorFile, FlashGeometry, ERASE_BLOCK_BYTES},
    types::Value,
    Database, VfsFile, NANO_ESP32, PICO_2,
};
use std::io::SeekFrom;

#[test]
fn sparse_blocks_reservations_wear_and_reboot_are_deterministic() {
    let mut file = FlashEmulatorFile::new(FlashGeometry::with_capacity(2 * ERASE_BLOCK_BYTES));
    file.set_len((2 * ERASE_BLOCK_BYTES) as u64)
        .expect("sparse logical extension");
    assert_eq!(file.stats().expect("stats").physical_bytes, 0);

    let mut block = vec![0; ERASE_BLOCK_BYTES];
    block[7] = 1;
    file.write_all(&block).expect("program sparse block");
    let programmed = file.stats().expect("program stats");
    assert_eq!(programmed.physical_bytes, ERASE_BLOCK_BYTES);
    assert_eq!(programmed.program_bytes, 256);
    assert_eq!(programmed.erase_operations, 0);

    file.seek(SeekFrom::Start(0)).expect("rewind");
    file.write_all(&block).expect("identical write");
    let identical = file.stats().expect("identical stats");
    assert_eq!(identical.physical_bytes, programmed.physical_bytes);
    assert_eq!(identical.program_bytes, programmed.program_bytes);
    assert_eq!(identical.program_operations, programmed.program_operations);
    assert_eq!(identical.erase_operations, programmed.erase_operations);
    assert_eq!(identical.write_bytes, programmed.write_bytes * 2);

    file.seek(SeekFrom::Start(0)).expect("rewind");
    file.write_all(&vec![0; ERASE_BLOCK_BYTES])
        .expect("erase sparse block");
    let erased = file.stats().expect("erase stats");
    assert_eq!(erased.physical_bytes, 0);
    assert_eq!(erased.erase_operations, 1);

    file.reserve(0, (2 * ERASE_BLOCK_BYTES) as u64)
        .expect("reserve full medium");
    assert_eq!(
        file.stats().expect("reservation stats").physical_bytes,
        2 * ERASE_BLOCK_BYTES
    );
    assert!(file
        .reserve((2 * ERASE_BLOCK_BYTES) as u64, ERASE_BLOCK_BYTES as u64)
        .is_err());
    file.sync_all().expect("persist reservation");
    let mut file = file.power_cycle().expect("power cycle");
    assert_eq!(
        file.stats().expect("recovered reservation").physical_bytes,
        2 * ERASE_BLOCK_BYTES
    );
    file.seek(SeekFrom::Start(0)).expect("rewind");
    file.write_all(&block).expect("unsynchronized write");
    let mut file = file.power_cycle().expect("discard unsynchronized write");
    let mut recovered = vec![1; ERASE_BLOCK_BYTES];
    file.read_exact(&mut recovered).expect("read durable zeros");
    assert!(recovered.iter().all(|byte| *byte == 0));
}

#[test]
fn board_profiles_fill_recover_and_stream_every_acknowledged_row() {
    let pico = fill_board(PICO_2);
    let nano = fill_board(NANO_ESP32);

    assert!(pico.rows >= 5_000, "unexpected Pico 2 capacity: {pico:?}");
    assert!(
        pico.rows < 43_200,
        "30 days unexpectedly fit Pico 2: {pico:?}"
    );
    assert!(nano.rows >= 43_200, "30 days must fit Nano ESP32: {nano:?}");
    assert!(pico.stats.erase_operations > 0);
    assert!(nano.stats.erase_operations > 0);
}

#[derive(Debug)]
struct CapacityResult {
    rows: u64,
    stats: reddb_mcu::flash_emulator::FlashStats,
}

fn fill_board(board: reddb_mcu::BoardProfile) -> CapacityResult {
    const BATCH: u64 = 128;
    let file = FlashEmulatorFile::for_board(board);
    let mut database = Database::create(file).expect("create flash-backed database");
    database
        .query(
            "CREATE TABLE measurements (recorded_at BIGINT NOT NULL, temperature DOUBLE NOT NULL)",
            1_000,
        )
        .expect("create measurement table");

    let mut rows = 0u64;
    loop {
        let values = measurement_values(rows, BATCH);
        if database
            .query(
                &format!("INSERT INTO measurements VALUES {values}"),
                2_000 + rows,
            )
            .is_err()
        {
            break;
        }
        rows += BATCH;
    }

    let file = database
        .into_inner()
        .power_cycle()
        .expect("restart after batch ENOSPC");
    let mut database = Database::open(file).expect("recover acknowledged batches");
    loop {
        let values = measurement_values(rows, 1);
        if database
            .query(
                &format!("INSERT INTO measurements VALUES {values}"),
                3_000 + rows,
            )
            .is_err()
        {
            break;
        }
        rows += 1;
    }

    let file = database
        .into_inner()
        .power_cycle()
        .expect("restart after final ENOSPC");
    let database = Database::open(file).expect("recover full flash");
    let mut expected = 0u64;
    let result = database
        .query_stream(
            "SELECT recorded_at, temperature FROM measurements",
            &[],
            &mut |row| {
                assert_eq!(row[0], Value::BigInt(expected as i64));
                assert!(matches!(row[1], Value::Float(_)));
                expected += 1;
                Ok(true)
            },
        )
        .expect("stream recovered capacity");
    assert_eq!(result.rows, rows);
    assert_eq!(expected, rows);
    let file = database.into_inner();
    let stats = file.stats().expect("final flash stats");
    assert!(stats.physical_bytes <= board.database_bytes);
    eprintln!(
        "board={} rows={} logical_bytes={} physical_bytes={} capacity_bytes={} program_bytes={} program_ops={} erase_ops={} sync_ops={}",
        board.name,
        rows,
        stats.logical_bytes,
        stats.physical_bytes,
        stats.capacity_bytes,
        stats.program_bytes,
        stats.program_operations,
        stats.erase_operations,
        stats.sync_operations,
    );
    CapacityResult { rows, stats }
}

fn measurement_values(first: u64, count: u64) -> String {
    (first..first + count)
        .map(|minute| format!("({minute}, {})", 18.0 + (minute % 100) as f64 / 10.0))
        .collect::<Vec<_>>()
        .join(", ")
}
