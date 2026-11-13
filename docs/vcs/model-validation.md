# VCS validation across collection models

VCS validation must exercise user-visible data, not just branch and commit
metadata. The inventory below describes tests located under
`tests/grouped/vcs` at the time of writing. A listed test is evidence for its
specific scenario, not a guarantee that the model supports every VCS operation.

| Collection model | Focused VCS evidence |
| --- | --- |
| Table | Independent branch updates, typed divergent merge, insert/delete, hard reset, cherry-pick/revert, indexed AS OF and recovery |
| Document | Historical patch/delete, independent branches, fast-forward/forced merge and recovery |
| Graph | Node/edge history, independent branches, actual merge, persisted conflict resolution, optional contract validation, recovery and failed publication |
| Vector | Autocommit, independent branches, merge, native reads, exact fallback for approximate search, embedded/paged recovery |
| KV | History, independent branch updates, INCR/CAS, tag invalidation, merge, conflicts on independently created keys and recovery |
| Mixed | Row/node/vector snapshots, independent additions/deletion, divergent merge and embedded/paged recovery |
| TimeSeries | Native points, append-only rejection, independent branch additions, merge and embedded/paged recovery |
| Queue | Payload history, isolated leases/ACK, rollback, merge, deduplication, move, retry, fanout, purge, WAL failures and embedded/paged recovery |
| Metrics | Prometheus remote write/read, independent branches, actual merge, AS OF values, retention and embedded/paged recovery |
| HLL | Native counts, pre-opt-in state, branches, rollback, conflicting merge resolution, AS OF and embedded/paged recovery |
| Sketch | Native frequency estimates, branches, rollback, conflicting merge resolution, AS OF and embedded/paged recovery |
| Filter | Native membership, insert/delete rollback, branches, conflicting merge resolution, AS OF and embedded/paged recovery |
| Config | Native rotation/history, branches, rollback, deletion, merge, independently created key conflicts and embedded/paged recovery |
| Vault | Sealed rotations, branch-local history, rollback, merge, multiple writes in one transaction, purge protection and embedded/paged recovery |

The catalog exposes all of these models. Versioned HLL, Sketch
and Filter now persist their native state in public collection versions;
their reads and writes bypass the shared legacy caches. Enabling history
publishes initial state, provenance and the setting in one WAL batch.

`e2e_vcs_merge_guards` covers all 14 declared models. Every model applies
fast-forward and forced merges, hard reset, cherry-pick and revert. Vault uses a synthetic test key and secret; metrics ingests a real
sample through Prometheus remote write. Probabilistic checks assert counts
and membership after every operation, not just the number of stored rows.
`e2e_vcs_native_visibility` exercises pending inserts and commit visibility through
native point reads and pagination for table, document, graph, vector and KV.
It also combines snapshots with tenant pagination and graph/document RLS.
Separate regressions check that alternating and simultaneous committers on one
branch keep every successful commit reachable.

Vector checkpoint recovery covers an embedded checkpoint copy and abrupt
process exits with full and warmed paged checkpoints. An uncommitted deletion
must not become committed when the database reopens.

For every model, a complete regression suite should cover the applicable public
read and write surfaces, values captured before and after a change, writes and
commits from different connections, concurrent writers, independent branch
updates and deletes, and reopening the database. Merge coverage should check
clean additions and changes, deletes, identical edits, conflicts, resolution,
and failures before and after durable publication. Include result caches,
indexes, tenancy and RLS where the model exposes them. Tests for queues,
metrics and probabilistic structures must assert their actual state and side
effects rather than substituting a table-shaped example.

Tenant regressions exercise denied fast-forward and divergent merges, hard
reset and diff, with unchanged HEAD and data after rejection. Vault presentation
excludes sealed envelopes as well as plaintext. Native Vault regressions cover
metadata-only reads, reveal capabilities, auditing and file recovery.

Queue payloads are historical data. Delivery leases, acknowledgements, retry
delays and deduplication belong to a branch writer epoch. Merge does not copy
leases, and moving HEAD starts a new operational scope. Queue configuration
and consumer group definitions remain shared catalog metadata.

Optional graph contracts are validated on admission, update, delete, transaction
commit, merge and hard reset. Enabling a contract checks every live branch.
`e2e_graph_contract` covers typed properties, endpoints, tenancy, concurrent
endpoint changes, embedded/paged reopen, and the HTTP API. Each vertex has one
concrete type. `e2e_hybrid_relations` checks text/vector seeds, selected edge
labels, one-hop paths, score fusion, RLS, branch isolation and transaction views.

Metadata publication tests inject WAL failures into branch creation/deletion,
checkout, tags, soft/mixed reset and merges. Missing ancestry or malformed
references fail the operation instead of producing partial history.

The native time-series read bridge bypasses shared column blocks for versioned
collections and resolves the authorized branch snapshot from canonical entities.
This also applies to blocks sealed before history was enabled. Versioned
collections skip columnar sealing; low-level columnar scans return no block.
Regression coverage checks independent branch points, merge, transactional
rollback, and embedded/paged reopen with a pre-existing compressed chunk.
