---
status: open
tag: AFK
gh: 463
---

# [AFK] gh-463 iter 3: C++ + Kotlin + Zig rich SDK helpers

GitHub: reddb-io/reddb#463

## Iter 1 + 2 already landed (on main)

- Go (drivers/go/helpers.go + helpers_test.go) — 125 Go tests passing.
- Dart (drivers/dart/lib/src/helpers.dart + test/helpers_test.dart) — mechanical port.
- Java (drivers/java/.../helpers/...) — JUnit tests, mechanical port.
- .NET (drivers/dotnet/.../Helpers/...) — xUnit tests, mechanical port.
- PHP (drivers/php/src/Helpers/...) — PHPUnit tests, mechanical port.

## Iter 3 — C++ + Kotlin + Zig

Mirror the Go helper surface (documents/kv/queue namespaces, same envelopes, same typed errors). Driver locations:
- drivers/cpp/
- drivers/kotlin/
- drivers/zig/

Reference: `drivers/go/helpers.go` + `drivers/go/helpers_test.go` for behavior.

## Acceptance for iter 3

- [x] C++: helpers header + impl + GoogleTest cases against mock Querier
- [x] Kotlin: helpers package + Kotest/JUnit tests against mock Querier
- [x] Zig: helpers module + std.testing cases against mock Querier
- [x] Each driver README updated with helper surface + typed error codes

## Progress 2026-05-16 — C++ + Kotlin + Zig landed (unverified locally)

Mechanical port from `drivers/go/helpers.go`. Same envelopes
(`InsertResult`/`DeleteResult`/`ExistsResult`/`ListResult`/`QueuePushResult`),
same typed errors (`InvalidArgument`/`NotFound`/`InvalidResponse`),
same `documents`/`kv`/`queue` namespaces.

### Files

- **Kotlin** (uses existing Jackson dep — no new deps):
  - `drivers/kotlin/src/main/kotlin/dev/reddb/helpers/Helpers.kt`
  - `drivers/kotlin/src/test/kotlin/dev/reddb/helpers/HelpersTest.kt`
  - `drivers/kotlin/README.md` — "Rich helpers" section added.
- **C++** (header + impl, minimal recursive-descent JSON parser to
  avoid new deps; tests via GoogleTest):
  - `drivers/cpp/include/reddb/helpers.hpp`
  - `drivers/cpp/src/helpers.cpp`
  - `drivers/cpp/tests/helpers_test.cpp`
  - `drivers/cpp/CMakeLists.txt` — registered the new src + test.
  - `drivers/cpp/README.md` — "Rich helpers" section added.
- **Zig** (uses std.json, fn-pointer Querier vtable; clones parsed
  rows so callers can keep them after `Parsed(...).deinit()`):
  - `drivers/zig/src/helpers.zig`
  - `drivers/zig/tests/helpers_test.zig`
  - `drivers/zig/src/reddb.zig` — `pub const helpers` exported.
  - `drivers/zig/build.zig` — `tests/helpers_test.zig` added.
  - `drivers/zig/README.md` — "Rich helpers" section added.

### ⚠️ Unverified locally

Sandbox lacks `cmake`/`g++`, `gradle`/`kotlinc`, and `zig`. Before
merging, run:

- C++:    `cmake -S drivers/cpp -B drivers/cpp/build && cmake --build drivers/cpp/build && ctest --test-dir drivers/cpp/build`
- Kotlin: `cd drivers/kotlin && ./gradlew test`
- Zig:    `cd drivers/zig && zig build test`

Likely tweaks if compilation fails:

- **C++**: minimal JSON parser rejects `\uXXXX` surrogate pairs; if
  an envelope ever ships them, extend `parse_string`.
- **Kotlin**: `Helpers.of(conn)` stringifies params through
  `conn.query(sql, *params)`. If the transport rejects `Any?` arrays,
  callers should adapt their own `Querier` instead.
- **Zig**: targets std.json `Value` as of 0.13 / 0.14
  (`.integer`/`.float`/`.number_string`/`.string`/`.array`/`.object`/
  `.bool`/`.null`). On older Zig swap `splitScalar` / `parseFromSlice`
  for the older `tokenize` / `parse`.

### Transactions

Still deferred for all three drivers (no transaction helper exposed).
READMEs note the gap and point at raw
`query("BEGIN/COMMIT/ROLLBACK")`.

### ⚠️ Commit blocked

`git status` / `git add` / `git commit` calls in this iteration
needed approval that wasn't granted — same blocker as iter 1 (Dart)
and iter 2 (Java + .NET + PHP). Files sit in the working tree.

Files to stage:

- `drivers/kotlin/src/main/kotlin/dev/reddb/helpers/Helpers.kt`
- `drivers/kotlin/src/test/kotlin/dev/reddb/helpers/HelpersTest.kt`
- `drivers/kotlin/README.md`
- `drivers/cpp/include/reddb/helpers.hpp`
- `drivers/cpp/src/helpers.cpp`
- `drivers/cpp/tests/helpers_test.cpp`
- `drivers/cpp/CMakeLists.txt`
- `drivers/cpp/README.md`
- `drivers/zig/src/helpers.zig`
- `drivers/zig/src/reddb.zig`
- `drivers/zig/tests/helpers_test.zig`
- `drivers/zig/build.zig`
- `drivers/zig/README.md`
- This issue file → `issues/done/` once committed AND CI confirms
  three test suites pass.

This is the 8th/last language (Go, Dart, Java, .NET, PHP, C++, Kotlin,
Zig). Recommended commit footer: `Closes #463`.

## Notes

- Commit with `Closes #463` if this finishes all 8 languages, else `Refs #463`.
- `CARGO_TARGET_DIR=.target-gh463-iter3` (not needed for non-Rust drivers).
- Local toolchains likely missing (no cmake / gradle / zig in sandbox); port mechanically per iter 2 pattern and document the gap. CI runs each.
- Read drivers/go/helpers.go before porting.
