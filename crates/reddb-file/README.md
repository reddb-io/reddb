# reddb-io-file

The file artifact authority for RedDB. This crate owns the durable byte-level
contracts that cross a persistence or recovery boundary: single-file `.rdb`
layout, sidecar names, WAL envelopes, checkpoints, manifests, locks, serverless
boot artifacts, and primary-replica file planning.

`reddb-io-file` does not execute SQL and does not own storage-engine semantics.
It gives the rest of the workspace one canonical place for paths, binary frames,
checksums, and recovery metadata.

## When to use it

Use this crate when code needs to produce or inspect RedDB persistence
artifacts without pulling in the full server:

- derive canonical paths for data files, support directories, WAL files,
  metadata, serverless roots, and primary-replica roots;
- encode or decode file-visible artifacts such as WAL records, backup manifests,
  column blocks, native store pages, graph/vector index artifacts, and page
  headers;
- plan primary-replica timelines, basebackups, relay logs, and replication-slot
  catalogs;
- plan serverless generations, boot indexes, hot/cold packs, hydration ranges,
  and local cache entries.

If you need query execution, use `reddb-io-server`. If you need protocol frames
or connection strings, use `reddb-io-wire`.

## Install

```toml
[dependencies]
reddb-io-file = "1.13"
```

The Rust import name is `reddb_file`:

```rust
use std::path::Path;

use reddb_file::{
    primary_replica_root, serverless_root, support_dir_for, unified_wal_path_in,
};

let data_path = Path::new("./data.rdb");
let support_dir = support_dir_for(data_path);
let wal_path = unified_wal_path_in(&support_dir, data_path);

let replica_root = primary_replica_root(data_path);
let serverless_root = serverless_root(data_path);

assert!(wal_path.ends_with("data.rdb-uwal"));
assert!(replica_root.ends_with("data.primary-replica"));
assert!(serverless_root.ends_with("data.serverless"));
```

## What it owns

- `embedded` and `file_format`: `.rdb` superblocks, page headers, page checksums,
  paged-encryption headers, and double-write-buffer frames.
- `layout`: canonical file names, support directories, atomic temp paths,
  metadata sidecars, WAL paths, audit logs, primary-replica roots, and
  serverless roots.
- `logical_wal`, `store_wal`, `transaction_wal`, and `wal_record`: persisted WAL
  framing, checksums, spool versions, and repair helpers.
- `backup_manifest`, `backup_temp`, and `operational_manifest`: backup,
  snapshot, archived-WAL, and operational manifest JSON/binary contracts.
- `primary_replica`: retained timeline planning, basebackup parts, WAL segment
  files, relay logs, replication-slot catalogs, and timeline history.
- `serverless`: generation pointers, manifests, boot indexes, extent indexes,
  hot packs, hydration plans, and local cache metadata.
- Native physical artifacts for indexes, graphs, vectors, column blocks, bloom
  segments, blob cache, physical metadata, and export data.

## Compatibility posture

Every function in this crate is part of a persistence boundary. Changes that
alter bytes, names, checksum input, version constants, or write ordering are
format changes and need explicit migration thinking. Runtime adapters may wrap
these APIs, but they should not redeclare equivalent artifact layouts inside
`reddb-io-server`.

## Verification

```sh
cargo test -p reddb-io-file
cargo check -p reddb-io-file
```

Useful focused tests:

```sh
cargo test -p reddb-io-file --test storage_layout
cargo test -p reddb-io-file --test embedded_rdb_artifact
cargo test -p reddb-io-file --test primary_replica_basebackup_crash
cargo test -p reddb-io-file --test serverless_current_pointer_crash
```

## References

- [Monorepo structure](../../docs/dev/monorepo-structure.md)
- [ADR 0003 - disk format v1](../../.red/adr/0003-disk-format-v1.md)
- [ADR 0046 - wire and file crate authority](../../.red/adr/0046-wire-file-crate-authority-boundary.md)
