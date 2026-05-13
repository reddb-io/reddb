# Cluster: ASK on replica + audit forward primary-sync [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/410

Labels: enhancement, ready-for-agent

GitHub issue number: #410

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

ASK on read replicas: retrieval reads local snapshot; LLM call from the local node; audit + cost forwarded synchronously to primary before answer returns.

If primary unreachable, replica returns 503 — no audit gap permitted. Cache populate is local + async-propagate.

Reuses the existing replication module's primary-sync RPC mechanism.

## Acceptance criteria

- [ ] ASK accepts on read replicas.
- [ ] Retrieval served from local snapshot (no primary roundtrip for read path).
- [ ] Audit row write forwarded to primary synchronously; answer waits for ACK.
- [ ] Cost-counter increment forwarded synchronously to primary.
- [ ] Primary unreachable → ASK on replica returns 503; no audit gap.
- [ ] Cache populate is async-local + propagate.
- [ ] Integration test in a 1-primary + 2-replica cluster harness.

## Blocked by

- #402

## Progress note (2026-05-13)

Rechecked after #402 and #403 closed. The ASK audit writer and answer
cache now exist, so the original listed blocker is no longer the
limiting factor.

The remaining blocker is architectural: current replication gRPC exposes
WAL pull/ack (`PullWalRecords`, `AckReplicaLsn`) but no primary-sync RPC
for a replica to durably submit non-WAL side effects such as `red_ask_audit`
rows and daily cost counter increments before returning an ASK answer.
`execute_ask` is synchronous and is also called from async gRPC handlers,
so adding this safely requires a defined primary-sync command/RPC contract
and async/sync boundary rather than a one-off transport call hidden inside
the ASK path.

Leaving runtime code unchanged and keeping this issue blocked until the
primary-sync RPC contract/harness exists. Acceptance items requiring a
1-primary + 2-replica harness remain unverified.

## Progress note (2026-05-13, primary-sync slice)

Added the first primary-sync contract slice:

- `SubmitAskSideEffects` gRPC endpoint on primaries.
- Replica ASK cost accounting forwards `ask.side_effects.v1` usage to the
  primary synchronously.
- Replica ASK audit rows forward to the primary synchronously before the
  answer returns.
- Primary-sync unavailability maps to HTTP 503 / gRPC unavailable.
- gRPC `Ask` runs the synchronous runtime path from a blocking task so the
  forwarding call does not block the async reactor.

Verified with:

- `cargo check --locked -p reddb-io-server`
- `cargo test --locked -p reddb-io-server primary_ask_side_effects`
- `cargo test --locked -p reddb-io-server ask_audit_retention_purge_deletes_rows_older_than_setting`

Still not done:

- Full 1-primary + 2-replica integration harness coverage.
- Cache async-propagate behavior is still unverified/not implemented here.
- The issue should remain open until the remaining acceptance criteria are
  covered.

Moved back to the active issue queue because the primary-sync RPC contract
now exists; the remaining work is implementation/test coverage rather than
an architectural blocker.

## Progress note (2026-05-13, cluster/cache coverage slice)

Added follow-up coverage and cache correctness work:

- Added `ask.cache_put.v1` as a best-effort cache-warm command over the
  existing ASK side-effects RPC.
- Replica ASK cache writes remain local and now asynchronously submit a cache
  warm payload to the primary; failures are ignored so audit/cost correctness
  still owns the blocking path.
- Primary can apply propagated ASK cache payloads into its BlobCache-backed
  ASK cache.
- Replica logical WAL apply now invalidates the local result/ASK caches for
  the changed collection, so cached ASK answers are not reused after source
  data catches up from the primary.
- Added an ignored `full` external-env test shape for 1-primary + 2-replica
  ASK audit/cost forwarding.
- Added an HTTP error-map assertion for `ask_primary_sync_unavailable` -> 503.

Verified with:

- `cargo check --locked -p reddb-io-server`
- `cargo test --locked -p reddb-io-server ask_cache`
- `cargo test --locked -p reddb-io-server table_cache_invalidation_clears_ask_answer_cache`
- `cargo test --locked -p reddb-io-server primary_ask_side_effects_payload_records_cost_and_audit`
- `cargo test --locked -p reddb-io-server map_runtime_error_covers_each_variant`
- `cargo test --locked --test integration_external_env --no-run`

Still not done:

- The ignored full-cluster ASK test has not been run against Docker with AI
  provider env injected into the stack.
- Primary-unreachable-on-replica-ASK is covered by mapping/unit behavior, but
  not yet by a Docker test that pauses/stops the primary and asserts the
  client-visible 503.

## Progress note (2026-05-13, full Docker closure)

Closed the remaining full-cluster verification gap:

- Added a deterministic `mock-ai` service to the full Docker compose profile
  and wired primary plus both replicas to it with OpenAI-compatible env vars.
- Made the mock chat completion usage configurable so the shared daily cost cap
  test can prove that cost accounting is forwarded synchronously to primary.
- Promoted the 1-primary + 2-replica ASK audit/cost test so it runs whenever
  `REDDB_TEST_PROFILE=full`.
- Added a gated destructive Docker test for replica ASK when the primary is
  stopped; it requires a fresh full profile and
  `REDDB_TEST_ASK_PRIMARY_DOWN_ENABLED=1`.
- Seeded ASK test tables with unique names via SQL so full-profile tests do not
  pass because of persisted stale rows.

Verified with:

- `rustfmt tests/integration_external_env.rs`
- `python3 -m py_compile tests/pgwire_clients/mock_ai.py`
- `docker compose -f testdata/compose/full.yml config`
- `cargo test --locked --test integration_external_env --no-run`
- `REDDB_TEST_PROFILE=full ... cargo test --locked --test integration_external_env external_ask_on_two_replicas_forwards_audit_and_cost_to_primary -- --ignored --nocapture`
- fresh full stack:
  `REDDB_TEST_PROFILE=full REDDB_TEST_ASK_PRIMARY_DOWN_ENABLED=1 ... cargo test --locked --test integration_external_env external_ask_on_replica_primary_down_returns_503 -- --ignored --nocapture`

All acceptance criteria for #410 are now covered by runtime/unit tests plus the
full Docker harness. Ready to move to `issues/done/` and close GitHub #410.
