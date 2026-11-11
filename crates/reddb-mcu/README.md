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

This revision qualifies the host/emulator integration surface. It does not yet
claim that the 1.33.3 dependency graph cross-compiles for every Arduino or the
Pico SDK: the current engine uses Rust `std`, so each firmware target still needs
a compatible Rust runtime/toolchain or a later platform-abstraction slice.
Physical flash layout, sparse pieces, wear accounting and power-cut behavior
belong in the concrete board `VfsFile`, never in the SQL engine or `.rdb` format.
Flash adapters must implement `VfsFile::reserve` so capacity exhaustion is
reported before a prepared double-write frame is sealed.

## Host flash-model qualification

`flash_emulator` models 4 KiB erase blocks, 256-byte program units, sparse zero
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
