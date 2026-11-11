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

This revision qualifies the host/emulator integration surface and cross-checks
the MCU feature set for the Nano ESP32's `xtensa-esp32s3-espidf` target. It does
not claim that the graph cross-compiles for every Arduino or the Pico SDK: the
Pico 2 target has no Rust `std`, so it still needs the `no_std + alloc` slice.
Physical flash layout, sparse pieces, wear accounting and power-cut behavior
belong in the concrete board `VfsFile`, never in the SQL engine or `.rdb` format.
Flash adapters must implement `VfsFile::reserve` so capacity exhaustion is
reported before a prepared double-write frame is sealed.

## Nano ESP32 cross-check

The `mcu` Cargo features retain the shared SQL/RQL, types, catalog codecs,
pager, B-tree and record codecs. Host-only resident storage, process file locks,
Rayon, Vault crypto, serverless/replica planning and deterministic fault
injection remain in the default `full` feature set.

With the `espup` ESP32-S3 toolchain installed, reproduce the library check with:

```text
cargo +esp check -Z build-std=std,panic_abort -p reddb-io-mcu --lib --target xtensa-esp32s3-espidf
```

This proves target compilation, not execution. Linking a firmware image,
mounting a flash partition through a concrete `VfsFile`, and running it in
Wokwi and on the Nano ESP32 are the next qualification boundary.

## Host flash-model qualification

The opt-in `host-flash-model` feature models 4 KiB erase blocks, 256-byte program units, sparse zero
blocks, capacity reservation, sync durability and power cycles entirely below
`VfsFile`. The SQL capacity test fills each board profile, retries the final
range one row at a time, power-cycles, and streams every acknowledged row.

| Profile | Rows | Logical `.rdb` | Physical bytes | Usable capacity |
| --- | ---: | ---: | ---: | ---: |
| Raspberry Pi Pico 2 | 15,413 | 4,194,304 | 2,596,864 | 2,621,440 |
| Arduino Nano ESP32 | 80,904 | 17,776,640 | 12,791,808 | 12,832,768 |

These are deterministic host-model receipts, not instruction-set emulator or
physical-board claims. Filesystem metadata, controller wear levelling and real
power-cut behavior still require the concrete LittleFS/FatFS adapters.
