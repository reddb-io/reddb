# Context index memory admission

The optional `context_index` stores token postings, field/value postings and
reverse lookup keys in memory. These allocations previously did not contribute
to `red.stats`' shared memory budget. In the RC384 reproduction, enabling this
index on the same 20,000 rows increased anonymous memory from about 121 MiB to
391 MiB before the first full query, while both runs reported about 50 MiB of
governed pools. A controlled allocator trim did not release the live index.
This experiment identifies a contribution, not the entire historical Platform
memory discrepancy.

The index now maintains a conservative resident allocation counter as its maps
and posting lists change. Retained vector and hash-map capacities remain charged
after removal. The counter is sampled into `IndexMemory` alongside native row
indexes; it is an allocation bound, not an RSS measurement. Sampling does not
walk the index or clone entity payloads.

Runtime inserts, bulk inserts, updates and explicit context reindexing reserve
the projected index growth before publication. Updates reserve the full new
image without borrowing credit from removing its preimage. Complex JSON and
array values use a conservative capped bound without parsing another owned
document before admission. Reservations share the existing runtime budget and
remain held through publication and cleanup. Rejection preserves the existing
row; a rejected bulk insert publishes no rows.

This is containment and accounting, not disk residency. Conservative bounds can
reject an operation before actual heap usage reaches the configured limit.
The budget also does not represent all process RSS, allocator fragmentation,
thread stacks or every query intermediate. Opening and rebuilding historical
stores still needs separate memory qualification: the storage loader operates
before runtime mutation admission. Data larger than RAM requires a persisted,
paged context index and bounded working buffers; this patch does not implement
that architecture or authorize a fleet rollout or volume reduction.

Regression coverage includes opt-in reporting, insertion/update/bulk rejection,
preservation of row/search content, repeated replacement and removal, retained
posting capacity, and independent allocator measurements of resident bytes and
per-mutation peaks. The exact final artifact still requires Auth and Platform
recovery, latency, memory and repeated storage qualification.
