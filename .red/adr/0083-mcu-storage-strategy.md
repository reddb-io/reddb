# ADR 0083 — MCU storage strategy over the canonical `.rdb`

Status: accepted; host/emulator MCU slice implemented
Date: 2026-11-11

## Context

ADR 0038 defines embedded as a working-set-in-memory profile and reserves
dataset-greater-than-memory paging for the server roadmap. A microcontroller
needs the opposite physical posture: small bounded RAM with a larger flash
artifact. Treating that as ordinary embedded would silently weaken its accepted
scale contract.

The MCU integration must not introduce a second SQL grammar, executor, catalog,
WAL, page format or database file. Its board package owns host callbacks and
resource policy; persistent behavior remains in the shared engine and
`reddb-file`.

## Decision

Define `mcu` as an experimental storage/deploy profile distinct from the
canonical embedded profile. It uses the same single `.rdb`, file codecs,
logical store actions, recovery ordering and query implementation. It differs
only in resource admission and in the storage strategy used to expose committed
rows and stage a transaction.

Keep `LocalDatabase<F>` as the public resident API. Internally, a generic
database session owns a `StorageStrategy`. The strategy supplies:

- a fallible logical `StoreView` with one-row-at-a-time scan callbacks;
- committed visibility and transaction staging;
- atomic action, catalog and transaction publication; and
- return of the exclusively owned `VfsFile` handle.

The first strategy is a behavior-preserving adapter over
`EmbeddedResidentStore<F>`. It retains the current resident snapshot copy for
`BEGIN`, the current catalog snapshot barrier, the native WAL action path and
the existing failure fence. The public API and stored bytes do not change.

`PagedStrategy<F>` implements `StoreView` with the canonical pager, B-tree,
RER1 entity codec, native manifest and physical metadata document. It selects
one cache page and exposes a callback query path that applies the canonical
filter, projection, offset and limit operators one row at a time. The ordinary
local query path remains available for admitted whole-result operations. No
second SQL or RQL dispatcher exists.

The bounded writer currently admits `CREATE TABLE` and append-only `INSERT`.
Transactions, `UPDATE`, `DELETE`, catalog extensions it cannot preserve, and
streaming plans that require whole-set materialization fail before publication.
Those are explicit implementation limits, not alternate semantics. A future
bounded transaction overlay and external operator spill can narrow the gap
without changing the strategy boundary or file format.

This ADR proposes the MCU exception to ADR 0038's embedded scale contract. It
does not broaden the canonical embedded profile and does not claim support for
every board. Each adapter and resource envelope requires its own qualification.

## Invariants

- Opening an MCU-produced `.rdb` with a canonical reader uses the same format
  and version checks. Profile settings are capabilities, not alternate codecs.
- Catalog changes and their data actions share one publication boundary.
- A failed or ambiguous durable write fences the owner until reopen.
- Recovery never depends on a sidecar or gateway.
- SQL admission, scalar semantics and type tags have one implementation.
- Board-specific wear reduction lives below `VfsFile`; it cannot reorder or
  omit the engine's durability barriers.

## Cost sketch

The resident strategy retains its existing O(database) recovered memory and
O(database) transaction snapshot copy. The paged strategy keeps one 16 KiB
cache page, one decoded row, query-operator state and one statement's admitted
mutation records in memory. Streaming scans are O(visited rows) and bounded in
resident row count. INSERT uniqueness checks remain bounded by scanning
existing rows one at a time, at the cost of O(existing rows × inserted rows)
work when a uniqueness rule exists.

One prepared native double-write frame holds at most the existing DWB entry
capacity. Page images are staged in the canonical on-file DWB zone; RAM retains
only page identities. A bounded VFS reserves backing capacity for every
destination page before the frame header is sealed, so a full medium rejects
the operation while recovery still owns the old state. An oversized statement
fails and fences the writer until reopen rather than publishing a prefix.

## Verification

The extraction keeps the existing local SQL, Vault, WAL interruption and
catalog atomicity tests green. The implemented slice additionally verifies:

- one-page cache operation and one-page owned read buffers;
- 43,200 minute measurements (30 days) inserted and streamed after reboot;
- all injected write/sync interruption boundaries of a multi-page prepared
  batch, accepting only the complete old or complete new state;
- the portable pager/query conformance suite; and
- read-only SQL access to an MCU-produced `.rdb` through the canonical 1.33.3
  server runtime.

This is host/emulator qualification, not a hardware support claim. RedDB 1.33.3
still depends on Rust `std`; concrete Nano ESP32 and Pico 2 firmware builds,
flash wear receipts, physical capacity and power-cut campaigns remain required
per board.
