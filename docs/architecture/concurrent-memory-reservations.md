# Concurrent growth reservations

The runtime admission seam reserves estimated growth against the shared memory
budget. Two operations on the same runtime (including its clones) cannot each
spend the same sampled headroom before either operation publishes its writes.
This extends [segment lifetime accounting](retained-segment-memory.md).

## Ownership and publication

Admission samples resident usage and reserves growth under one runtime mutex.
The check uses checked addition: resident bytes + outstanding reservations +
requested growth must fit the resolved budget. A successful call returns a
must-use RAII guard. Row mutation, explicit secondary-index creation, and
timeseries insertion retain their guards through their existing write scopes.
Timeseries segment and index guards coexist, so their estimates must fit together.

The mutex is released before mutation. Dropping a guard only marks its bytes
completed, including on early error return or panic unwind. It does not acquire
storage locks or assume the operation left no data behind. A subsequent sample,
under the same mutex, replaces completed reservations with actual resident usage.
Active guards remain charged. No older runtime sample can publish afterward and
erase the usage that justified returning the reservation.

The lock order is runtime reservation mutex, then the existing sampler's storage
locks (segment consolidation, growing, sealed, retired; secondary-index readers).
Pressure maintenance runs outside the reservation mutex and retries admission
through the same atomic check. Callers must retain the guard through writes and
release storage guards before releasing the reservation. The reservation mutex
is never retained by a successful admission guard.

Pool accounting and `red.stats` usage/high-water counters continue to describe
resident estimates. Reservations are separate from those counters; budget errors
include outstanding reserved bytes. A reporting refresh cannot erase an active
reservation. Public low-level accounting counter writes are observability APIs,
not an alternative admission protocol.

## Cost and limits

Cost sketch: each existing admission still performs one O(S + R + C) inventory
sample, plus O(1) reservation bookkeeping under a mutex; guard completion adds one
O(1) mutex acquisition. There is no per-reservation heap allocation and no added
WAL or network work. Concurrent samples serialize; writes run without holding the
reservation mutex. This is a correctness change, not measured throughput evidence.

Active writes can already appear in resident samples while their full reservation
is still outstanding. This deliberately conservative overlap can reject a request
earlier near the ceiling. Completed guards stop contributing after the next sample;
unused reservations do not leak and partial writes remain charged as resident data.

Estimates remain approximate. This does not establish a hard process RSS ceiling,
change transaction rollback behavior, or add admission to currently ungoverned
paths. [Row index admission](row-index-memory-admission.md) now covers registered
secondary indexes and the implicit id index in the row mutation engine. Remaining
work includes complete metadata estimates, other model/update paths, query buffers, maintenance
headroom before copying, and removed collections with outstanding handles.
Independently constructed runtimes do not gain a process-global reservation pool.

## Verification

A barrier test reproduces the original defect with two concurrent writers: both
were admitted for the entire available headroom. Only one is admitted after the
fix. Further tests cover concurrent sampling, reservations across pools, releasing
one guard while another remains active, partial writes followed by error, panic
unwind, and a public timeseries insert whose segment fits but combined segment and
index growth does not. Existing storage, SQL, transactions and multimodel suites
check the caller integration and existing visibility/persistence contracts.
