# ADR 0083 — MCU storage strategy over the canonical `.rdb`

Status: proposed; resident strategy extraction implemented
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

A later MCU strategy may implement `StoreView` with canonical pager/B-tree
cursors and a bounded transaction overlay. It must not implement a second SQL
or RQL dispatcher. Operators that require materialization, sorting, joins or
other memory above the local budget either fail with the existing didactic
resource error or execute through the explicit gateway contract; no operation
silently changes semantics.

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
O(database) transaction snapshot copy. The interface adds one indirect scan
callback per visited entity on the local database path; it adds no file I/O,
network operation or stored bytes. The future MCU strategy targets O(cache +
transaction overlay + result budget) RAM while scan work remains O(visited
rows). Operators that require materialization remain explicitly bounded.

## Verification

The extraction must keep the existing local SQL, Vault, WAL interruption,
catalog atomicity and canonical-reader tests green. Engine and file suites must
remain green, and `layout_authority` must confirm that no format authority moved
out of `reddb-file`. The later MCU strategy additionally requires deterministic
RAM admission, restart/fault campaigns, canonical-reader interoperability and
per-board emulator or hardware receipts before support is claimed.
