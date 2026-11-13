# VCS validation across collection models

VCS validation must exercise user-visible data, not just branch and commit
metadata. The inventory below describes tests located under
`tests/grouped/vcs` at the time of writing. A listed test is evidence for its
specific scenario, not a guarantee that the model supports every VCS operation.

| Collection model | Existing focused VCS evidence | Important gaps to cover |
| --- | --- | --- |
| Table | Historical table scan after an update | Inserts, deletes, branch isolation, merge application and recovery |
| Document | Historical patch and delete; concurrent update conflict | Inserts, branch isolation, merge application and recovery |
| Graph | Node and edge history, checkpoint recovery, conflict detection and WAL publication; native read visibility | Branch isolation and application of merges |
| Vector | Autocommit history after reopen, delete visibility, concurrent delete conflict, SELECT AS OF payloads, pending-delete recovery in embedded and paged checkpoints | Historical similarity search, branch isolation and merge application |
| KV | Historical writes and deletes, conflicts, and commits from another connection | Branch isolation, merge application and recovery |
| Mixed | No dedicated model-specific VCS test in this directory | Mixed-model snapshots, branch isolation, merge and recovery |
| TimeSeries | No dedicated model-specific VCS test in this directory | Point insertion/deletion, historical reads, branch isolation, merge and recovery |
| Queue | No dedicated model-specific VCS test in this directory | Message state, branch isolation, merge and recovery without unintended delivery side effects |
| Metrics | No dedicated model-specific VCS test in this directory | Samples and derived state, historical reads, branch isolation, merge and recovery |
| HLL | No dedicated model-specific VCS test in this directory | State changes, historical reads, branch isolation, diff, merge and recovery |
| Sketch | No dedicated model-specific VCS test in this directory | State changes, historical reads, branch isolation, diff, merge and recovery |
| Filter | No dedicated model-specific VCS test in this directory | State changes, historical reads, branch isolation, diff, merge and recovery |
| Config | No dedicated model-specific VCS test in this directory | Configuration values, historical reads, branch isolation, merge and recovery |
| Vault | No dedicated model-specific VCS test in this directory | Secret-bearing state, access controls, branch isolation, merge and recovery using synthetic test values |

The catalog exposes all of these models, but `vcs_set_versioned` currently
stores an opt-in flag without checking the collection model. That flag alone
does not establish historical reads or merge support. HLL, sketch and filter
state is stored under internal `red_config` keys, while `vcs_diff` skips
internal collections; tests must verify how their state becomes part of VCS
instead of treating the flag as evidence of support.

`e2e_vcs_merge_guards` covers all 14 declared models. It checks that unsupported
data application cannot advance HEAD through fast-forward merge, forced merge,
cherry-pick or revert. Vault uses a synthetic test key and secret; metrics
ingests a real sample through Prometheus remote write. These are not successful
merge or history tests.
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
