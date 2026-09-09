# Embedded append tail validation

Issue #2270 exposed an append cost that grows with the live WAL: every write
read and decoded all earlier frames to recover the sequence and CRC chain.
The process now retains those three tail scalars under the existing per-path
mutex after a successful append.

Before reuse, the writer holds the file lock, validates the superblock and
manifest, and compares the selected superblock plus Unix device, inode, file
length, mtime and ctime (including nanoseconds). A different writer, checkpoint,
replacement or observed file mutation forces a full scan. Platforms without
that identity vocabulary always scan. This assumes cooperating writers take
the file lock; arbitrary concurrent file edits are not a supported writer API.

The cache is discarded before fallible append I/O and before checkpoint.
It is republished only after frame `sync_data`, superblock `sync_all`, final
metadata validation and successful unlock. Neither durability barrier, WAL
encoding, checksum chain, ACK boundary nor checkpoint fence changes. A scan
that finds a corrupt published WAL refuses append; recovery and salvage can
still inspect the valid prefix. All read/recovery paths continue scanning and
checking frames, including faults invisible to filesystem timestamps.

Resource sketch: one fixed-size tail entry (under 512 bytes, no payloads or open
handles) per entry in the existing process path-lock registry. The registry
already retains paths for the process lifetime. Cache hits remove O(live WAL
bytes) reads and payload allocations per append, adding two metadata reads.
Misses retain the prior scan cost. Snapshot validation and both sync calls
remain, so this is not a constant-cost or zero-sync commit claim.

Validation includes concurrent threads, another process, checkpoint/wrap,
pathname replacement, corruption followed by repair, and crash injection at
frame write, frame sync and superblock publication. Run:

```sh
cargo test -p reddb-io-file --test embedded_rdb_artifact
```

The larger #2270 comparison still requires release-equivalent workloads and
durability. Shared-host timings are diagnostic, not evidence of leadership.
