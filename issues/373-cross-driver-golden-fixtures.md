# Cross-driver wire conformance via shared golden fixtures [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/373

Labels: enhancement

GitHub issue number: #373

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

A shared golden-fixture test suite that pins the wire-level encoding of every `Value` variant. Every official driver's parameter serializer is verified against these fixtures so they all produce byte-identical output.

This is the contract that keeps the 12 drivers in sync over time. Without it, drivers will silently drift.

Format: a directory of `.bin` (or hex) golden files plus a manifest describing the input value (in a language-neutral form like JSON) and the expected wire bytes for both the RedWire frame and the gRPC proto.

## Acceptance criteria

- [ ] Golden-fixture directory under `crates/reddb-wire/tests/fixtures/params/` (or similar).
- [ ] Fixtures cover every Value variant including boundary cases (i64::MIN/MAX, NaN, ±inf, empty/large vectors, large bytes, unicode text).
- [ ] Each driver has a conformance test that loads the manifest and asserts byte-identical output.
- [ ] CI runs the conformance test for every driver.
- [ ] Documented process for adding new fixtures when extending the Value enum.

## Blocked by

- #356
- #357
