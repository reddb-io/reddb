# Standalone — RedDB Domain Glossary

Part of the [glossary map](../CONTEXT-MAP.md). The embedded single-file single-node posture. The shared storage engine underneath lives in [Persistence](persistence.md); the fast-boot/object-storage posture lives in [Serverless](serverless.md).

## Embedded single-file profile

- **Embedded single-file profile** — SQLite-like RedDB posture for embedded, local, test, plugin, and prototype use where one `.rdb` file contains all required durable state.
- **Embedded internal manifest** — authoritative manifest inside a zoned single-file `.rdb`, rooted by the file's superblock. It maps internal zones and logical objects such as collections, indexes, WAL region, free-space state, and checkpoint boundary without requiring an external `red.manifest`.
- **Embedded superblock pair** — two ping-pong superblock copies inside the single-file `.rdb`, each carrying generation and checksum metadata. Open chooses the newest valid copy to root the embedded internal manifest.
- **Embedded WAL region** — circular internal WAL zone inside the single-file `.rdb`. Entries may be overwritten only after the checkpoint/superblock boundary proves they are no longer needed for recovery.

## Migration path

- **Profile migration path** — supported conversion between storage/deploy profiles. The first official path is offline embedded single-file `.rdb` to operational directory layout, matching the expected growth path from local/prototype to production. The source database is closed, checkpoint-validated, and exported into an operational directory.
