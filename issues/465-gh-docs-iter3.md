---
status: open
tag: AFK
gh: 465
---

# [AFK] gh-465 iter 3: Driver README sweep across all 10 SDKs

GitHub: reddb-io/reddb#465

## Iter 1 + 2 (already on main)

- iter 1: README.md, hypertable/KV/document INSERT examples, limitations.md.
- iter 2: docs/query/graph-commands.md, docs/query/search-commands.md aligned with parser.

## Iter 3 — driver READMEs

Audit each driver README for examples/promises that don't match the current helper surface (helpers landed in #463 across 8 languages: Go, Dart, Java, .NET, PHP, C++, Kotlin, Zig; Rust/Python/JS pre-existed).

For each driver under `drivers/<lang>/README.md`:
1. Verify code examples compile/run against the helper surface that actually exists in that driver.
2. Remove or flag examples for unsupported behavior.
3. Confirm error-type names match what the helper actually throws.
4. Cross-check against `crates/reddb-client/README.md` for the Rust reference.

Drivers in scope:
- drivers/rust/  (or crates/reddb-client/)
- drivers/python/, drivers/python-asyncio/
- drivers/js/, drivers/js-client/
- drivers/go/
- drivers/dart/
- drivers/java/
- drivers/dotnet/
- drivers/php/
- drivers/cpp/
- drivers/kotlin/
- drivers/zig/

## Acceptance for iter 3

- [ ] Each driver README examples use only behaviors implemented by that driver's helper surface
- [ ] Typed error names match implementation
- [ ] Cross-references to other drivers updated where stale

## Notes

- Commit with `Refs #465` (contract matrix doc still remains).
- Skip the C++/Kotlin/Zig drivers if their READMEs were just updated by #463 iter 3 — those landed fresh.
- Be surgical. Only edit what's actually wrong against the helper surface.

## Iter 3 run (2026-05-16)

Audited every driver README against its helper surface + parser grammar:

- `drivers/go/README.md` — examples match `helpers.go` surface (NewHelpers, Documents/KV/Queue, InsertResult/DeleteResult/ExistsResult/ListResult/QueuePushResult, CodeInvalidArgument/CodeNotFound). No changes.
- `drivers/dart/README.md` — examples match `lib/src/helpers.dart`; typed errors `InvalidArgument`, `NotFound`, `InvalidResponse` exist in `lib/src/errors.dart`. No changes.
- `drivers/java/README.md` — examples match `helpers/{Helpers,DocumentClient,KvClient,QueueClient}.java`; nested option classes (`KvClient.KvListOptions`, `QueueClient.PushOptions`) and `HelperException.{InvalidArgument,NotFound,InvalidResponse}` line up. No changes.
- `drivers/dotnet/README.md` — examples match `src/Reddb/Helpers/Helpers.cs` (`KvClient.ListOpts`, `QueueClient.PushOptions`); error names `HelperException.{InvalidArgument,NotFound,InvalidResponse}` correct. No changes.
- `drivers/php/README.md` — examples match `src/Helpers/{Queue,Kv,Documents}.php`; typed exceptions `Reddb\Helpers\{InvalidArgument,NotFound,InvalidResponse}` correct. No changes.
- `drivers/js/README.md`, `drivers/js-client/README.md` — `documents/kv/queue` namespaces line up with `src/{documents,kv,queue}.js`; `RedDBError` codes match. No changes.
- `drivers/python/README.md` — surface (`documents`, `kv`, `db.query` variadic + `params=`) matches the pyo3 binding. No changes.
- `drivers/python-asyncio/README.md` — **fixed**: SEARCH SIMILAR example used `IN embeddings K 5`; parser (`search_commands.rs:43`) requires `COLLECTION col [LIMIT n]`. Rewrote example to `SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 5` — matches iter 2 fix to `docs/query/search-commands.md`.
- `drivers/{cpp,kotlin,zig}/README.md` — skipped per issue note (landed fresh in #463 iter 3).
- `crates/reddb-client/README.md` — already uses correct `SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2`. Reference shape for the other driver READMEs.

## Still open (separate slices)

- Contract matrix doc linking each public promise to a test — larger structural deliverable, not a docs-sweep slice.
- Test coverage audit for `GRAPH CLUSTERING / TOPOLOGICAL_SORT / PROPERTIES / PATH FROM` parser arms — already flagged in iter 2.

Acceptance for iter 3 (driver README sweep) is met: every README's example surface matches its helper implementation. Issue can move to `done/` once committed.

## Blocker

Bash `git` operations require approval in this harness; edits are uncommitted. To land:
  `git add drivers/python-asyncio/README.md issues/465-gh-docs-iter3.md`
  `git mv issues/465-gh-docs-iter3.md issues/done/465-gh-docs-iter3.md`
Suggested message: `docs: align python-asyncio SEARCH SIMILAR example with parser (refs #465)`

