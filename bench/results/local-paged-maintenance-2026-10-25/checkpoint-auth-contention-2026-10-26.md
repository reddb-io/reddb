# Native preparation blocks audited login, 2026-10-26

Historical characterization frozen at commit
`e5048b52e933d94c6872e8a8dc48ce3e90c09a3d`. The current affirmative regression
and candidate correction are described in
[native preparation generations](native-preparation-generations-2026-10-26.md).

Runtime source remains `dd5e81d475ac6e6438947f2e289278ad52eee0cc`.
The intervening commits through `1a83c6f982c2d49e8872248035eb0c8f7d5a59fd`
change only experiment reports. This experiment adds an integration test and
does not change the engine or qualify a new container.

## Question and reproduction

Can the publication fence alone produce the historical login deadline, without
the large Platform corpus, CPU-intensive index construction or queued requests?

`crates/reddb-server/tests/checkpoint_auth_contention.rs` runs a real paged
runtime, `AuthStore`, `RuntimeLedger`, HTTP listener and checkpoint. A
thread-local tracing subscriber pauses the checkpoint at the existing
`native artifact snapshots started` event. It waits on a channel rather than
spinning; no runtime instrumentation, injected HTTP delay or fabricated ledger
error is introduced. The ledger wrapper signals immediately before and after
delegating to the real persistent ledger.

1. A normal login returns 200 and completes its ledger insertion.
2. The real checkpoint reaches native preparation and waits for release.
3. One login validates its credentials and enters the real ledger. Its
   insertion cannot complete while preparation is paused.
4. HTTP returns 503 with `handler deadline exceeded`.
5. Releasing preparation lets checkpoint and ledger insertion complete.
6. A subsequent login returns 200. An explicit checkpoint and cold reopen
   preserve all three additional authentication events.

The first diagnostic used a one-second HTTP deadline and passed in 3.18 s.
The committed test retains and asserts the ordinary **30-second** server
deadline. Neither this test nor the short diagnostic changes the historical
10-second checkpoint or 64 MiB allocated-growth acceptance criteria.

The final test passed in 32.00 s under Rust 1.95.0. The login entered its ledger
after 452.395 ms; HTTP returned 503 at 30.003 s while the checkpoint remained
paused. Cold reopen found all three distinct login IDs with the expected
`auth.login`, `allowed`, `user` and `alice` fields. The preceding ordinary-deadline
run also reproduced the failure (ledger entry 1.123 s, response 30.004 s).
The test ran in its own scope capped at 8 GiB RAM and 4 GiB swap; it did not
use a historical or active database file. This is local causal evidence, not
cloud performance qualification.

## Attribution and decision

The checkpoint's `flush_local_only()` holds `vault_publication_lock` across
store publication and physical metadata/native artifact preparation.
`persist_native_physical_header()` and `prepare_native_artifact_publication()`
also acquire that reentrant lock. Successful authentication synchronously calls
`RuntimeLedger::emit()`, whose `UnifiedStore::insert_auto()` needs the same
lock on the HTTP worker thread. Credentials can therefore be valid while the
HTTP handler exceeds its deadline waiting to record the decision.

This establishes that the fence is sufficient to reproduce the availability
failure. CPU saturation and HTTP queueing are unnecessary for this reproduction;
they are not excluded as additional contributors to historical latency.
It does not prove that every deployed login failure has this cause.

The timed-out HTTP handler's blocking engine task continues: after release,
its event is inserted even though its successful response was not delivered.
That is not evidence of missing acknowledged writes, nor an attribution of
historical gigabytes to login retries. The test checks clean cold recovery of
events, not process-kill or power-loss recovery.

**Decision: RC375 remains rejected.** Do not remove the publication fence from
row/WAL insertion, bypass required audit recording, or extend the login timeout.
The next small engine experiment must separate expensive derived-index
preparation from publication while retaining an exact source proof. Exercise a
mutation between preparation and publication: stale prepared bytes must be
rejected or retried conservatively, and login must complete while preparation
is paused. Bound retained source memory and retry work before measuring the
historical input. Final WAL sync, DWB, metadata ordering and retirement gates
remain mandatory.

An incremental full-text path that decodes, traverses and serializes the entire
frame is still proportional to index size. A faster hash builder or compression
alone does not establish bounded publication latency.

## Relation to established engines

PostgreSQL's checkpointer writes dirty buffers under its WAL ordering and
coordinates final checkpoint publication; routine writes do not require one
global mutex held across reconstruction of every index. Autovacuum recovers
obsolete tuple space subject to snapshot horizons; ordinary reuse is distinct
from shrinking files.

WiredTiger checkpoints use a snapshot, reconcile dirty pages, skip clean pages
and checkpoint metadata last. Schema/transaction locks are taken for specific
phases; its checkpoint lock serializes checkpoints rather than every ordinary
write. Its block manager tracks extents and retains blocks needed by older
checkpoints/readers before reuse. Neither engine is lock-free or immune to
checkpoint I/O pressure. These mechanisms support the direction of bounded
maintenance with short publication phases, not a claim of RedDB parity.

References: [PostgreSQL WAL/checkpoints](https://www.postgresql.org/docs/current/wal-configuration.html),
[routine vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html),
[WiredTiger checkpoints](https://source.wiredtiger.com/develop/arch-checkpoint.html),
[block manager](https://source.wiredtiger.com/develop/arch-block.html).

## Mission gates

| Requirement | Source / artifact | Evidence and result | Remaining decision |
| --- | --- | --- | --- |
| Attribute initial Platform allocation | `dd5e81d475` / RC375 | 22,175,744 allocated-byte growth; prior report explains ownership changes | Initial scope only; repeated cycles still open |
| Explain login 503 | Same runtime source / local native test | Real HTTP and ledger with sleeping checkpoint reproduce contention | Correct preparation/publication coupling |
| Checkpoint below 10 s | RC375 historical Platform | 50.782 s: rejected | Same final artifact must pass Auth and Platform |
| Repeated load, pinned snapshot, drain/reuse, confirmed-write crash recovery | Final artifact not selected | Not established by this experiment | Complete engine qualification before rollout |
| Fleet upgrade, observation and volume reduction | Live fleet unchanged by this experiment | Pending engine acceptance | Preserve rollback and qualify smaller volumes |
| Partitioned all-org/free usage, config/engine versions and AI tokens | Existing usage work | Pending; no new reporting verification here | Close after engine acceptance |
| Filipe's STG free storage/sleep/wake simulation | Existing free database | Pending | Measure storage, idle suspension and wake latency |

From an isolated checkout of that frozen commit, run the reproduction with:

```bash
cargo +1.95.0 test --locked -p reddb-io-server --test checkpoint_auth_contention -- --nocapture
```

This is a **characterization of the remaining defect**. Its passing result
means the contention was reproduced, not that login availability is repaired.
The following correction replaces the blocked/503 assertions with an
affirmative availability regression and retains event-content/recovery checks.
