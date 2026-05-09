# Events: backpressure + DLQ + outbox metrics (Prometheus) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/299

Labels: needs-triage

GitHub issue number: #299

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Robustez do outbox: retry com backoff, DLQ após N retries, métricas Prometheus.

End-to-end:
- Drain worker tenta push N vezes (default 5) com exponential backoff.
- Após N falhas → evento vai pra `<queue>_outbox_dlq` (auto-criada).
- Métricas Prometheus:
  - `reddb_events_outbox_size_bytes` — tamanho atual do outbox.
  - `reddb_events_outbox_lag_ms` — diff entre LSN do mais antigo entry e LSN atual.
  - `reddb_events_drain_rate_eps` — events/sec drenados.
  - `reddb_events_drain_retries_total{reason}` — counter de retries.
  - `reddb_events_dlq_total{queue}` — counter de eventos em DLQ.
- Warning em `outbox_warn_bytes` (default 1 GiB), max em `outbox_max_bytes` (default 10 GiB).
- Audit log + OperatorEvent quando DLQ ativa.

## Acceptance criteria

- [ ] Queue cheia + retry exhausted → evento em `<queue>_outbox_dlq`.
- [ ] Métricas Prometheus expostas em `/metrics`.
- [ ] Outbox > warn_bytes → log warning + OperatorEvent.
- [ ] Outbox > max_bytes → eventos novos vão direto pra DLQ.
- [ ] Conformance: 3 casos (queue full, drain retry, DLQ).

## Blocked by

- #292
