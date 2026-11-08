# ADR 0081: Four deployment modes and one canonical database artifact

Status: accepted for implementation

RedDB has one product contract across official language facades. Every facade
must expose the same query, parameter, helper, transaction, error and
authentication vocabulary in four deployment modes:

1. **serverless** — connect to a managed, attachable RedDB endpoint;
2. **embedded** — run the engine in-process against memory or one `.rdb` file;
3. **server with read replicas** — discover topology, send writes to the active
   primary and route explicitly declared reads according to their consistency;
4. **cluster** — discover ownership, follow redirects and retry safely across
   primary or range-owner changes.

Transport support is not deployment-mode support. HTTP, RedWire and gRPC are
mechanisms. A package that can dial one address but cannot consume topology or
survive promotion does not support the replica or cluster mode. A separate
embedded package and remote package do not make either public facade compliant;
the language's official facade must select the implementation from the URI.

## Qualification

A mode is `qualified` only when an automated journey opens or connects through
the public facade, creates schema, writes, reads, closes or disconnects, and
validates the mode-specific property. Embedded additionally reopens the same
single file. Serverless exercises attach/warm/cold lifecycle. Replica exercises
read routing, promotion, post-promotion writes and causal/strong reads. Cluster
exercises ownership movement, redirects, retry idempotency and recovery after an
interrupted request. Unit tests, URI parsing and compile-only checks may justify
`partial`; they cannot justify `qualified`.

The release gate reads
`docs/conformance/deployment-mode-matrix.json`. Every official facade has all
four cells. Missing support cannot be represented by an omitted cell and a
facade is four-mode ready only when every cell is qualified.

## Shared embedded boundary

Non-Rust language drivers embed one native engine through the versioned C ABI in
`crates/reddb-ffi`; they do not reimplement persistence or spawn the `red`
binary. The ABI owns an in-process `EmbeddedRpcSession`, so transactions,
cursors, prepared statements and future methods use the same dispatcher as the
server. Language bindings own URI parsing, typed value conversion and ergonomic
async wrappers. Rust may call the engine directly, but must pass the same
conformance journey.

The C ABI uses opaque handles and caller-owned output strings, serializes calls
within a session, discards an uncommitted transaction before checkpoint, and
checkpoints before releasing the file lock. A release engine panic aborts the
host process; unwinding through database mutation paths is not a supported
recovery mechanism. WAL recovery restores the single file on the next open.

## Single-file invariant

The canonical durable database artifact is one `.rdb` file in every mode. A
live service may create caches, downloaded extents, temporary range-transfer
files or other operational state only when that state is bounded, disposable
or reconstructible from the canonical database and its replicated durability
protocol. Backup, restore, export and node bootstrap must accept or produce one
portable `.rdb` artifact. No user must copy an undocumented directory tree to
obtain a complete database.

This decision supersedes the packaging decision in ADR 0040 that made an
operational directory the canonical cluster layout. Existing operational
directories remain a migration input while their state is folded into zoned
single-file storage or made explicitly reconstructible. The change does not
permit several processes to write the same local file: embedded retains one
local writer, while server and cluster nodes coordinate replicated ownership.

## Safe routing defaults

Arbitrary SQL is sent to the active primary or range owner. RedDB has effectful
statements whose leading token is `SELECT`, so thin clients must not infer
read-only behavior from a keyword heuristic. Replica routing is an explicit
read API carrying `Strong`, `Causal`, `BoundedStaleness` or `Local`
consistency. A transport that cannot enforce the requested consistency fails
closed with `CONSISTENCY_UNSUPPORTED`; it never silently drops the option.

Topology publication changes the active primary, endpoint pools and membership
as one client snapshot. New endpoints are connected before publication, and
in-flight requests may finish on their already-owned channel. Writes begun
after publication use the new primary.

## Consequences

- The existing JS embedded/remote package split and the remote-only drivers are
  tracked as product gaps, not documented as four-mode support.
- Stable transport APIs may remain stable while the facade is not yet marked
  four-mode ready.
- The native ABI is packaged per supported OS/architecture and reused by C++,
  Go, .NET, JVM, Dart, PHP and Zig adapters.
- Release and documentation jobs fail if a qualified/partial cell loses its
  evidence, a language omits a mode, or readiness disagrees with the four cells.
- Mode-specific conformance is intentionally integration-heavy; parser or mock
  tests supplement but do not replace the required journeys.
