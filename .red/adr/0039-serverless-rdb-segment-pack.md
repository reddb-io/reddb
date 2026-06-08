# Serverless RDB Segment Pack

Status: proposed

RedDB's serverless storage profile keeps `.rdb` as the canonical database
artifact while allowing runtimes to export, hydrate, and cache a derived segment
pack for object storage and fast boot.

## Decisions

**The canonical artifact remains `.rdb`.** Serverless RedDB must not introduce a
second logical database format that users have to reason about separately from
embedded/local RedDB.

**Segment packs are derived operational packaging.** A serverless deployment may
represent an `.rdb` checkpoint as a manifest plus immutable parts and delta WAL
segments to support multipart copy, object storage caching, hot boot, and
incremental hydration.

**The segment pack must round-trip.** Exporting `.rdb` to a segment pack and
hydrating the segment pack back to `.rdb` must preserve the same logical database
state, checksums, and recovery boundary.

**Serverless packaging does not weaken embedded single-file semantics.** The
embedded profile's normal durable artifact remains one `.rdb` file. Segment packs
are a serverless/runtime optimization, not a mandatory sidecar set for local use.

## Considered Options

- **Canonical `.rdb` plus derived segment pack.** Chosen because it preserves one
  user-facing database artifact while enabling serverless-specific boot and copy
  performance.
- **Serverless-only segmented format.** Rejected because it creates a second
  storage contract and makes movement between embedded/local and serverless
  harder to reason about.
- **Exact embedded `.rdb` only.** Rejected because large mutable single-file
  objects are a poor fit for object storage, multipart copy, and cold/hot boot
  optimization.

## Consequences

- Serverless tooling needs explicit export/hydrate validation rather than treating
  arbitrary directory contents as the database.
- Segment manifests need versioning, checksums, and a clear checkpoint/WAL
  boundary.
- The storage engine should keep logical file-format compatibility separate from
  serverless transport/caching concerns.
