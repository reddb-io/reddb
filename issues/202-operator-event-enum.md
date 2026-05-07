# Logging: OperatorEvent enum + emit() module [AFK]

GitHub: reddb-io/reddb#202
Parent: #201

Additive new file `crates/reddb-server/src/telemetry/operator_event.rs`. Closed enum ~12 variants (replication broken / divergence / WAL fsync failed / disk space critical / auth bypass / admin capability granted / secret rotation failed / config changed / startup failed / shutdown forced / schema corruption / checkpoint failed). Public surface:

```rust
pub enum OperatorEvent { /* 12 variants */ }
impl OperatorEvent {
    pub fn emit(self, audit: &AuditLogger);  // sync, void, consume self
}
```

Sync emit: persist to AuditLogger first → `tracing::warn!(target: "reddb::operator")` breadcrumb → fallback `eprintln!` if audit fails. Each variant carries typed `AuditValue` fields (escape-safe).

## Acceptance Criteria

- [ ] New file exists with 12 variants + emit().
- [ ] Each variant uses typed AuditValue fields.
- [ ] emit() order: audit_log → tracing breadcrumb → eprintln fallback.
- [ ] Inline tests cover each variant's emit (captured-tracing + in-memory audit fixture).
- [ ] Adversarial corpus: CRLF/NUL/quote/non-UTF8 in fields → escape-safe.
- [ ] Module rustdoc explains operator/developer split.
- [ ] mod.rs registration deferred to wiring lane.
