# reddb-io-mcu

Embedded integration package for RedDB 1.33.3. It composes board-owned byte I/O
with the canonical RedDB SQL/RQL parser, local executor, pager, B-tree, RER1
records and `.rdb` layout.

The board supplies a `VfsFile`; `Database<F>` supplies bounded paged SQL with a
one-page cache. Use `query_stream` for histories that cannot be materialized in
RAM. The bounded writer currently admits `CREATE TABLE` and append-only
`INSERT`; streaming plans that require whole-set work such as `ORDER BY` are
rejected before execution. Files with catalog extensions this writer cannot
preserve are rejected before mutation. A canonical 1.33.3 server can open and
query the `.rdb` produced by this package.

This revision qualifies the host integration surface and the Nano ESP32's
dual-core `xtensa-esp32s3-espidf` firmware in Espressif `esp-emu` 0.51.0. It
does not claim that the graph cross-compiles for every Arduino or the Pico SDK:
the Pico 2 target has no Rust `std`, so it still needs the `no_std + alloc`
slice. Physical flash layout, sparse pieces, wear accounting and power-cut
behavior belong in the concrete board `VfsFile`, never in the SQL engine or
`.rdb` format. Flash adapters must implement `VfsFile::reserve` so capacity
exhaustion is reported before a prepared double-write frame is sealed.

## Nano ESP32 cross-check

The `mcu` Cargo features retain the shared SQL/RQL, types, catalog codecs,
pager, B-tree and record codecs. Host-only resident storage, process file locks,
Rayon, Vault crypto, serverless/replica planning and deterministic fault
injection remain in the default `full` feature set.

With the `espup` ESP32-S3 toolchain installed, reproduce the library check with:

```text
cargo +esp check -Z build-std=std,panic_abort -p reddb-io-mcu --lib --target xtensa-esp32s3-espidf
```

The standalone project in `firmware/nano-esp32` mounts FAT over ESP-IDF wear
levelling, preallocates a 12,378,112-byte file, and wraps it in
`FixedCapacityFile`. The adapter changes physical allocation semantics only;
the shared pager, catalog, SQL executor and `.rdb` bytes remain unchanged.

The dual-core emulator receipt ingests 16,384 measurements, injects a one-shot
CPU reset during the third file write of the next SQL transaction, recovers
exactly 16,384 rows, continues to 43,200, and streams the complete history. A
second process boot opens `state=Existing` and queries the same 43,200 rows
without reinsertion or gateway. The ESP-IDF allocator prefers PSRAM for general
heap allocations; every measured phase enforces at least 32 KiB of internal
heap and 512 KiB of PSRAM headroom. The observed minima were 79,824 internal
bytes and 8,224,908 PSRAM bytes. Physical Nano ESP32 execution and electrical
power-cut testing remain separate qualification boundaries.

## Host flash-model qualification

The opt-in `host-flash-model` feature models 4 KiB erase blocks, 256-byte program units, sparse zero
blocks, capacity reservation, sync durability and power cycles entirely below
`VfsFile`. The SQL capacity test fills each board profile, retries the final
range one row at a time, power-cycles, and streams every acknowledged row.

| Profile | Rows | Logical `.rdb` | Physical bytes | Usable capacity |
| --- | ---: | ---: | ---: | ---: |
| Raspberry Pi Pico 2 | 15,413 | 4,194,304 | 2,596,864 | 2,621,440 |
| Arduino Nano ESP32 | 77,981 | 17,170,432 | 12,337,152 | 12,378,112 |

These are deterministic host-model receipts, not instruction-set emulator or
physical-board claims. The separate Nano firmware receipt covers the concrete
FAT/wear-levelling adapter in an instruction-set emulator; physical power-cut
behavior still requires hardware qualification.
