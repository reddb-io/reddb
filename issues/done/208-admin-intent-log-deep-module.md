# 208: AdminIntentLog: deep module + OperatorEvent::DanglingAdminIntent variant

## Parent

#207

## What to build

`AdminIntentLog` deep module em `crates/reddb-server/src/telemetry/admin_intent_log.rs` + 13ª variant `DanglingAdminIntent` em `OperatorEvent`. Não wira nenhum consumer real — slice é puramente infra.

Inclui:
- `AdminIntentLog::open/begin/list_unfinished/scan_and_report`
- `IntentHandle::checkpoint/complete/id` + linear `complete(self)` + `Drop` auto-aborted
- JSON line schema: `{id, op, phase, ts, actor, args, progress?, summary?}`
- UUID v7 generation (use `uuid` crate v1 com feature `v7`)
- `O_APPEND` + NonBlocking writer (lossy=false) + 3KB record cap enforced em begin/checkpoint/complete
- fsync apenas em begin (begin failure propaga Err)
- `OperatorEvent::DanglingAdminIntent { id: Uuid, op: IntentOp, started_at: DateTime<Utc>, last_phase: IntentPhase }`
- `pub mod admin_intent_log;` em `crates/reddb-server/src/telemetry/mod.rs`
- Closed enum `IntentOp { ReplicaBootstrap }` — adicionar variants quando consumers novos chegarem
- `IntentArgs/Progress/Summary` wrappers sobre `serde_json::Map<String, Value>` com AuditFieldEscaper redaction

## Acceptance criteria

- [ ] `cargo check -p reddb-server` passes
- [ ] `cargo test -p reddb-server admin_intent_log` passes (testes cobrindo 7 cenários do PRD #207 Testing Decisions)
- [ ] OperatorEvent enum tem 13 variants, exhaustive match em todos call sites compila clean
- [ ] Multi-process test (spawn 2 children + parent) verifies POSIX atomicity sem corruption
- [ ] Record > 3KB returns `IntentLogError::TooLarge`, sem write
- [ ] Drop sem complete escreve phase=aborted ao file
- [ ] scan_and_report emite exatamente N DanglingAdminIntent events pra N unfinished intents
- [ ] Linha JSON corrompida não derruba scan, emite tracing::warn

## Blocked by

None — additive infra.
