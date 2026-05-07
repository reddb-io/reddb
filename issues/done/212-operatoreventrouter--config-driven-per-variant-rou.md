# null: OperatorEventRouter: config-driven per-variant routing (HITL design)

## What to build

Hoje `OperatorEvent::emit(audit_logger)` faz dispatch fixo: persiste em audit_log + emite tracing breadcrumb. Toda variante segue mesma rota.

Esta slice introduz `OperatorEventRouter` que mapeia variant → `Vec<Handler>` configurável. Handlers possíveis:
- `AuditLog` (foundational, always available)
- `Tracing { target, level }` (foundational, always available)
- `Stderr` (fallback)
- `WebhookPagerDuty { url, auth_env, rate_limit }` (paging severity)
- `WebhookGeneric { url, auth_env, rate_limit, body_template }` (custom)

## Design decisions (HITL grilled, locked in)

### 1. Webhook auth: bearer token via env-var ref

```toml
[telemetry.operator_event.routes.pagerduty]
url = "https://events.pagerduty.com/v2/enqueue"
auth_env = "PAGERDUTY_INTEGRATION_KEY"  # token lido do env var em runtime
```

- Bearer é universal (PD, Slack, Discord, generic webhooks)
- Env-var ref evita secret no config file (12-factor)
- Tainted<T>::escape_for(boundary) protege contra leak (PRD #173)
- Boot fail-fast se env var não setada
- Signed body (HMAC) e mTLS = future Handler variants se signal real

### 2. Rate limit: per-handler token bucket

```toml
[telemetry.operator_event.routes.pagerduty]
url = "..."
auth_env = "PAGERDUTY_INTEGRATION_KEY"
rate_limit = { requests = 60, window_sec = 60 }  # 1/sec sustained

[telemetry.operator_event.routes.audit_log]
# no rate_limit field → no limit. Default for local handlers.
```

- Limit é propriedade do *handler* (PagerDuty pipe), não do *event variant*
- Saturação: drop event nesse handler, outros handlers no route continuam disparando
- Metrics: `operator_event_dropped{handler, reason="rate_limit"}` Prometheus-style
- Token bucket inline em router. Quando N=2 com `runtime/quota_bucket.rs`, extrair `util/token_bucket.rs`

### 3. Webhook failure mode: bounded async queue + 3 retries + drop oldest

```
caller.emit() → push to MPSC<OperatorEvent> bounded(1000) per-handler [returns]

worker thread per handler:
  for attempt in 1..=3 {
    match webhook.post(event).await {
      Ok => break;
      Err(retryable) if attempt < 3 => sleep(2^attempt * 100ms);
      Err(_) => { metric::drop("max_retries"); break; }
    }
  }

on channel saturation: drop OLDEST + metric::drop("queue_full")
```

- Block é vetado: emit() é sync por contrato (#202), 12 hot-path call sites (#205)
- Persistent queue over-engineering: source of truth é audit_log (local fsync), webhook é notification echo
- Sem ordering guarantee (PD/Slack deduplicate por event_key + ts)
- Process restart loses in-flight queue (acceptable, audit_log captura tudo)
- Retry=3 fixo, sem config knob (mais retries = staleness, fix receiver não tunar retries)

### 4. Config schema: STRICT validation no boot, levenshtein suggestion

```rust
fn validate(config: &OperatorEventRoutes) -> Result<(), ConfigError> {
  let known: HashSet<&str> = OperatorEvent::all_variant_names().into_iter().collect();
  for route_key in config.routes.keys() {
    if route_key != "default" && !known.contains(route_key.as_str()) {
      return Err(ConfigError::UnknownVariant {
        key: route_key.clone(),
        line: route_key.line,
        suggestion: closest_match(route_key, &known),  // strsim levenshtein
      });
    }
  }
}
```

Erro example:
```
ERROR: unknown OperatorEvent variant 'AuthByPassAttempt' at config.toml:42
       did you mean: AuthBypassAttempt?
```

- Silent typo = alarme nunca dispara (worst possible failure mode)
- Closed enum compile-time vale só se config-time match
- Handler names também strict (closed set: audit_log, tracing, stderr, pagerduty, generic_webhook)
- `default` é palavra reservada, tratada explicitamente
- `strsim` crate (1 dep, ~5 linhas, operator-grade UX)

### 5. Default routing: implicit code default, zero upgrade burden

Resolution order:
1. `[...routes.<variant>]` block? Use it.
2. `[...routes.default]` block? Use for variants sem específico.
3. Senão? Code default = `["audit_log", "tracing"]` (preserves current #202 + #205 behavior).

Empty user config = identical to today. Operator opt-in pra webhooks adiciona apenas:
```toml
[telemetry.operator_event.routes.pagerduty]
url = "..."
auth_env = "..."
rate_limit = { requests = 60, window_sec = 60 }

[telemetry.operator_event.routes.AuthBypassAttempt]
handlers = ["audit_log", "tracing", "pagerduty"]
```

- Não shippa explicit defaults pra config file (would be 13 blocks of noise)
- Não enforce "audit_log mandatory in default" — operator com custom pipeline pode opt out, documentar implication
- Handler names lowercase em config, variant names CamelCase (visual disambig)

### Validation pipeline order

1. Parse TOML → struct
2. Strict variant name validation (Q4)
3. Strict handler name validation (closed set)
4. Resolve effective routing (code default ← user default ← per-variant)
5. Webhook auth env var presence check
6. Any failure → boot fail-fast com erro claro

## Acceptance criteria

- [ ] `OperatorEventRouter` deep module em `crates/reddb-server/src/telemetry/operator_event_router.rs`
- [ ] TOML config schema documented em `docs/operations/logging.md`
- [ ] All 13 OperatorEvent variants compatíveis com router (no regression em emit() — empty config = current behavior)
- [ ] AuditLog + Tracing handlers (foundational), Stderr (fallback), WebhookPagerDuty + WebhookGeneric (paging)
- [ ] Bearer auth via env-var ref, fail-fast se var ausente
- [ ] Per-handler token bucket rate limit
- [ ] Bounded async queue (1000 slots) per webhook handler, 3 retries exp backoff, drop oldest on saturation
- [ ] Strict variant + handler name validation no boot, levenshtein suggestion via strsim
- [ ] Implicit code default `["audit_log", "tracing"]` quando config silente — zero burden upgraders
- [ ] Prometheus-style metrics: operator_event_dropped{handler, reason}, operator_event_sent{handler, attempts}
- [ ] Tests: unit per handler + integration with mock webhook server + race-condition test for rate limit
- [ ] Default routing test: empty config → all 13 variants emit to audit_log + tracing exactly como #205

## Blocked by

None — design locked in via grill on 2026-05-07. Can dispatch AFK impl immediately.
