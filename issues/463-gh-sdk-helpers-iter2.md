---
status: open
tag: AFK
gh: 463
---

# [AFK] gh-463 iter 2: Java + .NET + PHP rich SDK helpers

GitHub: reddb-io/reddb#463

## Iter 1 (landed on main, commit 96256466)

- Go (drivers/go/): full helpers + 125 tests passed.
- Dart (drivers/dart/): mechanical port, helpers + tests (CI runs Dart).

## Iter 2 — Java + .NET + PHP

Mirror the Go helper surface for each language. Driver locations:
- drivers/java/
- drivers/dotnet/
- drivers/php/

Same envelope shape (InsertResult, DeleteResult, ExistsResult, ListResult, QueuePushResult), same typed errors (InvalidArgument, InvalidResponse, NotFound), same namespaces (documents/kv/queue).

Reference: `drivers/go/helpers.go` + `drivers/go/helpers_test.go` for behavior.

## Acceptance for iter 2

- [x] Java: `Helpers` class with documents/kv/queue namespaces + JUnit tests against mock Querier
- [x] .NET: `Helpers` namespace + xUnit tests against mock Querier
- [x] PHP: `Documents` / `Kv` / `Queue` namespace classes + PHPUnit mocks
- [x] Each driver README updated with helper surface + typed error codes

## Progress 2026-05-16 — Java + .NET + PHP landed (unverified locally)

All three driver helpers are mechanical ports of `drivers/go/helpers.go`.
Same envelope shapes, same typed errors (`InvalidArgument`, `NotFound`,
`InvalidResponse`), same namespaces.

### Files

- Java: `drivers/java/src/main/java/dev/reddb/helpers/`
  - `Querier.java` — functional interface (`byte[] query(String, Object...)`)
  - `HelperException.java` — typed error tree
  - `Envelopes.java` — `InsertResult`, `DeleteResult`, `ExistsResult`,
    `ListResult`, `QueuePushResult` (records)
  - `Sql.java` — pure SQL builders + JSON envelope parser (Jackson)
  - `DocumentClient.java`, `KvClient.java`, `QueueClient.java`
  - `Helpers.java` — entry point with `Helpers.of(Conn)` adapter
  - Tests: `drivers/java/src/test/java/dev/reddb/helpers/HelpersTest.java`
    (mirrors `helpers_test.go` 1:1 via JUnit 5 + Jackson FakeQuerier).
- .NET: `drivers/dotnet/src/Reddb/Helpers/Helpers.cs` (single file: `IQuerier`,
  `HelperException`, all envelopes as records, `DocumentClient`,
  `KvClient`, `QueueClient`, `Sql` internal, `Helpers.For(IConn)` adapter).
  Tests: `drivers/dotnet/tests/Reddb.Tests/HelpersTests.cs`.
- PHP: `drivers/php/src/Helpers/` (PSR-4, one class per file): `Querier`,
  `HelperException` + `InvalidArgument` + `NotFound` + `InvalidResponse`,
  `InsertResult` + `DeleteResult` + `ExistsResult` + `ListResult` +
  `QueuePushResult`, `Sql`, `Documents`, `Kv`, `Queue`, `Helpers` (with
  `Helpers::for(Conn|Querier)`), `ConnQuerier` adapter.
  Tests: `drivers/php/tests/Helpers/HelpersTest.php`.
- READMEs: java/dotnet/php each gained a "Rich helpers (SDK Helper Spec v0.1)"
  section with examples + typed error list.

### ⚠️ Unverified locally

The sandbox only has `java` (no `gradle`/`mvn`/`dotnet`/`php`/`phpunit`).
Each port is a mechanical translation of the Go helpers, which pass their
conformance tests. Before merging, run:

- Java: `cd drivers/java && ./gradlew test`
- .NET: `cd drivers/dotnet && dotnet test`
- PHP:  `cd drivers/php && composer install && vendor/bin/phpunit`

Likely tweaks needed:

- Java: Jackson default may render `Integer 1` differently in some test
  cases — if `{"a":1}` literal check fails, adjust `Sql.kvValueLiteral`
  number formatting.
- .NET: `Helpers.For(IConn)` uses ValueTask wrappers; if compile fails on
  `IConn.QueryAsync` overload disambiguation, fall back to constructing
  `ConnQuerier` directly.
- PHP: PSR-4 autoload requires `composer dump-autoload` to discover the
  new `src/Helpers/` namespace.

### Transactions

Still deferred for all three drivers — none expose a transaction helper.
READMEs note the gap and point at raw `query("BEGIN/COMMIT/ROLLBACK")`.

### ⚠️ Commit blocked

`git add` / `git commit` calls in this iteration needed approval that
wasn't granted, so the changes sit in the working tree for the next
iteration to commit. Same blocker pattern as iter 1's Dart port.

Files to stage:

- `drivers/java/src/main/java/dev/reddb/helpers/{Querier,HelperException,
  Envelopes,Sql,DocumentClient,KvClient,QueueClient,Helpers}.java`
- `drivers/java/src/test/java/dev/reddb/helpers/HelpersTest.java`
- `drivers/java/README.md`
- `drivers/dotnet/src/Reddb/Helpers/Helpers.cs`
- `drivers/dotnet/tests/Reddb.Tests/HelpersTests.cs`
- `drivers/dotnet/README.md`
- `drivers/php/src/Helpers/{Querier,HelperException,InvalidArgument,
  NotFound,InvalidResponse,InsertResult,DeleteResult,ExistsResult,
  ListResult,QueuePushResult,Sql,Documents,Kv,Queue,Helpers,
  ConnQuerier}.php`
- `drivers/php/tests/Helpers/HelpersTest.php`
- `drivers/php/README.md`
- This issue file → `issues/done/` once committed AND a CI/local
  toolchain run confirms the three test suites pass.

## Out of scope

- C++ / Kotlin / Zig drivers (separate iter).

## Notes

- Commit with `Refs #463`.
- `CARGO_TARGET_DIR=.target-gh463-iter2` (not needed for non-Rust drivers).
- If any toolchain (mvn / dotnet / php) is missing locally, port mechanically and document the gap.
- Read drivers/go/helpers.go before porting — surface mismatch defeats the purpose.
