# Logging: SlowQueryLogger module + dedicated sink red-slow.log [AFK]

GitHub: reddb-io/reddb#203
Parent: #201

Additive new file `crates/reddb-server/src/telemetry/slow_query_logger.rs`. Owns `NonBlocking` writer for `red-slow.log`. Public surface:

```rust
pub struct SlowQueryLogger { /* sink + threshold + sample atomics */ }
impl SlowQueryLogger {
    pub fn new(opts: SlowQueryOpts) -> Arc<Self>;
    pub fn record(&self, kind: QueryKind, duration_ms: u64, sql_redacted: String, scope: &EffectiveScope);
}
pub struct SlowQueryOpts { pub log_dir: PathBuf, pub threshold_ms: u64, pub sample_pct: u8 }
pub enum QueryKind { Select, Insert, Update, Delete, Bulk, Aggregate, DDL, Internal }
```

Below threshold: atomic compare drop, zero hot-path overhead. Above + sampled: structured JSON line.

## Acceptance Criteria

- [ ] New file exists.
- [ ] Below-threshold: 10000 calls < 10ms total wall time, zero file writes.
- [ ] Above-threshold: structured JSON line, fields parseable.
- [ ] Sampling property test: sample_pct=10 over 10000 calls produces ~10% emissions (±2pp).
- [ ] Adversarial test: CRLF/NUL/quote in pre-redacted SQL → escape-safe.
- [ ] QueryKind closed enum.
- [ ] No call site wired in this slice.
- [ ] Flag for orchestrator: mod.rs reg + statement_frame.rs call site deferred.
