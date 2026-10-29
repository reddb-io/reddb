# Resident row and metadata accounting

Native paged open restores physical records into segment managers. The page
cache does not make these resident rows evictable. Accounting therefore needs
their heap ownership as well as cached page payloads.

Row estimates include column-vector capacity, named-field hash buckets and
vacant slots, owned field-name capacity, and value allocations. Text includes
the shared allocation's counters and alignment. Shared values remain charged
per resident owner, independent of their current reference counts.

Segments additionally charge unused entity-directory slots, retained kind-index
sets and tombstone buckets. Entity and kind capacities keep high-water
observations: deletion can change a hash table's reported usable capacity
without returning its allocation. Flat vectors charge spare slots separately
from the bodies already charged per physical row.

Typed metadata updates cached ownership counters during insertion, replacement
and removal. These include both copies of field names, nested payloads,
entity-key sets, typed trees and retained hash-table capacity. Nested ownership
uses bounded iterative traversal; an unrepresentable API value conservatively
closes its owner's accounting until reclamation. Sampling visits no stored
metadata values or entity payloads.

## First-column index removal

The old `pk_index` created a debug-formatted copy of every positional row's
first value. It was unrelated to declared PRIMARY KEY metadata. Query pruning
extracts only physical `rid` probes; registered user-key lookups have separate
indexes. The obsolete first-column map and its update snapshots are removed.
Unknown probes remain conservative. Physical RID membership still uses the
segment's physical entity lookup. No persistent index format changes.

The independent allocation regression covers native manager restoration at
1,000 and 4,000 rows, positional/named values and per-row metadata. It also
checks replacement stability, empty retained capacity and physical deletion.
For 100 bulk rows with 40 deletions, restoring the old first-column map in a
controlled comparison changed live requested allocation from 72,428 to 78,608
bytes during consolidation. Removing it changed 56,582 to 55,718 bytes. These
are local requested allocations, not cloud RSS measurements.

## Estimates and limits

Hash-table and tree bounds cover vacant slots, control bytes and nodes. Their
conservative padding differs between flat and hash storage. The estimated
`consolidation.bytes_reclaimed` can consequently be zero while independently
measured live allocation decreases. Lifetime tests verify that publication
releases the old owner; allocator tests separately check real reclamation.
Reader-pinned ownership stays charged.

Payload counting is proportional to the changed payload. Sampling is constant
in entity count, with a fixed six-kind directory inventory. There is no new
WAL, pager publication, lock or file-format change.

This is not a hard process RSS bound or disk residency. Query intermediates,
allocator overhead, admission before directory resize/maintenance peaks,
other auxiliary ownership and eager historical loading remain qualification
work. The exact final artifact must pass Auth and Platform content, index,
recovery, snapshot, latency and repeated-space tests before fleet rollout.
