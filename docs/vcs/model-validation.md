# VCS validation across collection models

VCS validation must exercise user-visible data, not just branch and commit
metadata. The inventory below describes tests located under
`tests/grouped/vcs` at the time of writing. A listed test is evidence for its
specific scenario, not a guarantee that the model supports every VCS operation.

| Collection model | Existing focused VCS evidence | Important gaps to cover |
| --- | --- | --- |
| Table | Independent branch updates, typed divergent merge, insert/delete, hard reset, cherry-pick/revert, indexed AS OF and recovery | Broader constraint and authorization combinations |
| Document | Historical patch/delete, independent branches, fast-forward/forced merge and recovery | Conflicts involving independent creations of the same application key |
| Graph | Node/edge history, independent branches, actual merge, persisted conflict resolution, recovery and failed publication | Contract enforcement during merge |
| Vector | Autocommit, independent branches, merge, native reads, exact fallback for approximate search, embedded/paged recovery | Additional historical similarity workloads |
| KV | History, independent branch updates, INCR/CAS, tag invalidation, merge, conflicts on independently created keys and recovery | More failure/authorization combinations |
| Mixed | Row/node/vector snapshots, independent additions/deletion, divergent merge and embedded/paged recovery | More combinations of native mutation surfaces |
| TimeSeries | Native points, append-only rejection, independent branch additions, merge and embedded/paged recovery | Retention, chunk and rollup operations |
| Queue | No dedicated model-specific VCS test in this directory | Message state, branch isolation, merge and recovery without unintended delivery side effects |
| Metrics | Prometheus remote write/read, independent branches, actual merge, AS OF values, retention and embedded/paged recovery | More range/aggregation and authorization combinations |
| HLL | Native counts, pre-opt-in state, branches, rollback, conflicting merge resolution, AS OF and embedded/paged recovery | More multi-source unions and failure combinations |
| Sketch | Native frequency estimates, branches, rollback, conflicting merge resolution, AS OF and embedded/paged recovery | More multi-source merges and failure combinations |
| Filter | Native membership, insert/delete rollback, branches, conflicting merge resolution, AS OF and embedded/paged recovery | More capacity and failure combinations |
| Config | Native rotation/history, branches, rollback, deletion, merge, independently created key conflicts and embedded/paged recovery | More authorization and legacy-history combinations |
| Vault | No dedicated model-specific VCS test in this directory | Secret-bearing state, access controls, branch isolation, merge and recovery using synthetic test values |

The catalog exposes all of these models. The opt-in flag alone does not
establish complete VCS support for Queue and Vault. Versioned HLL, Sketch
and Filter now persist their native state in public collection versions;
their reads and writes bypass the shared legacy caches. Enabling history
publishes initial state, provenance and the setting in one WAL batch.

`e2e_vcs_merge_guards` covers all 14 declared models. Supported models apply
fast-forward and forced merges, hard reset, cherry-pick and revert. For the
remaining models it checks that unsupported data application cannot advance
HEAD. Vault uses a synthetic test key and secret; metrics ingests a real
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
