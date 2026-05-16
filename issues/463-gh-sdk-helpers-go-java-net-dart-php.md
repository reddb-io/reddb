---
status: open
tag: AFK
gh: 463
---

# [AFK] gh-463: Implement rich SDK helpers for Go, Java, .NET, Dart, and PHP

GitHub: reddb-io/reddb#463

## What to build

Each driver (Go, Java, .NET, Dart, PHP) exposes + tests SDK Helper Spec helpers or equivalent adapters. Conformance tests cover query, documents, KV, queue/transaction helpers where supported. Driver README matches actual helper surface. Unsupported transport-specific behavior fails with clear typed errors.

## Acceptance criteria

- [x] Go driver: helpers + conformance tests
- [ ] Java driver: helpers + conformance tests
- [ ] .NET driver: helpers + conformance tests
- [ ] Dart driver: helpers + conformance tests
- [ ] PHP driver: helpers + conformance tests
- [x] Go README matches actual helper surface (other READMEs still pending)
- [x] Unsupported behavior fails with clear typed errors (Go)

## Notes
- Drivers live under `drivers/<lang>/`.
- Spec lives in `crates/reddb-server/src/...` or `docs/sdk-helper-spec.md`.
- This is **big**. If too large for one iter, land 1-2 languages with full helpers/tests and leave a clear progress note for the rest. Don't half-do all 5.
- Commit `Refs #463` (partial) or `Closes #463` (only if all 5 done).

## Progress 2026-05-16 — Go landed

- `drivers/go/helpers.go` adds `reddb.NewHelpers(Querier)` → `Documents`,
  `KV`, `Queue` namespaces mirroring the SDK Helper Spec v0.1
  (`docs/clients/sdk-helper-spec.md`).
- Envelopes: `InsertResult`, `DeleteResult`, `ExistsResult`, `ListResult`,
  `QueuePushResult`. New typed codes: `CodeInvalidArgument`,
  `CodeInvalidResponse`.
- Conformance covered by `drivers/go/helpers_test.go` against a `fakeQuerier`
  — KV exact-key round-trip, prefix filter, value literal escaping;
  Queue push/pop/peek/len with priority + identifier validation; Documents
  insert/get/patch/list NOT_FOUND + INVALID_ARGUMENT cases; nested
  `result.affected` envelope handling.
- README extended with helper examples.
- Transactions: deferred — Go `Conn` has no transaction helper yet; README
  notes the gap and points at `Conn.Exec(BEGIN/COMMIT/ROLLBACK)`.
- Probabilistic helpers: not wired (server-side surface is still SQL only;
  parity with JS/Python helpers can land as a follow-up).

## Remaining for #463

- **Java** (`drivers/java/`): mirror `NewHelpers` as `Reddb.helpers()` with
  `DocumentsClient`, `KvClient`, `QueueClient`. Reuse existing HTTP client.
  Tests via JUnit fake transport.
- **.NET** (`drivers/dotnet/`): expose `Reddb.Helpers` extension with the
  same envelopes (xUnit + a fake `IRedTransport`).
- **Dart** (`drivers/dart/`): add `documents`, `kv`, `queue` on `Reddb`;
  tests via `package:test` with a mock transport.
- **PHP** (`drivers/php/`): add `Documents`, `Kv`, `Queue` namespace classes
  on `Reddb`; PHPUnit tests with a mock client.
- For each: README helper table + typed error codes (`INVALID_ARGUMENT`,
  `NOT_FOUND`, `INVALID_RESPONSE`) matching Go/Python wording.
- The Go helpers are pure SQL string builders + envelope normalisation —
  each remaining driver can port `helpers.go` verbatim, swapping SQL
  literal escaping rules where needed.

## Progress 2026-05-16 (cont.) — Dart landed (unverified locally)

- `drivers/dart/lib/src/helpers.dart`: `Helpers(Querier)` exposing
  `documents` / `kv` / `queue` namespaces, ported from `helpers.go`. Same
  envelopes (`InsertResult`, `DeleteResult`, `ExistsResult`, `ListResult`,
  `QueuePushResult`) and typed error classes (`InvalidArgument`,
  `InvalidResponse`, `NotFound` added to `drivers/dart/lib/src/errors.dart`).
- `Reddb` now `implements Querier` and exposes a `helpers` getter.
- `drivers/dart/lib/reddb.dart`: helpers + envelopes + `kvPath` exported.
- `drivers/dart/test/helpers_test.dart`: conformance against an
  in-memory `_FakeQuerier` mirroring the Go tests — KV path quoting,
  exact-key round-trip + escape semantics, prefix filter; queue push
  priority + payload, len, pop count, invalid-identifier guard;
  documents insert envelope, NotFound on missing, JSON-pointer
  rejection, default ordering, "already exists" passthrough; nested
  `result.affected` envelope handling.
- README documents the helper surface + transaction gap.
- ⚠️ **Not run locally**: no `dart` toolchain on this sandbox. The port
  is a mechanical translation of the Go helpers (which pass their own
  conformance) — run `dart pub get && dart analyze && dart test`
  before merging.
- ⚠️ **Commit blocked**: every `git` invocation in this iteration
  needed approval that wasn't granted, so the changes sit in the
  working tree for the next iteration to commit. Files to stage:
  `drivers/dart/lib/src/helpers.dart`,
  `drivers/dart/lib/src/errors.dart`,
  `drivers/dart/lib/src/reddb_base.dart`,
  `drivers/dart/lib/reddb.dart`,
  `drivers/dart/test/helpers_test.dart`,
  `drivers/dart/README.md`.

## Remaining for #463 (updated)

- **Java** (`drivers/java/`): mirror as `Reddb.helpers()`.
- **.NET** (`drivers/dotnet/`): `Reddb.Helpers` extension + xUnit fakes.
- **PHP** (`drivers/php/`): `Documents` / `Kv` / `Queue` namespace
  classes + PHPUnit mocks.
- For each: README helper table + typed error codes.
