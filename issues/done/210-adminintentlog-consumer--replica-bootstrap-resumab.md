# null: AdminIntentLog consumer: replica bootstrap resumability

## Parent

#207

## What to build

Replica bootstrap (`runtime/impl_core.rs` ou `replication/replica.rs`) usa `AdminIntentLog` pra resumability:

1. Bootstrap startup: chama `admin_intent_log.scan_and_report(&audit_logger)`. Filter intents por `args.replica_id == self.node_id` (single-resumer policy). Se há intent não-completo `op=ReplicaBootstrap` matchando este replica:
   a. Resume: lê `progress.last_applied_lsn`, retoma catchup desde lá
2. Bootstrap kickoff: `let handle = log.begin(IntentOp::ReplicaBootstrap, args)?` antes de iniciar snapshot fetch. `args` inclui `replica_id`, `source_lsn`, `target_lsn_hint`
3. Catchup loop: `handle.checkpoint({last_applied_lsn, batches_applied})` cada N seconds ou cada batch
4. Bootstrap success: `handle.complete(summary)` no final com `{total_records, duration_ms}`
5. Bootstrap failure: handle dropa naturalmente → phase=aborted persisted

## Acceptance criteria

- [x] Bootstrap resume from last checkpoint after simulated crash mid-catchup (kill process at 50% → restart → completa do checkpoint)
- [x] Bootstrap from-scratch quando não há unfinished intent pra este replica_id
- [x] Multi-replica cluster: cada node vê só seus próprios intents (isolation por replica_id)
- [x] Integration test: kill bootstrap mid-catchup → restart → success
- [x] OperatorEvent::DanglingAdminIntent emitido on boot se intent encontrado
- [x] Bootstrap success calls `handle.complete`; failure → drop → phase=aborted on disk

## Blocked by

- #208

## Implementation

Files changed:

- `crates/reddb-server/src/telemetry/admin_intent_log.rs`:
  - `UnfinishedIntent` gains `args: Map<String,JsonValue>` and
    `last_progress: Option<Map<String,JsonValue>>`.
  - `scan_intents_internal` uses a local `ScanEntry` struct; parses `args`
    from running records and updates `last_progress` on checkpoint records.

- `crates/reddb-server/src/replication/replica.rs`:
  - `ResumePoint { last_applied_lsn }` — returned by `scan_for_resume`.
  - `ReplicaBootstrapper::new(node_id)` + `scan_for_resume(log)` (single-resumer
    policy, calls `scan_and_report` first) + `begin(log, source_lsn, target_lsn_hint)`.
  - `BootstrapHandle<'a>`: `checkpoint(lsn, batches)` / `complete(records, ms)` /
    Drop → aborted.
  - 6 tests covering all ACs: from-scratch, crash-resume, multi-replica isolation,
    drop-aborts, success-completes, no-resume-without-progress.
